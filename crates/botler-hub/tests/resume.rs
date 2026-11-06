//! A turn the hub's restart cuts off is resumed at boot (once the chat is woken with a note), but
//! not one that settled, was stopped or archived, nor one that keeps taking the hub down.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::config::HubConfig;
use botler_hub::ops::{self, Caller};
use botler_hub::{Hub, agents, router};
use botler_store::InstanceState;
use chrono::Utc;
use common::{Env, env};
use serde_json::json;

const NOTE: &str = "The botler hub restarted at";

async fn pump(hub: &Arc<Hub>) {
    for _ in 0..3 {
        while router::route_pending(hub).await.unwrap() > 0 {}
        agents::dispatch_pending(hub).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
}

/// A `dev` chat working on its task, its turn held open.
async fn working(e: &Env) -> String {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    e.fake.set_hold_turns(true);
    let child = router::spawn(&e.hub, "dev", Some("botler"), Some("build it"), None, None).await.unwrap();
    pump(&e.hub).await;
    assert!(e.hub.agents.get(&child).await.unwrap().busy(), "working on its task");
    child
}

/// Shut `hub` down as `serve` does, then boot a new hub on the same data directory.
async fn restart(e: &Env, hub: &Arc<Hub>, instance: &str) -> Arc<Hub> {
    agents::shutdown(hub).await;
    for _ in 0..200 {
        if hub.agents.get(instance).await.is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(hub.agents.get(instance).await.is_none(), "stopped");
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    let hub = Hub::open(e.hub.paths.clone(), config, Arc::new(e.fake.clone())).await.unwrap();
    agents::reconcile_on_boot(&hub).await.unwrap();
    hub
}

fn resumes(e: &Env, hub: &Hub, instance: &str) -> usize {
    e.fake.prompts(&agents::container_name(hub, instance)).iter().filter(|p| p.contains(NOTE)).count()
}

#[tokio::test]
async fn a_turn_cut_off_by_a_restart_is_resumed_first_and_once() {
    let e = env().await;
    let child = working(&e).await;
    // Queued behind the running turn when the hub stops.
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(child.clone())
        .payload(json!({"text": "after that, this", "mode": "queue"}));
    e.hub.emit(ev).await.unwrap();
    pump(&e.hub).await;
    assert_eq!(e.hub.store.pending_deliveries(&child).await.unwrap().len(), 1);

    let hub = restart(&e, &e.hub, &child).await;
    let pending = hub.store.pending_deliveries(&child).await.unwrap();
    assert_eq!(pending.len(), 2);
    let first = hub.store.event(&pending[0].event_id).await.unwrap();
    assert_eq!(first.source, Source::Hub);
    let text = first.payload["text"].as_str().unwrap();
    assert!(text.contains(NOTE) && text.contains("continue your task"), "{text}");

    pump(&hub).await;
    assert!(hub.agents.get(&child).await.unwrap().busy(), "woken and working again");
    let prompts = e.fake.prompts(&agents::container_name(&hub, &child));
    let note = prompts.iter().position(|p| p.contains(NOTE)).expect("resume note delivered");
    assert!(prompts[note + 1..].iter().all(|p| !p.contains("after that, this")), "the queued message waits");
    assert_eq!(hub.store.pending_deliveries(&child).await.unwrap().len(), 1, "one turn, the queued one waits");

    // A turn that settles is over: the next restart does not resume it.
    e.fake.set_hold_turns(false);
    assert!(e.fake.pi_event(&agents::container_name(&hub, &child), json!({"type": "agent_settled"})));
    pump(&hub).await;
    assert!(hub.store.pending_deliveries(&child).await.unwrap().is_empty(), "the queued message followed");
    let before = resumes(&e, &hub, &child);
    let hub = restart(&e, &hub, &child).await;
    pump(&hub).await;
    assert_eq!(resumes(&e, &hub, &child), before, "nothing to resume");
    assert!(hub.agents.get(&child).await.is_none(), "stays asleep");
}

#[tokio::test]
async fn stopped_and_archived_chats_are_not_resumed() {
    let e = env().await;
    let stopped = working(&e).await;
    let archived = router::spawn(&e.hub, "dev", Some("botler"), Some("other"), None, None).await.unwrap();
    pump(&e.hub).await;
    assert!(e.hub.agents.get(&archived).await.unwrap().busy());
    let user = Caller { principal: Principal::User, capabilities: vec![], runnable: None };
    ops::call(&e.hub, &user, "stop_chat", json!({"chat": stopped})).await.unwrap();

    agents::shutdown(&e.hub).await;
    assert!(e.exited(&stopped).await && e.exited(&archived).await);
    e.hub.store.set_instance_state(&archived, InstanceState::Archived, None).await.unwrap();
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    let hub = Hub::open(e.hub.paths.clone(), config, Arc::new(e.fake.clone())).await.unwrap();
    agents::reconcile_on_boot(&hub).await.unwrap();
    for chat in [&stopped, &archived] {
        for d in hub.store.pending_deliveries(chat).await.unwrap() {
            let ev = hub.store.event(&d.event_id).await.unwrap();
            assert!(!ev.payload["text"].as_str().is_some_and(|t| t.contains(NOTE)), "{chat} not resumed");
        }
    }
}

#[tokio::test]
async fn a_turn_that_keeps_taking_the_hub_down_opens_an_incident() {
    let e = env().await;
    let child = working(&e).await;
    let mut hub = e.hub.clone();
    for n in 1..=agents::MAX_RESUMES as usize {
        hub = restart(&e, &hub, &child).await;
        pump(&hub).await;
        assert_eq!(resumes(&e, &hub, &child), n, "resumed after restart {n}");
        assert!(hub.agents.get(&child).await.unwrap().busy());
    }
    let hub = restart(&e, &hub, &child).await;
    pump(&hub).await;
    assert_eq!(resumes(&e, &hub, &child), agents::MAX_RESUMES as usize, "not woken again");
    let incidents = hub.store.open_incidents().await.unwrap();
    assert!(
        incidents.iter().any(|i| i.runnable == format!("agent:{child}") && i.kind == "interrupted"),
        "{incidents:?}"
    );
    // The user hears of it through the front door; no triage chat is started for it.
    let chats = hub.store.live_instances().await.unwrap();
    assert!(!chats.iter().any(|i| i.definition == "triage"), "{chats:?}");
    let q = botler_store::EventQuery { kind: Some("runnable.failed".into()), limit: 5, ..Default::default() };
    let failed = &hub.store.events(&q).await.unwrap()[0];
    let to: Vec<String> =
        hub.store.deliveries_for_event(&failed.id.to_string()).await.unwrap().into_iter().map(|d| d.instance).collect();
    assert_eq!(to, [botler_core::FRONT_DOOR]);
}
