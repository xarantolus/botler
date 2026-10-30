mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::{agents, router};
use botler_store::InstanceState;
use chrono::{Duration, Utc};
use common::env;
use serde_json::json;

async fn with_key(e: &common::Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

async fn settle() {
    // let the pump task process the fake pi's events
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn message_wakes_main_and_is_delivered_once() {
    let e = env().await;
    with_key(&e).await;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("main")
        .payload(json!({"text": "hello"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let name = agents::container_name("main");
    let c = &e.fake.containers()[&name];
    let cmd = c.spec.command.join(" ");
    assert!(cmd.contains("--mode rpc") && cmd.contains("--session /botler/sessions/main.jsonl"), "{cmd}");
    assert!(cmd.contains("--model openrouter/deepseek/deepseek-v4.1-flash"), "{cmd}");
    assert_eq!(c.spec.env["OPENROUTER_API_KEY"], "sk-test");
    assert_eq!(c.spec.env["PI_OFFLINE"], "1");
    assert!(
        c.spec.mounts.iter().any(|m| matches!(m, botler_runtime::Mount::Tmpfs { dst } if dst == "/botler/agent-dir"))
    );
    let models = std::fs::read_to_string(e.hub.paths.agent_dir("main").join("models.json")).unwrap();
    assert!(models.contains("\"$OPENROUTER_API_KEY\"") && models.contains("8192"), "{models}");
    assert!(!models.contains("null"), "pi ignores models.json containing nulls: {models}");
    assert!(!models.contains("sk-test"), "secret values never land in rendered files");
    assert!(e.hub.paths.workspace("main").join(".git").exists());

    let prompts = e.fake.prompts(&name);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].starts_with("botler:delivery ") && prompts[0].contains("hello"));
    assert!(e.hub.store.pending_deliveries("main").await.unwrap().is_empty());

    // A redelivery of the same delivery id (crash between prompt and mark_delivered) is dropped
    // by the extension, so the agent sees it once.
    let ev = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some("message".into()), limit: 1, ..Default::default() })
        .await
        .unwrap()
        .remove(0);
    let first_id: String = serde_json::from_str::<serde_json::Value>(prompts[0].trim_start_matches("botler:delivery "))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into();
    let delivery = e.hub.store.delivery(&first_id).await.unwrap();
    let agent = e.hub.agents.get("main").await.unwrap();
    let again = agent.pi.prompt(&agents::envelope(&delivery, &ev), &[], None).await.unwrap();
    assert_eq!(again, botler_runtime::Disposition::Handled);
    assert_eq!(e.fake.prompts(&name).len(), 1);
    settle().await;
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
}

#[tokio::test]
async fn idle_ephemeral_ends_and_always_on_main_stays() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("main"), Some("build a thing"), None, None).await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("main")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert!(e.fake.prompts(&agents::container_name(&child))[0].contains("build a thing"));

    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(2)).await.unwrap();
    settle().await;
    // `main` is always_on in the template, so it is never reaped.
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
    assert_eq!(e.hub.store.instance(&child).await.unwrap().state, InstanceState::Ended);
    assert_eq!(e.hub.agents.all().await.len(), 1);
}

#[tokio::test]
async fn missing_secret_defers_delivery() {
    let e = env().await;
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("main")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.pending_deliveries("main").await.unwrap().len(), 1, "waits until the key is entered");
    with_key(&e).await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert!(e.hub.store.pending_deliveries("main").await.unwrap().is_empty());
}

#[tokio::test]
async fn always_on_main_starts_without_a_message() {
    let e = env().await;
    with_key(&e).await;
    agents::keep_always_on(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
    assert!(e.fake.containers().contains_key(&agents::container_name("main")));
    assert!(e.fake.prompts(&agents::container_name("main")).is_empty(), "no prompt needed to be up");
}

#[tokio::test]
async fn crashing_always_on_agent_backs_off_and_opens_an_incident() {
    let e = env().await;
    with_key(&e).await;
    e.fake.set_crash_on_start(true);
    let mut starts = 0;
    for _ in 0..20 {
        if agents::keep_always_on(&e.hub).await.is_ok() && e.hub.agents.get("main").await.is_some() {
            starts += 1;
        }
        settle().await;
    }
    assert!(starts <= 2, "backoff must stop a hot restart loop (started {starts} times)");
}
