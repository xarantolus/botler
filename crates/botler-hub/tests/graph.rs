//! The per-chat graph on the fake runtime: grants (spawn env, `request_access`, policy, revoke,
//! archive) and runtime subscriptions (routing, quiet notes, safety, suspension), plus the event
//! type catalog in search.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{agents, grants, router};
use botler_store::{ArchiveRecord, DialogKind};
use chrono::{Duration, Utc};
use common::{Env, env};
use serde_json::{Value, json};

const SECRETS: &str = r#"version = 1
[[group]]
name = "openrouter"
class = "container"
[[group.secret]]
name = "OPENROUTER_API_KEY"

[[group]]
name = "nas"
class = "container"
[[group.secret]]
name = "NAS_PASS"

[[group]]
name = "imap"
class = "container"
[[group.secret]]
name = "IMAP_PASS"

[[group]]
name = "github-ssh"
class = "container"
[[group.secret]]
name = "GITHUB_SSH_KEY"
file = "/home/agent/.ssh/id_ed25519"
"#;

const EVENTS: &str = r#"[[event]]
type = "mail.received"
description = "A new mail arrived in a watched folder."
schema = { type = "object", properties = { folder = { type = "string" }, subject = { type = "string" } } }

[[event]]
type = "mail.raw"
description = "Raw mail contents."
restricted = true
"#;

async fn setup() -> Env {
    let e = env().await;
    let mut files: Vec<(String, String)> = vec![
        ("secrets.toml".into(), SECRETS.into()),
        ("events/mail.toml".into(), EVENTS.into()),
        ("events/ping.toml".into(), "[[event]]\ntype = \"ping.sent\"\n".into()),
    ];
    files.extend(common::job("mail-watch", "command = [\"watch\"]\nemits = [\"mail.received\"]\n"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    for (k, v) in
        [("OPENROUTER_API_KEY", "sk"), ("NAS_PASS", "hunter2"), ("IMAP_PASS", "imap"), ("GITHUB_SSH_KEY", "KEY")]
    {
        e.hub.store.set_secret(&e.hub.secret_key, k, v).await.unwrap();
    }
    e
}

fn agent(name: &str) -> Caller {
    Caller {
        principal: Principal::Agent { instance: name.into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    }
}

fn user() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

async fn op(e: &Env, c: &Caller, name: &str, args: Value) -> Result<Value, OpError> {
    ops::call(&e.hub, c, name, args).await
}

/// Route, deliver and let the fake pi settle until nothing changes (restarts take two rounds).
async fn run(e: &Env) {
    for _ in 0..4 {
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

fn container_env(e: &Env, instance: &str) -> std::collections::BTreeMap<String, String> {
    e.fake.containers()[&agents::container_name(&e.hub, instance)].spec.env.clone()
}

async fn message(e: &Env, to: &str, text: &str) {
    let ev =
        Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now()).to(to).payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
}

async fn sleep_all(e: &Env) {
    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(3)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

fn archive_record() -> ArchiveRecord {
    ArchiveRecord {
        confirmed_by_agent: false,
        forced: true,
        reason: None,
        report: None,
        handed_to: None,
        at: Utc::now(),
        freed_bytes: None,
    }
}

#[tokio::test]
async fn spawn_grants_follow_the_chat_through_wakes_revoke_and_archive() {
    let e = setup().await;
    let out = op(
        &e,
        &user(),
        "spawn_agent",
        json!({"definition": "dev", "task": "sync photos", "env": {"secrets": ["nas"]}}),
    )
    .await
    .unwrap();
    assert_eq!(out["granted"], json!(["secret:nas"]));
    let dev = out["instance"].as_str().unwrap().to_string();
    run(&e).await;
    assert_eq!(container_env(&e, &dev)["NAS_PASS"], "hunter2", "applied on the first wake");

    // Every wake gets it again.
    sleep_all(&e).await;
    assert!(e.exited(&dev).await);
    message(&e, &dev, "again").await;
    run(&e).await;
    assert_eq!(container_env(&e, &dev)["NAS_PASS"], "hunter2", "and on the next");

    // Archive and restore keep it (suspended meanwhile).
    sleep_all(&e).await;
    e.hub.store.archive_instance(&dev, &archive_record()).await.unwrap();
    let c = op(&e, &user(), "list_connections", json!({"chat": dev})).await.unwrap();
    assert_eq!(c["archived"], true);
    assert_eq!(c["grants"][0]["suspended"], true);
    message(&e, &dev, "back").await; // restores it
    run(&e).await;
    assert_eq!(container_env(&e, &dev)["NAS_PASS"], "hunter2", "restored with its grant");

    // Revoking: the chat is told, and its container loses it (restarted before the notice).
    let grant = c["grants"][0]["id"].as_str().unwrap().to_string();
    let r = op(&e, &user(), "revoke_access", json!({"grant": grant})).await.unwrap();
    assert_eq!(r["revoked"], true);
    run(&e).await;
    assert!(!container_env(&e, &dev).contains_key("NAS_PASS"), "gone after revoking");
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, &dev));
    assert!(prompts.last().unwrap().contains("access.changed") && prompts.last().unwrap().contains("revoked"));
    // An agent cannot revoke someone else's grant.
    let g = grants::give(&e.hub, &dev, &"secret:imap".parse().unwrap(), None, "user", false).await.unwrap();
    assert!(matches!(
        op(&e, &agent("botler"), "revoke_access", json!({"grant": g.id})).await,
        Err(OpError::Forbidden(_))
    ));
}

#[tokio::test]
async fn request_access_asks_then_wakes_with_the_grant_and_always_writes_policy() {
    let e = setup().await;
    // A parent that does not hold `nas` cannot pass it on: the user is asked for the child.
    let out =
        op(&e, &agent("botler"), "spawn_agent", json!({"definition": "dev", "task": "t", "env": {"secrets": ["nas"]}}))
            .await
            .unwrap();
    assert_eq!(out["granted"], json!([]));
    assert_eq!(out["requested"].as_array().unwrap().len(), 1);
    let dev = out["instance"].as_str().unwrap().to_string();
    run(&e).await;
    assert!(!container_env(&e, &dev).contains_key("NAS_PASS"));

    // The chat asks for another group itself: a turn-ending request.
    assert!(ops::turn_ending().any(|n| n == "request_access"));
    assert!(matches!(
        op(&e, &agent(&dev), "request_access", json!({"resource": "secret:nope", "reason": "x"})).await,
        Err(OpError::BadRequest(m)) if m.contains("no secret group nope")
    ));
    let asked = op(&e, &agent(&dev), "request_access", json!({"resource": "secret:imap", "reason": "read my mail"}))
        .await
        .unwrap();
    assert_eq!(asked["status"], "asked");
    let id = asked["request_id"].as_str().unwrap();
    let d = e.hub.store.dialog(id).await.unwrap();
    assert_eq!(d.kind, DialogKind::Access);
    assert_eq!(d.ctas, [grants::ALLOW_CHAT, grants::ALLOW_ALWAYS, grants::DENY]);
    assert!(grants::answer(&e.hub, id, "maybe").await.is_err(), "only the offered answers");

    sleep_all(&e).await;
    grants::answer(&e.hub, id, grants::ALLOW_CHAT).await.unwrap();
    run(&e).await;
    assert_eq!(container_env(&e, &dev)["IMAP_PASS"], "imap", "the answer wakes it with the grant");
    let last = e.fake.prompts(&agents::container_name(&e.hub, &dev)).last().cloned().unwrap();
    assert!(last.contains("dialog.answered") && last.contains("Granted"), "{last}");

    // "Always allow" grants it and writes the policy through a hub commit.
    let spawn_req = out["requested"][0].as_str().unwrap();
    grants::answer(&e.hub, spawn_req, grants::ALLOW_ALWAYS).await.unwrap();
    run(&e).await;
    assert_eq!(container_env(&e, &dev)["NAS_PASS"], "hunter2");
    let main = e.hub.repo.main().await.unwrap();
    let policy = e.hub.repo.read_file(&main, "policies/grants.toml").await.unwrap().unwrap();
    assert!(policy.contains("\"secret:nas\" = \"auto\"") && policy.starts_with("# Which resources"), "{policy}");

    // From now on, `auto`: granted without a dialog, and the chat is told.
    let other = router::new_instance(&e.hub, "dev", Some("botler"), Some("t")).await.unwrap();
    let dialogs = e.hub.store.open_dialogs().await.unwrap().len();
    let r =
        op(&e, &agent(&other), "request_access", json!({"resource": "secret:nas", "reason": "backup"})).await.unwrap();
    assert_eq!(r["status"], "granted");
    assert_eq!(e.hub.store.open_dialogs().await.unwrap().len(), dialogs, "no dialog");
    run(&e).await;
    assert_eq!(container_env(&e, &other)["NAS_PASS"], "hunter2");
    assert!(e.fake.prompts(&agents::container_name(&e.hub, &other))[0].contains("access.changed"));
    let held =
        op(&e, &agent(&other), "request_access", json!({"resource": "secret:nas", "reason": "again"})).await.unwrap();
    assert_eq!(held["status"], "held");

    // Denied: nothing granted, still told.
    let ask =
        op(&e, &agent(&other), "request_access", json!({"resource": "secret:imap", "reason": "mail"})).await.unwrap();
    grants::answer(&e.hub, ask["request_id"].as_str().unwrap(), grants::DENY).await.unwrap();
    assert!(e.hub.store.live_grants(&other).await.unwrap().iter().all(|g| g.resource != "imap"));
}

#[tokio::test]
async fn subscriptions_route_wake_quietly_stop_and_are_safe() {
    let e = setup().await;
    let a = router::new_instance(&e.hub, "dev", Some("botler"), Some("watch mail")).await.unwrap();
    let b = router::new_instance(&e.hub, "dev", Some("botler"), Some("log mail")).await.unwrap();
    let mail = |folder: &str| {
        Event::new("mail.received", Source::Runnable("mail-watch".into()), Provenance::External, Utc::now())
            .payload(json!({"folder": folder, "subject": "hi"}))
    };

    // Feedback: unknown types get suggestions, unknown filter fields are named.
    match op(&e, &agent(&a), "subscribe", json!({"type": "mail.recieved"})).await {
        Err(OpError::BadRequest(m)) => assert!(m.contains("`mail.received`"), "{m}"),
        other => panic!("{other:?}"),
    }
    match op(&e, &agent(&a), "subscribe", json!({"type": "mail.received", "filter": {"fodler": "INBOX"}})).await {
        Err(OpError::BadRequest(m)) => assert!(m.contains("`fodler`") && m.contains("`folder`"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Restricted types need a grant.
    assert!(matches!(op(&e, &agent(&a), "subscribe", json!({"type": "mail.raw"})).await, Err(OpError::Forbidden(_))));
    grants::give(&e.hub, &a, &"event:mail.raw".parse().unwrap(), None, "user", false).await.unwrap();
    op(&e, &agent(&a), "subscribe", json!({"type": "mail.raw"})).await.unwrap();
    // No subscribing to what you emit yourself.
    op(&e, &agent(&b), "emit_event", json!({"type": "ping.sent", "payload": {}})).await.unwrap();
    match op(&e, &agent(&b), "subscribe", json!({"type": "ping.*"})).await {
        Err(OpError::BadRequest(m)) => assert!(m.contains("loop"), "{m}"),
        other => panic!("{other:?}"),
    }

    let s = op(
        &e,
        &agent(&a),
        "subscribe",
        json!({"type": "mail.received", "filter": {"folder": "INBOX"}, "note": "triage it"}),
    )
    .await
    .unwrap();
    assert_eq!(s["matches"], json!(["mail.received"]));
    let sub_id = s["subscription"]["id"].as_str().unwrap().to_string();
    // A glob covering a restricted type needs its grant too.
    assert!(matches!(op(&e, &agent(&b), "subscribe", json!({"type": "mail.*"})).await, Err(OpError::Forbidden(_))));
    op(&e, &agent(&b), "subscribe", json!({"type": "mail.received", "wake": false})).await.unwrap();
    let listed = op(&e, &agent(&a), "list_subscriptions", json!({})).await.unwrap();
    assert_eq!(listed["subscriptions"].as_array().unwrap().len(), 2);

    // A non-matching event reaches only the quiet subscriber, as a note that wakes nobody.
    e.hub.emit(mail("Spam")).await.unwrap();
    run(&e).await;
    assert!(e.hub.agents.get(&a).await.is_none() && e.hub.agents.get(&b).await.is_none(), "nobody woke");
    let quiet = e.hub.store.pending_deliveries(&b).await.unwrap();
    assert!(quiet.len() == 1 && quiet[0].quiet, "{quiet:?}");
    assert!(e.hub.store.pending_deliveries(&a).await.unwrap().is_empty());

    // A matching one wakes the sleeping subscriber, with its note.
    e.hub.emit(mail("INBOX")).await.unwrap();
    run(&e).await;
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, &a));
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].contains("mail.received") && prompts[0].contains("triage it"), "{}", prompts[0]);

    // Unsubscribing stops delivery.
    op(&e, &agent(&a), "unsubscribe", json!({"id": sub_id})).await.unwrap();
    e.hub.emit(mail("INBOX")).await.unwrap();
    run(&e).await;
    assert_eq!(e.fake.prompts(&agents::container_name(&e.hub, &a)).len(), 1);

    // Archived chats' subscriptions are suspended and come back on restore.
    e.hub.store.archive_instance(&b, &archive_record()).await.unwrap();
    let before = e.hub.store.pending_deliveries(&b).await.unwrap().len();
    e.hub.emit(mail("INBOX")).await.unwrap();
    e.route().await;
    assert_eq!(e.hub.store.pending_deliveries(&b).await.unwrap().len(), before);
    e.hub.store.restore_instance(&b).await.unwrap();
    e.hub.emit(mail("INBOX")).await.unwrap();
    e.route().await;
    assert_eq!(e.hub.store.pending_deliveries(&b).await.unwrap().len(), before + 1);
}

#[tokio::test]
async fn event_types_list_producers_and_subscribers_and_are_searchable() {
    let e = setup().await;
    let a = router::new_instance(&e.hub, "dev", Some("botler"), Some("t")).await.unwrap();
    op(&e, &agent(&a), "subscribe", json!({"type": "mail.received"})).await.unwrap();
    let out = op(&e, &user(), "list_event_types", json!({"query": "mail"})).await.unwrap();
    let types = out["types"].as_array().unwrap();
    let received = types.iter().find(|t| t["type"] == "mail.received").unwrap();
    assert_eq!(received["producers"], json!(["job mail-watch"]));
    assert!(received["subscribers"][0].as_str().unwrap().contains(&a), "{received}");
    assert!(types.iter().any(|t| t["type"] == "mail.raw" && t["restricted"] == true));
    assert!(types.iter().all(|t| t["type"].as_str().unwrap().starts_with("mail.")), "query narrows");
    let builtin = op(&e, &user(), "list_event_types", json!({"query": "runnable.*"})).await.unwrap();
    assert!(builtin["types"][0]["schema"]["properties"]["runnable"].is_object());

    botler_hub::search::index_pending(&e.hub).await.unwrap();
    let hits = op(&e, &agent(&a), "search", json!({"query": "mail-watch", "sources": ["event_type"]})).await.unwrap();
    let hits = hits["results"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0]["ref"], "mail.received");
    assert_eq!(hits[0]["source"], "event_type");

    // `emits` must name declared types.
    let report = botler_core::load(
        &botler_core::defs::MemTree::new()
            .with("botler.toml", "[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"p\"\nmodel = \"m\"\n")
            .with("jobs/j/job.toml", "command = [\"x\"]\nemits = [\"nope.never\"]\n")
            .with("jobs/j/Dockerfile", "FROM x"),
    );
    assert!(report.errors().any(|d| d.message.contains("nope.never")));
}

#[cfg(unix)]
#[tokio::test]
async fn a_granted_ssh_key_lands_private_in_a_private_ssh_dir() {
    use std::os::unix::fs::PermissionsExt;
    let e = setup().await;
    let out = op(
        &e,
        &user(),
        "spawn_agent",
        json!({"definition": "dev", "task": "push", "env": {"secrets": ["github-ssh"]}}),
    )
    .await
    .unwrap();
    let dev = out["instance"].as_str().unwrap().to_string();
    run(&e).await;
    let spec = &e.fake.containers()[&agents::container_name(&e.hub, &dev)].spec;
    let key = spec
        .mounts
        .iter()
        .find_map(|m| match m {
            botler_runtime::Mount::Bind { src, dst, read_only: true } if dst == "/home/agent/.ssh/id_ed25519" => {
                Some(src.clone())
            }
            _ => None,
        })
        .expect("key mounted at ~/.ssh/id_ed25519");
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "KEY");
    assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    let home = e.hub.paths.home(&dev).join(".ssh");
    assert_eq!(std::fs::metadata(&home).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(!spec.env.contains_key("GITHUB_SSH_KEY"), "files are not env vars");
}
