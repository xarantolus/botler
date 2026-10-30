//! The web app's TypeScript types are generated from the Rust API types and committed in
//! `frontend/src/generated/`. This test fails when they are stale; regenerate with
//! `BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn read_tree(root: &Path) -> BTreeMap<PathBuf, String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, root, out);
            } else {
                out.insert(p.strip_prefix(root).unwrap().to_path_buf(), std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

#[test]
fn typescript_bindings_are_current() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/src/generated");
    let fresh = tempfile::tempdir().unwrap();
    botler_hub::bindings::generate(fresh.path()).unwrap();
    let want = read_tree(fresh.path());
    assert!(want.contains_key(Path::new("routes.ts")) && want.contains_key(Path::new("BotlerEvent.ts")));

    if std::env::var("BOTLER_UPDATE_BINDINGS").is_ok_and(|v| v == "1") {
        if committed.exists() {
            std::fs::remove_dir_all(&committed).unwrap();
        }
        for (rel, body) in &want {
            let p = committed.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        return;
    }

    let have = read_tree(&committed);
    let stale: Vec<String> = want
        .keys()
        .chain(have.keys())
        .filter(|k| want.get(*k) != have.get(*k))
        .map(|k| k.display().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(
        stale.is_empty(),
        "frontend/src/generated is stale ({}). Regenerate with \
         `BOTLER_UPDATE_BINDINGS=1 cargo test -p botler --test bindings`, then rebuild the frontend.",
        stale.join(", ")
    );
}
