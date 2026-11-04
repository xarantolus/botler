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
    let mut pi = e.hub.watch_pi("botler");
    deliver(hi.payload(json!({"text": "hi"}))).await;
    // Its reply counts too: the hub records its time after the turn settles, in its own task, so
    // the time is read once it stays put.
    let settled = async { while pi.recv().await.unwrap()["type"] != "agent_settled" {} };
    tokio::time::timeout(std::time::Duration::from_secs(20), settled).await.expect("the turn settles");
    let stable = async {
        let mut last = e.hub.store.instance("botler").await.unwrap().last_message_at;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let now = e.hub.store.instance("botler").await.unwrap().last_message_at;
            if now == last {
                return now;
            }
            last = now;
        }
    };
    let first = tokio::time::timeout(std::time::Duration::from_secs(10), stable).await.expect("the time settles");

    let ping = Event::new("test.ping", Source::Hub, Provenance::Trusted, Utc::now()).to("botler");
    assert_eq!(deliver(ping.payload(json!({}))).await, first, "a hub event is no message");

    let from = Event::new(kinds::MESSAGE, Source::Agent("dev".into()), Provenance::Trusted, Utc::now()).to("botler");
    assert!(deliver(from.payload(json!({"text": "the bill model"}))).await > first, "another chat wrote");
}
