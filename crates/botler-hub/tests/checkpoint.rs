//! Memory checkpoints: an agent that talked with the user and wrote no memory gets one short
//! turn to save what is worth keeping before it sleeps.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller};
use botler_hub::{agents, checkpoint, router};
use botler_store::InstanceState;
use chrono::{Duration, Utc};
use common::{Env, env};
use serde_json::json;

async fn settle() {
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

/// A chat the user said something to; returns it and its container.
async fn talked_to(e: &Env, def: &str) -> (String, String) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    let chat = if def == "botler" {
        "botler".to_string()
    } else {
        router::spawn(&e.hub, def, None, None, None, None).await.unwrap()
    };
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(chat.clone())
        .payload(json!({"text": "I prefer short answers."}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let container = agents::container_name(&e.hub, &chat);
    assert!(e.fake.prompts(&container).last().unwrap().contains("short answers"));
    (chat, container)
}

fn checkpoints(e: &Env, container: &str) -> usize {
    e.fake.prompts(container).iter().filter(|p| p.contains("worth keeping in memory")).count()
}

/// Reap at `later`, deliver what that queued, and let the turns run.
async fn reap(e: &Env, later: Duration) {
    agents::reap_idle(&e.hub, Utc::now() + later).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
}

#[tokio::test]
async fn a_chat_that_talked_gets_one_checkpoint_then_sleeps() {
    let e = env().await;
    let (chat, container) = talked_to(&e, "chat").await;
    reap(&e, Duration::hours(2)).await;
    assert_eq!(checkpoints(&e, &container), 1);
    assert!(e.fake.prompts(&container).last().unwrap().contains("worth keeping in memory"));
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant, "asleep after it");
    assert!(e.hub.agents.get(&chat).await.is_none());
}

#[tokio::test]
async fn a_chat_that_wrote_memory_just_sleeps() {
    let e = env().await;
    let (chat, container) = talked_to(&e, "chat").await;
    let wake = e.hub.agents.get(&chat).await.unwrap().wake.clone();
    let me = Caller::resolve(&e.hub, Principal::Agent { instance: chat.clone(), wake }).await.unwrap();
    ops::call(&e.hub, &me, "memory_append", json!({"path": "user/preferences.md", "text": "- short answers"}))
        .await
        .unwrap();
    reap(&e, Duration::hours(2)).await;
    assert_eq!(checkpoints(&e, &container), 0);
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);
}

#[tokio::test]
async fn the_checkpoint_turn_does_not_trigger_another() {
    // `botler` is always on: it stays up after its checkpoint, idles again, and is not asked twice.
    let e = env().await;
    let (_, container) = talked_to(&e, "botler").await;
    reap(&e, Duration::hours(2)).await;
    let prompts = e.fake.prompts(&container).len();
    assert_eq!(checkpoints(&e, &container), 1);
    assert_eq!(e.hub.store.instance("botler").await.unwrap().state, InstanceState::Running);
    reap(&e, Duration::hours(4)).await;
    reap(&e, Duration::hours(6)).await;
    assert_eq!(e.fake.prompts(&container).len(), prompts, "one checkpoint for one conversation");
    assert_eq!(checkpoints(&e, &container), 1);
}

#[tokio::test]
async fn a_failing_checkpoint_still_sleeps() {
    let e = env().await;
    let (chat, container) = talked_to(&e, "chat").await;
    e.fake.fail_turns(&chat, &["400 something went wrong"]);
    reap(&e, Duration::hours(2)).await;
    assert_eq!(checkpoints(&e, &container), 1);
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty(), "a failed checkpoint is no incident");
}

#[tokio::test]
async fn a_hanging_checkpoint_is_cut_short() {
    let e = env().await;
    let (chat, container) = talked_to(&e, "chat").await;
    e.fake.set_hold_turns(true);
    reap(&e, Duration::hours(2)).await;
    assert_eq!(checkpoints(&e, &container), 1);
    assert!(e.hub.agents.get(&chat).await.is_some(), "still saving");
    reap(&e, Duration::hours(2) + Duration::seconds(checkpoint::TIMEOUT_SECS + 1)).await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);
}
