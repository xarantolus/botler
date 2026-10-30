//! Disk space and archived chats: cleaning and packing homes, restoring them, auto-archive,
//! deleting for good, and restoring into a world that changed.

mod common;

use std::path::Path;
use std::process::Command;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller};
use botler_hub::{agents, restore, router, storage};
use botler_store::{InstanceState, NewSubscription};
use chrono::{Duration, Utc};
use common::{Env, env};
use serde_json::json;

async fn with_key(e: &Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

async fn message(e: &Env, to: &str, text: &str) {
    let ev =
        Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now()).to(to).payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
}

async fn sleep_all(e: &Env) {
    for a in e.hub.agents.all().await {
        e.hub.store.set_instance_state(&a.instance, InstanceState::Dormant, None).await.unwrap();
    }
    agents::stop_all(&e.hub).await;
    settle().await;
}

async fn spawn(e: &Env, def: &str) -> String {
    let chat = router::spawn(&e.hub, def, None, Some("work"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    chat
}

fn user() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn git_init(dir: &Path) {
    let out = Command::new("git").args(["init", "-q"]).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").output();
    assert!(out.unwrap().status.success());
}

/// A home with a git checkout holding ignored and kept files, a cache and a plain file.
fn fill_home(home: &Path) {
    let repo = home.join("proj");
    write(&repo.join(".gitignore"), "node_modules/\n*.log\n!keep.log\n/build\n");
    write(&repo.join("sub/.gitignore"), "secret.tmp\n");
    for f in ["src/main.rs", "keep.log", "sub/build", "sub/code.py"] {
        write(&repo.join(f), &format!("kept {f}"));
    }
    for f in ["node_modules/x/index.js", "debug.log", "build/out.o", "sub/secret.tmp", "sub/deep/secret.tmp"] {
        write(&repo.join(f), &format!("ignored {f}"));
    }
    git_init(&repo);
    write(&home.join(".cache/pip/blob"), "cache");
    write(&home.join("notes.md"), "my notes");
}

const KEPT: &[&str] = &[
    "notes.md",
    "proj/.gitignore",
    "proj/sub/.gitignore",
    "proj/src/main.rs",
    "proj/keep.log",
    "proj/sub/build",
    "proj/sub/code.py",
];
const GONE: &[&str] =
    &[".cache", "proj/node_modules", "proj/debug.log", "proj/build", "proj/sub/secret.tmp", "proj/sub/deep"];

#[test]
fn ignored_paths_follow_git_semantics() {
    let dir = tempfile::tempdir().unwrap();
    fill_home(dir.path());
    let mut got: Vec<String> = storage::ignored_paths(dir.path())
        .unwrap()
        .iter()
        .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().into_owned())
        .collect();
    got.sort();
    // `sub/deep` holds only ignored files, so git reports the directory.
    assert_eq!(got, ["proj/build", "proj/debug.log", "proj/node_modules", "proj/sub/deep", "proj/sub/secret.tmp"]);
}

#[tokio::test]
async fn archiving_cleans_and_packs_the_home_and_waking_unpacks_it() {
    let e = env().await;
    with_key(&e).await;
    let chat = spawn(&e, "dev").await;
    let home = e.hub.paths.home(&chat);
    fill_home(&home);
    let workspace_ignored = e.hub.paths.workspace(&chat).join("target/debug.bin");
    write(&e.hub.paths.workspace(&chat).join(".gitignore"), "target/\n");
    write(&workspace_ignored, "build output");
    let kept: Vec<Vec<u8>> = KEPT.iter().map(|f| std::fs::read(home.join(f)).unwrap()).collect();

    // Not archived: nothing is packed.
    sleep_all(&e).await;
    storage::sweep(&e.hub, Utc::now()).await.unwrap();
    assert!(home.join("proj/node_modules").exists());

    storage::force_archive(&e.hub, &chat, None).await.unwrap();
    storage::sweep(&e.hub, Utc::now()).await.unwrap();
    assert!(!home.exists(), "home packed away");
    assert!(storage::packed_home(&e.hub, &chat).is_file());
    assert!(!workspace_ignored.exists(), "workspace's ignored files cleaned");
    let report = e.hub.store.instance(&chat).await.unwrap().archive_report.unwrap();
    assert!(report.freed_bytes.unwrap() > 0, "{report:?}");
    let usage = storage::overview(&e.hub, true).await.unwrap();
    let c = usage.chats.iter().find(|c| c.instance == chat).unwrap();
    assert!(c.packed && c.home_bytes > 0);

    // Messaging restores it; waking unpacks the home byte for byte, minus what was rebuildable.
    message(&e, &chat, "back to work").await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running);
    assert!(!storage::packed_home(&e.hub, &chat).exists());
    for (f, before) in KEPT.iter().zip(&kept) {
        assert_eq!(&std::fs::read(home.join(f)).unwrap(), before, "{f} kept byte for byte");
    }
    assert_eq!(std::fs::read_to_string(home.join("proj/src/main.rs")).unwrap(), "kept src/main.rs");
    assert_eq!(std::fs::read_to_string(home.join("proj/keep.log")).unwrap(), "kept keep.log");
    for f in GONE {
        assert!(!home.join(f).exists(), "{f} should be cleaned");
    }
    assert!(home.join("proj/.git").is_dir(), "the checkout itself stays");
}

#[tokio::test]
async fn shared_homes_are_never_packed() {
    let e = env().await;
    with_key(&e).await;
    e.commit(&[
        ("agents/proj/agent.toml", "kind = \"ephemeral\"\nhome = \"groundstation\"\n"),
        ("agents/proj/AGENTS.md", "p"),
    ])
    .await;
    let a = spawn(&e, "proj").await;
    let b = spawn(&e, "proj").await;
    let shared = e.hub.paths.shared_home("groundstation");
    fill_home(&shared);
    sleep_all(&e).await;
    storage::force_archive(&e.hub, &a, None).await.unwrap();
    storage::force_archive(&e.hub, &b, None).await.unwrap();
    storage::sweep(&e.hub, Utc::now()).await.unwrap();
    assert!(shared.join("proj/node_modules/x/index.js").exists(), "shared home untouched");
    assert!(!storage::packed_home(&e.hub, &a).exists());
    let id = ops::call(&e.hub, &user(), "delete_chat", json!({"chat": a})).await.unwrap();
    assert!(id["freed_bytes"].is_u64());
    assert!(shared.join("notes.md").exists(), "deleting a chat keeps the shared home");
}

#[tokio::test]
async fn delete_chat_removes_an_archived_chat_for_good() {
    let e = env().await;
    with_key(&e).await;
    let chat = spawn(&e, "dev").await;
    fill_home(&e.hub.paths.home(&chat));
    sleep_all(&e).await;
    let refused = ops::call(&e.hub, &user(), "delete_chat", json!({"chat": chat})).await;
    assert!(refused.is_err(), "only archived chats");
    storage::force_archive(&e.hub, &chat, None).await.unwrap();
    storage::sweep(&e.hub, Utc::now()).await.unwrap();
    let events_before = e.hub.store.events_after(None, 10_000).await.unwrap().len();

    let out = ops::call(&e.hub, &user(), "delete_chat", json!({"chat": chat})).await.unwrap();
    assert!(out["freed_bytes"].as_u64().unwrap() > 0);
    assert!(e.hub.store.find_instance(&chat).await.unwrap().is_none());
    for p in [
        e.hub.paths.home(&chat),
        storage::packed_home(&e.hub, &chat),
        e.hub.paths.workspace(&chat),
        e.hub.paths.sessions(&chat),
    ] {
        assert!(!p.exists(), "{} deleted", p.display());
    }
    assert_eq!(e.hub.store.events_after(None, 10_000).await.unwrap().len(), events_before, "history stays");
    // The user only.
    let agent = Caller {
        principal: Principal::Agent { instance: "botler".into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    assert!(!ops::visible(&agent).any(|o| o.name == "delete_chat" || o.name == "storage_usage"));
}

#[tokio::test]
async fn dormant_chats_are_archived_after_a_while_a_few_at_a_time() {
    let e = env().await;
    with_key(&e).await;
    message(&e, "botler", "hi").await;
    let mut chats = Vec::new();
    for _ in 0..6 {
        chats.push(spawn(&e, "dev").await);
    }
    sleep_all(&e).await;
    let requested = async || {
        let mut n = Vec::new();
        for c in &chats {
            if e.hub.store.instance(c).await.unwrap().archive_requested_at.is_some() {
                n.push(c.clone());
            }
        }
        n
    };
    storage::auto_archive(&e.hub, Utc::now() + Duration::days(29)).await.unwrap();
    assert!(requested().await.is_empty(), "not before 30 days");

    storage::auto_archive(&e.hub, Utc::now() + Duration::days(31)).await.unwrap();
    assert_eq!(requested().await.len(), storage::AUTO_ARCHIVE_PER_HOUR as usize, "rate limited");
    assert!(e.hub.store.instance("botler").await.unwrap().archive_requested_at.is_none(), "persistent never");
    let asked = e.hub.store.events_after(None, 10_000).await.unwrap();
    assert_eq!(asked.iter().filter(|ev| ev.kind == kinds::ARCHIVE_REQUESTED).count(), 4);

    // No handover within a day: archived without it.
    storage::auto_archive(&e.hub, Utc::now() + Duration::days(32)).await.unwrap();
    let archived = {
        let mut n = 0;
        for c in &chats {
            let i = e.hub.store.instance(c).await.unwrap();
            if i.state == InstanceState::Archived {
                assert!(i.archive_report.unwrap().forced);
                n += 1;
            }
        }
        n
    };
    assert_eq!(archived, 4);

    // "never" turns it off.
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/botler.toml")).unwrap();
    let toml = toml.replace("[defaults]\n", "[defaults]\nauto_archive_after = \"never\"\n");
    e.commit(&[("botler.toml", toml.as_str())]).await;
    assert_eq!(e.hub.applied().await.defs.instance.defaults.auto_archive_after, None);
}

#[tokio::test]
async fn restoring_reconciles_grants_subscriptions_and_a_removed_definition() {
    let e = env().await;
    with_key(&e).await;
    let secrets = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/secrets.toml")).unwrap();
    let groups = "\n[[group]]\nname = \"nas\"\nclass = \"container\"\n[[group.secret]]\nname = \"NAS_PASS\"\n\
                  \n[[group]]\nname = \"imap\"\nclass = \"container\"\n[[group.secret]]\nname = \"IMAP_PASS\"\n";
    let with_groups = secrets.clone() + groups;
    e.commit(&[
        ("secrets.toml", with_groups.as_str()),
        ("events/mail.toml", "[[event]]\ntype = \"mail.received\"\n"),
        ("agents/temp/agent.toml", "kind = \"ephemeral\"\n"),
        ("agents/temp/AGENTS.md", "temp"),
    ])
    .await;
    e.hub.store.set_secret(&e.hub.secret_key, "NAS_PASS", "x").await.unwrap();
    e.hub.store.set_secret(&e.hub.secret_key, "IMAP_PASS", "y").await.unwrap();
    let chat = spawn(&e, "temp").await;
    let nas = e.hub.store.add_grant(&chat, &"secret:nas".parse().unwrap(), None, "user").await.unwrap();
    let imap = e.hub.store.add_grant(&chat, &"secret:imap".parse().unwrap(), None, "user").await.unwrap();
    let sub = e
        .hub
        .store
        .add_subscription(&NewSubscription {
            instance: chat.clone(),
            pattern: "mail.received".into(),
            filter: Default::default(),
            wake: true,
            note: None,
        })
        .await
        .unwrap();
    sleep_all(&e).await;
    storage::force_archive(&e.hub, &chat, None).await.unwrap();

    // While archived: the nas group goes, imap loses its value, mail.received and `temp` go.
    let only_imap =
        secrets + "\n[[group]]\nname = \"imap\"\nclass = \"container\"\n[[group.secret]]\nname = \"IMAP_PASS\"\n";
    e.commit_removing(&[("secrets.toml", only_imap.as_str())], &["events/mail.toml", "agents/temp"]).await;
    e.hub.store.delete_secret("IMAP_PASS").await.unwrap();

    // A message cannot restore it: held, and the user is offered a way out.
    message(&e, &chat, "are you there?").await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Archived);
    assert!(e.hub.store.pending_deliveries(&chat).await.unwrap().is_empty(), "held");
    let gone = restore::restore(&e.hub, &chat, false).await;
    assert!(matches!(gone, Err(botler_hub::HubError::DefinitionGone { .. })), "{gone:?}");

    let changes = restore::restore(&e.hub, &chat, true).await.unwrap();
    assert_eq!(changes.len(), 4, "{changes:#?}");
    assert!(changes[0].contains("`temp` no longer exists"), "{changes:#?}");
    let inst = e.hub.store.instance(&chat).await.unwrap();
    assert_eq!((inst.state, inst.definition.as_str()), (InstanceState::Dormant, restore::GENERIC));
    let grants = e.hub.store.live_grants(&chat).await.unwrap();
    let stale = |id: &str| grants.iter().find(|g| g.id == id).unwrap().stale.clone();
    assert!(stale(&nas.id).unwrap().contains("no secret group nas"), "{grants:?}");
    assert!(stale(&imap.id).unwrap().contains("no value for IMAP_PASS"), "{grants:?}");
    assert!(e.hub.store.subscription(&sub.id).await.unwrap().stale.is_some());
    assert!(e.hub.store.active_subscriptions().await.unwrap().is_empty(), "stale subscriptions are not routed");
    let (granted, _) = botler_hub::grants::environment(&e.hub, &chat).await.unwrap();
    assert!(granted.secrets.is_empty(), "stale grants are not applied");

    // A quiet note rides along with the held message, which is released.
    let pending = e.hub.store.pending_deliveries(&chat).await.unwrap();
    assert_eq!(pending.len(), 2, "{pending:?}");
    let mut kinds_seen = Vec::new();
    for d in &pending {
        let ev = e.hub.store.event(&d.event_id).await.unwrap();
        if ev.kind == kinds::CHAT_RESTORED {
            assert!(d.quiet);
            assert_eq!(ev.payload["changes"].as_array().unwrap().len(), 4);
        }
        kinds_seen.push(ev.kind);
    }
    kinds_seen.sort();
    assert_eq!(kinds_seen, [kinds::CHAT_RESTORED, kinds::MESSAGE]);

    // Restoring again changes nothing.
    assert!(restore::restore(&e.hub, &chat, false).await.unwrap().is_empty());
}
