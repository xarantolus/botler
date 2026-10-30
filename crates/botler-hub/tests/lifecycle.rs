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
