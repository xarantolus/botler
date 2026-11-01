//! A chat's lifecycle on the fake runtime: stopping (with cascade).

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{agents, router};
use botler_store::DeliveryStatus;
use chrono::Utc;
use common::{Env, env};
use serde_json::{Value, json};

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
}

async fn pump(e: &Env) {
    for _ in 0..3 {
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
        settle().await;
    }
}

async fn user_says(e: &Env, to: &str, text: &str) {
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(to.to_string())
        .payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
}

fn user() -> Caller {
    Caller { principal: Principal::User, capabilities: vec![], runnable: None }
}

async fn setup() -> Env {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    e
}

/// The kinds of `instance`'s queued deliveries, and whether each is a quiet note.
async fn queued(e: &Env, instance: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for d in e.hub.store.pending_deliveries(instance).await.unwrap() {
        out.push((e.hub.store.event(&d.event_id).await.unwrap().kind, d.quiet));
    }
    out
}

#[tokio::test]
async fn stopping_aborts_cancels_cascades_and_tells_chat_and_parent() {
    let e = setup().await;
    let parent = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let child = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    let grandchild = router::new_instance(&e.hub, "dev", Some(&child), Some("test it")).await.unwrap();
    e.fake.set_hold_turns(true);
    pump(&e).await;
    assert!(e.hub.agents.get(&child).await.unwrap().busy(), "working on its task");
    user_says(&e, &grandchild, "queued, not yet delivered").await;
    e.route().await;
    let queued_id = e.hub.store.pending_deliveries(&grandchild).await.unwrap()[0].id.clone();

    // Agents cannot stop chats.
    let agent = Caller {
        principal: Principal::Agent { instance: parent.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    assert!(matches!(ops::call(&e.hub, &agent, "stop_chat", json!({"chat": child})).await, Err(OpError::Forbidden(_))));

    let out =
        ops::call(&e.hub, &user(), "stop_chat", json!({"chat": child, "cascade": true, "reason": "wrong approach"}))
            .await
            .unwrap();
    let stopped: &Vec<Value> = out["stopped"].as_array().unwrap();
    assert_eq!(stopped.len(), 2);
    assert_eq!(stopped[0]["chat"], child.as_str());
    assert_eq!(stopped[0]["aborted"], true);
    assert_eq!(stopped[1]["chat"], grandchild.as_str());
    assert_eq!(stopped[1]["cancelled"], 1);
    assert_eq!(stopped[1]["with"], child.as_str());
    settle().await;
    assert!(!e.hub.agents.get(&child).await.unwrap().busy(), "the turn ended");
    e.route().await;
    assert!(
        !queued(&e, &parent).await.iter().any(|(k, _)| k == "agent.idle"),
        "no end-of-turn notice wakes the parent"
    );

    // The cancelled delivery is recorded, not lost.
    let d = e.hub.store.delivery(&queued_id).await.unwrap();
    assert_eq!(d.status, DeliveryStatus::Cancelled);
    assert!(d.note.unwrap().contains("wrong approach"));
    // Each chat and each parent gets a quiet note.
    assert_eq!(queued(&e, &grandchild).await, [("chat.stopped".to_string(), true)]);
    assert!(queued(&e, &parent).await.contains(&("chat.stopped".to_string(), true)));
    assert_eq!(
        queued(&e, &child).await.iter().filter(|(k, q)| k == "chat.stopped" && *q).count(),
        2,
        "its own stop, and its child's"
    );

    // The grandchild reads why when it next wakes.
    e.fake.set_hold_turns(false);
    user_says(&e, &grandchild, "start over").await;
    pump(&e).await;
    let notes = e.fake.notes(&agents::container_name(&e.hub, &grandchild));
    assert!(notes.iter().any(|n| n.contains("chat.stopped") && n.contains("wrong approach")), "{notes:?}");
    assert!(e.fake.prompts(&agents::container_name(&e.hub, &grandchild)).last().unwrap().contains("start over"));
}

fn agent_caller(name: &str) -> Caller {
    Caller {
        principal: Principal::Agent { instance: name.into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    }
}

fn report() -> Value {
    json!({"summary": "Set up the weather tool and answered three questions about it.",
           "outstanding": ["add a 7-day view"], "state": ["memory: user/weather.md"]})
}

async fn kinds_to(e: &Env, to: &str, kind: &str) -> Vec<Event> {
    e.hub
        .store
        .events(&botler_store::EventQuery { kind: Some(kind.into()), limit: 20, ..Default::default() })
        .await
        .unwrap()
        .into_iter()
        .filter(|ev| ev.to.as_deref() == Some(to))
        .collect()
}

#[tokio::test]
async fn botler_rolls_over_to_a_fresh_session_seeded_with_its_report() {
    let e = setup().await;
    agents::keep_always_on(&e.hub).await.unwrap();
    settle().await;
    let old = e.fake.containers()[&agents::container_name(&e.hub, "botler")].spec.clone();
    assert!(old.command.join(" ").contains("--session /botler/sessions/botler.jsonl"));
    user_says(&e, "botler", "remember the weather tool").await;
    pump(&e).await;
    // The fake pi keeps no file; the real one would have written this.
    std::fs::write(e.hub.paths.sessions("botler").join("botler.jsonl"), "{\"type\":\"session\"}\n").unwrap();

    // Nothing asked for a handover yet.
    assert!(matches!(
        ops::call(&e.hub, &agent_caller("botler"), "handover_ready", report()).await,
        Err(OpError::BadRequest(_))
    ));
    let r = ops::call(&e.hub, &user(), "handover", json!({"chat": "botler"})).await.unwrap();
    assert_eq!((r["to"].as_str(), r["requested"].as_bool()), (Some("self"), Some(true)));
    pump(&e).await;
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, "botler"));
    assert!(prompts.last().unwrap().contains("handover.requested"));

    let out = ops::call(&e.hub, &agent_caller("botler"), "handover_ready", report()).await.unwrap();
    let file = out["session"].as_str().unwrap().to_string();
    assert_eq!(out["archived"], false);
    assert_ne!(file, "botler.jsonl");
    // The container restarts after the turn had time to end, on the new session file.
    tokio::time::sleep(botler_hub::handover::RESTART_AFTER + std::time::Duration::from_millis(500)).await;
    assert!(e.exited("botler").await);
    agents::keep_always_on(&e.hub).await.unwrap();
    pump(&e).await;
    let new = e.fake.containers()[&agents::container_name(&e.hub, "botler")].spec.clone();
    assert!(new.command.join(" ").contains(&format!("--session /botler/sessions/{file}")));
    assert_eq!(new.mounts[0], old.mounts[0], "same home");
    let notes = e.fake.notes(&agents::container_name(&e.hub, "botler"));
    assert!(notes.iter().any(|n| n.contains("session.started") && n.contains("7-day view")), "{notes:?}");

    // Both sessions are there; the new one starts with the typed report, the old one is readable.
    std::fs::write(
        e.hub.paths.sessions("botler").join(&file),
        format!(
            "{}\n{}\n",
            json!({"type": "session", "version": 3, "cwd": "/home/agent"}),
            json!({"type": "custom_message", "customType": "botler.note", "id": "n1", "timestamp": "2026-10-30T10:00:00Z",
                   "content": format!("[from hub | provenance: trusted | session.started | delivery d | event {}]\nreport",
                                      kinds_to(&e, "botler", "session.started").await[0].id)})
        ),
    )
    .unwrap();
    let items = botler_hub::sessions::read(&e.hub, "botler", None).await.unwrap();
    assert!(
        matches!(&items[0], botler_hub::sessions::Item::SessionStarted { previous, .. } if previous == "botler.jsonl")
    );
    let files = botler_hub::sessions::files(&e.hub, "botler").await.unwrap();
    assert_eq!(files.last(), Some(&file));
    assert!(files.contains(&"botler.jsonl".to_string()));
    assert!(botler_hub::sessions::read(&e.hub, "botler", Some("../../etc/passwd")).await.is_err());
}

#[tokio::test]
async fn a_full_context_asks_for_a_rollover_once() {
    let e = setup().await;
    agents::keep_always_on(&e.hub).await.unwrap();
    settle().await;
    e.fake.set_context_tokens("botler", 180_000);
    user_says(&e, "botler", "one").await;
    pump(&e).await;
    let asked = kinds_to(&e, "botler", "handover.requested").await;
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].payload["to"], "self");
    assert!(asked[0].payload["reason"].as_str().unwrap().contains("180000 tokens"));
    user_says(&e, "botler", "two").await;
    pump(&e).await;
    assert_eq!(kinds_to(&e, "botler", "handover.requested").await.len(), 1, "pending: not asked again");
}

#[tokio::test]
async fn a_handover_to_another_chat_sends_the_report_and_archives() {
    let e = setup().await;
    let dev = router::new_instance(&e.hub, "dev", None, Some("t")).await.unwrap();
    ops::call(&e.hub, &user(), "handover", json!({"chat": dev, "to": "botler"})).await.unwrap();
    let out = ops::call(&e.hub, &agent_caller(&dev), "handover_ready", report()).await.unwrap();
    assert_eq!((out["archived"].as_bool(), out["handed_to"].as_str()), (Some(true), Some("botler")));
    let msg = kinds_to(&e, "botler", "message").await;
    assert!(msg[0].payload["text"].as_str().unwrap().contains("7-day view"));
    assert_eq!(e.hub.store.instance(&dev).await.unwrap().state, botler_store::InstanceState::Archived);
}

#[tokio::test]
async fn agent_containers_have_a_git_identity() {
    let e = setup().await;
    agents::keep_always_on(&e.hub).await.unwrap();
    let env = &e.fake.containers()[&agents::container_name(&e.hub, "botler")].spec.env;
    for (k, v) in [
        ("GIT_AUTHOR_NAME", "botler"),
        ("GIT_COMMITTER_NAME", "botler"),
        ("GIT_AUTHOR_EMAIL", "botler@botler.invalid"),
        ("GIT_COMMITTER_EMAIL", "botler@botler.invalid"),
    ] {
        assert_eq!(env.get(k).map(String::as_str), Some(v), "{k}");
    }
}

#[tokio::test]
async fn a_turn_the_hub_aborted_is_not_a_failure_to_retry() {
    let e = setup().await;
    let parent = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let chat = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    e.fake.set_hold_turns(true);
    pump(&e).await;
    let agent = e.hub.agents.get(&chat).await.unwrap();
    assert!(agent.busy());
    // pi ends the aborted model call with "This operation was aborted".
    agent.abort().await.unwrap();
    settle().await;
    assert!(!agent.busy());
    assert!(e.hub.store.turn_retry(&chat).await.unwrap().is_none(), "the abort is not retried");
}

#[tokio::test]
async fn compact_and_clear_from_the_composer() {
    let e = setup().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let out = ops::call(&e.hub, &user(), "compact_chat", json!({"chat": chat})).await.unwrap();
    assert_eq!(out["note"], "Compacted.");
    assert_eq!(e.fake.compactions(&chat), 1);

    let before = e.hub.store.instance(&chat).await.unwrap().session_file;
    ops::call(&e.hub, &user(), "clear_chat", json!({"chat": chat})).await.unwrap();
    let after = e.hub.store.instance(&chat).await.unwrap().session_file;
    assert_ne!(before, after, "a fresh session");
    assert!(ops::call(&e.hub, &user(), "clear_chat", json!({"chat": "nope"})).await.is_err());
}

#[tokio::test]
async fn a_persistent_agent_has_its_chat_once_defined() {
    let e = setup().await;
    e.commit(&[("agents/curator/agent.toml", "kind = \"persistent\"\n"), ("agents/curator/AGENTS.md", "c")]).await;
    let chat = e.hub.store.instance("curator").await.unwrap();
    assert!(chat.persistent);
    assert!(e.hub.agents.get("curator").await.is_none(), "asleep until something is delivered");
}
