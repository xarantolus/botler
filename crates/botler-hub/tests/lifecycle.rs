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
    let items = botler_hub::sessions::read(&e.hub, "botler", None, None).await.unwrap().0;
    assert!(
        matches!(&items[0], botler_hub::sessions::Item::SessionStarted { previous, .. } if previous == "botler.jsonl")
    );
    let files = botler_hub::sessions::files(&e.hub, "botler").await.unwrap();
    assert_eq!(files.last(), Some(&file));
    assert!(files.contains(&"botler.jsonl".to_string()));
    assert!(botler_hub::sessions::read(&e.hub, "botler", Some("../../etc/passwd"), None).await.is_err());
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

#[tokio::test]
async fn a_chat_building_its_image_holds_up_no_other() {
    let e = setup().await;
    // Named after `dev-…`: a pass reaches it after the dev chat.
    let parent = router::new_instance(&e.hub, "triage", None, None).await.unwrap();
    // Its image is built; it sleeps.
    user_says(&e, &parent, "hello").await;
    pump(&e).await;
    agents::stop_all(&e.hub).await;
    settle().await;
    e.fake.set_slow_builds("image-dev", std::time::Duration::from_secs(20));
    let dev = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    user_says(&e, &parent, "are you there?").await;
    e.route().await;
    let hub = e.hub.clone();
    let pass = tokio::spawn(async move { agents::dispatch_pending(&hub).await });
    let name = agents::container_name(&e.hub, &parent);
    for _ in 0..200 {
        if e.fake.prompts(&name).iter().any(|p| p.contains("are you there?")) {
            assert!(e.hub.agents.get(&dev).await.is_none(), "the dev chat is still building");
            pass.abort();
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the chat waited for the other chat's image build");
}

#[tokio::test]
async fn the_user_pins_and_renames_chats_and_a_finished_chat_archives_itself() {
    let e = setup().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    ops::call(&e.hub, &user(), "pin_chat", json!({"chat": chat, "pinned": true})).await.unwrap();
    ops::call(&e.hub, &user(), "rename_chat", json!({"chat": chat, "title": "Flat search"})).await.unwrap();
    let i = e.hub.store.instance(&chat).await.unwrap();
    assert!(i.pinned && i.titled_by_user);
    assert_eq!(i.title.as_deref(), Some("Flat search"));

    // Its agent keeps the user's title, and archives the chat once its task is done.
    let agent = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    // It may still rename its slug (when the user asks), and the user's title stays.
    let r = ops::call(&e.hub, &agent, "set_chat_title", json!({"title": "Other", "slug": "other-topic"})).await;
    assert_eq!(r.unwrap()["title"], "Flat search");
    let i = e.hub.store.resolve_instance("other-topic").await.unwrap().unwrap();
    assert_eq!((i.name.as_str(), i.title.as_deref()), (chat.as_str(), Some("Flat search")));

    // A spawned chat is named by its spawner; a taken slug is refused before anything starts.
    let spawned =
        ops::call(&e.hub, &agent, "spawn_agent", json!({"definition": "chat", "task": "t", "slug": "flat-photos"}))
            .await
            .unwrap();
    let child = e.hub.store.resolve_instance("flat-photos").await.unwrap().unwrap();
    assert_eq!(
        (child.name.as_str(), child.title.as_deref()),
        (spawned["instance"].as_str().unwrap(), Some("Flat photos"))
    );
    let taken =
        ops::call(&e.hub, &agent, "spawn_agent", json!({"definition": "chat", "task": "t", "slug": "other-topic"}));
    assert!(taken.await.unwrap_err().to_string().contains("taken"));
    ops::call(&e.hub, &agent, "handover_ready", json!({"summary": "Found three flats and sent them to the user."}))
        .await
        .unwrap();
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, botler_store::InstanceState::Archived);

    // Botler only hands over when asked.
    router::create_persistent_chats(&e.hub).await.unwrap();
    let botler = Caller {
        principal: Principal::Agent { instance: "botler".into(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let r = ops::call(&e.hub, &botler, "handover_ready", json!({"summary": "Nothing to hand over, really."})).await;
    let err = r.unwrap_err().to_string();
    assert!(err.contains("nobody asked"), "{err}");
}

#[tokio::test]
async fn an_agent_waits_through_the_hub_instead_of_a_shell() {
    let e = setup().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let agent = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let out = ops::call(&e.hub, &agent, "wait", json!({"in": "10m", "reason": "the mail-watch trial"})).await.unwrap();
    assert!(out["note"].as_str().unwrap().contains("at the latest"));
    assert!(botler_hub::ops::turn_ending().any(|op| op == "wait"));
    let timers = e.hub.store.due_timers(Utc::now() + chrono::Duration::minutes(11)).await.unwrap();
    let t = timers.iter().find(|t| t.id == out["timer_id"].as_str().unwrap()).unwrap();
    assert!(t.prompt.as_deref().unwrap().contains("the mail-watch trial"));
    let long = ops::call(&e.hub, &agent, "wait", json!({"in": "3d", "reason": "x"})).await;
    assert!(long.unwrap_err().to_string().contains("at most a day"));

    // A new wait replaces the last one, and anything arriving ends it.
    let waits = |e: &Env| {
        let hub = e.hub.clone();
        async move { hub.store.pending_timers(None).await.unwrap().len() }
    };
    ops::call(&e.hub, &agent, "wait", json!({"in": "20m", "reason": "the page"})).await.unwrap();
    assert_eq!(waits(&e).await, 1);
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(&chat)
        .payload(json!({"text": "it is up"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert_eq!(waits(&e).await, 0);
}

#[tokio::test]
async fn a_chat_that_was_quiet_for_long_is_compacted_before_its_next_turn() {
    let e = setup().await;
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/botler.toml")).unwrap();
    let toml = toml.replace("[defaults]\n", "[defaults]\ncompact_after_idle = \"1s\"\n");
    e.commit(&[("botler.toml", toml.as_str())]).await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    user_says(&e, &chat, "hi").await;
    pump(&e).await;
    e.fake.set_context_tokens(&chat, 100_000);
    user_says(&e, &chat, "right away").await;
    pump(&e).await;
    assert_eq!(e.fake.compactions(&chat), 0, "a chat in use is not compacted");
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    user_says(&e, &chat, "much later").await;
    pump(&e).await;
    assert_eq!(e.fake.compactions(&chat), 1);
    assert!(e.hub.store.pending_deliveries(&chat).await.unwrap().is_empty(), "the message went in after it");
    // A small context is left alone.
    e.fake.set_context_tokens(&chat, 10_000);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    user_says(&e, &chat, "again").await;
    pump(&e).await;
    assert_eq!(e.fake.compactions(&chat), 1);
}

#[tokio::test]
async fn the_user_allows_a_chat_over_its_budget_more_for_today() {
    let e = setup().await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    // The template's default budget is $1 a day.
    e.hub.store.record_usage(&chat, Utc::now(), "m", 0, 1.5).await.unwrap();
    user_says(&e, &chat, "hi").await;
    pump(&e).await;
    let subject = format!("instance:{chat}");
    assert_eq!(e.hub.store.breaker(&subject).await.unwrap().unwrap().kind, "budget");

    let out = ops::call(&e.hub, &user(), "raise_budget", json!({"chat": chat, "usd": 0.2})).await.unwrap();
    assert!((out["usd_per_day"].as_f64().unwrap() - 1.2).abs() < 1e-9);
    assert!(e.hub.store.breaker(&subject).await.unwrap().is_some(), "still over: it stays stopped");
    let out = ops::call(&e.hub, &user(), "raise_budget", json!({"chat": chat, "usd": 1.0})).await.unwrap();
    assert!((out["usd_per_day"].as_f64().unwrap() - 2.2).abs() < 1e-9, "raises add up");
    assert!(e.hub.store.breaker(&subject).await.unwrap().is_none(), "resumed");

    user_says(&e, &chat, "go on").await;
    pump(&e).await;
    assert!(e.hub.store.breaker(&subject).await.unwrap().is_none());
    assert!(e.hub.store.pending_deliveries(&chat).await.unwrap().is_empty());
    // Only for today: the definition's budget is unchanged.
    assert_eq!(e.hub.applied().await.defs.instance.defaults.budget.usd_per_day, Some(1.0));
    let agent_caller = Caller {
        principal: Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    assert!(
        ops::call(&e.hub, &agent_caller, "raise_budget", json!({"chat": chat, "usd": 5.0})).await.is_err(),
        "agents cannot"
    );
}

#[tokio::test]
async fn a_turn_the_user_started_in_a_child_tells_the_parent_quietly() {
    let e = setup().await;
    let parent = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let child = router::spawn(&e.hub, "dev", Some(&parent), Some("build it"), None, None).await.unwrap();
    pump(&e).await;
    // Whether each end-of-turn notice to the parent woke it, oldest first.
    let notices = || async {
        let q = botler_store::EventQuery { kind: Some(kinds::AGENT_IDLE.into()), limit: 10, ..Default::default() };
        let mut out = Vec::new();
        for ev in e.hub.store.events(&q).await.unwrap().into_iter().rev() {
            for d in e.hub.store.deliveries_for_event(&ev.id.to_string()).await.unwrap() {
                assert_eq!(d.instance, parent);
                out.push(!d.quiet);
            }
        }
        out
    };
    assert_eq!(notices().await, [true], "the parent's task: its end wakes the parent");
    user_says(&e, &child, "also add tests").await;
    pump(&e).await;
    assert_eq!(notices().await, [true, false], "the user's own turn: a quiet note");
}
