//! `botler backup` on a freshly initialised data directory.

use std::path::Path;
use std::process::Command;

fn botler(data: &Path, args: &[&str]) -> std::process::Output {
    // The hub's runtime directory (its socket) goes next to the data directory, not into the
    // user's real one.
    let runtime = data.parent().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_botler"))
        .arg("--data")
        .arg(data)
        .args(args)
        .env("XDG_RUNTIME_DIR", runtime)
        .output()
        .unwrap();
    assert!(out.status.success(), "botler {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out
}

fn entries(archive: &Path) -> Vec<String> {
    let out = Command::new("tar").arg("-tf").arg(archive).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(|l| l.trim_end_matches('/').to_string()).collect()
}

#[test]
fn backup_archives_facts_not_derived_state() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    botler(&data, &["init"]);

    let archive = dir.path().join("b.tar");
    let out = botler(&data, &["backup", archive.to_str().unwrap()]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("decrypt"));
    let list = entries(&archive);
    for want in ["hub.db", "secrets.key", "repo.git", "repo.git/HEAD", "sessions", "workspaces", "homes", "uploads"] {
        assert!(list.iter().any(|e| e == want), "{want} missing from {list:?}");
    }
    assert!(data.join("search.db").exists(), "the hub creates its search index");
    for derived in ["exports", "worktrees", "logs", "agent-dirs", "assets", "search.db"] {
        assert!(!list.iter().any(|e| e.starts_with(derived)), "{derived} archived: {list:?}");
    }
    assert!(!dir.path().join("b.staging").exists());
    // Never overwrites.
    let again =
        Command::new(env!("CARGO_BIN_EXE_botler")).arg("--data").arg(&data).arg("backup").arg(&archive).output();
    assert!(!again.unwrap().status.success());

    let archive = dir.path().join("nokey.tar");
    botler(&data, &["backup", "--no-key", archive.to_str().unwrap()]);
    let list = entries(&archive);
    assert!(list.iter().any(|e| e == "hub.db"));
    assert!(!list.iter().any(|e| e == "secrets.key"), "{list:?}");
}

#[test]
fn backup_leaves_out_what_git_ignores_unless_full() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    botler(&data, &["init"]);
    let repo = data.join("homes/chat-1/proj");
    let write = |rel: &str, body: &str| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    };
    write(".gitignore", "node_modules/\n*.log\n!keep.log\n");
    for f in ["src/a.rs", "keep.log", "node_modules/x/i.js", "debug.log", "we ird [x].log"] {
        write(f, f);
    }
    let out =
        Command::new("git").args(["init", "-q"]).current_dir(&repo).env("GIT_CONFIG_GLOBAL", "/dev/null").output();
    assert!(out.unwrap().status.success());

    let archive = dir.path().join("b.tar");
    botler(&data, &["backup", archive.to_str().unwrap()]);
    let list = entries(&archive);
    let has = |f: &str| list.iter().any(|e| e == &format!("homes/chat-1/proj/{f}"));
    assert!(has("src/a.rs") && has("keep.log") && has(".gitignore"), "{list:?}");
    for f in ["node_modules", "debug.log", "we ird [x].log"] {
        assert!(!has(f), "{f} is ignored: {list:?}");
    }

    let archive = dir.path().join("full.tar");
    botler(&data, &["backup", "--full", archive.to_str().unwrap()]);
    let list = entries(&archive);
    let has = |f: &str| list.iter().any(|e| e == &format!("homes/chat-1/proj/{f}"));
    assert!(has("node_modules/x/i.js") && has("debug.log"), "{list:?}");
}
