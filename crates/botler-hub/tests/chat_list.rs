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
    // The chat starts with its first message: no time before.
    let before = None;
    deliver(hi.payload(json!({"text": "hi"}))).await;
    // Its reply counts too: wait until "hi" was delivered (it set the time) and the turn is over
    // (the reply set it again), with the time then staying put a while.
    let (mut first, mut still) = (before, 0);
    for _ in 0..200 {
        let now = Some(e.hub.store.instance("botler").await.unwrap().last_message_at);
        let idle = !e.hub.agents.get("botler").await.is_some_and(|a| a.busy());
        still = if now > before && now == first && idle { still + 1 } else { 0 };
        first = now;
        if still == 8 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(still, 8, "the turn ended");
    let first = first.unwrap();

    let ping = Event::new("test.ping", Source::Hub, Provenance::Trusted, Utc::now()).to("botler");
    assert_eq!(deliver(ping.payload(json!({}))).await, first, "a hub event is no message");

    let from = Event::new(kinds::MESSAGE, Source::Agent("dev".into()), Provenance::Trusted, Utc::now()).to("botler");
    assert!(deliver(from.payload(json!({"text": "the bill model"}))).await > first, "another chat wrote");
}
