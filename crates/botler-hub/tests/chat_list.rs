//! The chat list's time: when someone last wrote in a chat, the user or another chat; not hub
//! events or timers.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::agents;
use chrono::Utc;
use common::env;
use serde_json::json;

#[tokio::test]
async fn a_message_from_another_chat_counts_and_a_hub_event_does_not() {
    let e = env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let deliver = async |ev: Event| {
        e.hub.emit(ev).await.unwrap();
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        e.hub.store.instance("botler").await.unwrap().last_message_at
    };
    let hi = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now()).to("botler");
    deliver(hi.payload(json!({"text": "hi"}))).await;
    // Its reply counts too: once the turn is over, the time stays.
    let mut first = e.hub.store.instance("botler").await.unwrap().last_message_at;
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let now = e.hub.store.instance("botler").await.unwrap().last_message_at;
        if now == first && !e.hub.agents.get("botler").await.is_some_and(|a| a.busy()) {
            break;
        }
        first = now;
    }

    let ping = Event::new("test.ping", Source::Hub, Provenance::Trusted, Utc::now()).to("botler");
    assert_eq!(deliver(ping.payload(json!({}))).await, first, "a hub event is no message");

    let from = Event::new(kinds::MESSAGE, Source::Agent("dev".into()), Provenance::Trusted, Utc::now()).to("botler");
    assert!(deliver(from.payload(json!({"text": "the bill model"}))).await > first, "another chat wrote");
}
