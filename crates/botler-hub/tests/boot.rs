//! The hub comes up even when the applied definitions no longer load (a newer hub validating
//! more strictly): degraded, with the invalid parts left out and an alarm, until a change fixes it.

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use botler_core::change::Verdict;
use botler_core::event::kinds;
use botler_core::{Event, FRONT_DOOR, Provenance, Source};
use botler_hub::config::HubConfig;
use botler_hub::{Hub, agents, router, transactions};
use botler_store::ProposalStatus;
use chrono::Utc;
use serde_json::json;

/// `network = "none"` with the template's hosted model route: valid once, rejected now.
const BROKEN: &str = "kind = \"persistent\"\nnetwork = \"none\"\n";

async fn wait_for(hub: &Hub, id: &str, want: &[ProposalStatus]) -> botler_store::Proposal {
    for _ in 0..400 {
        let p = hub.store.proposal(id).await.unwrap();
        let settled = p.status != ProposalStatus::Reviewing || p.reviewer.is_some();
        if want.contains(&p.status) && settled {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("proposal stuck in {:?}", hub.store.proposal(id).await.unwrap().status);
}

#[tokio::test]
async fn an_applied_commit_that_no_longer_loads_boots_degraded_until_fixed() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    // Main gains an agent that today's validation rejects, without the hub applying it.
    let base = e.hub.repo.main().await.unwrap();
    let head =
        e.push("agents/test/broken", &[("agents/broken/agent.toml", BROKEN), ("agents/broken/AGENTS.md", "hi")]).await;
    e.hub.repo.advance_main(&base, &head).await.unwrap();
    assert!(botler_hub::hub::load_at(&e.hub.repo, &head).await.is_err());

    // A restart (the same data directory) comes up anyway.
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    let hub = Hub::open(e.hub.paths.clone(), config, Arc::new(e.fake.clone())).await.unwrap();
    let applied = hub.applied().await;
    assert_eq!(applied.commit, head, "main's valid part");
    assert_eq!(applied.skipped, ["agents/broken"]);
    assert!(applied.problems.iter().any(|d| d.path == "agents/broken/agent.toml" && d.message.contains("network")));
    assert!(!applied.defs.agents.contains_key("broken") && applied.defs.agents.contains_key(FRONT_DOOR));
    drop(applied);

    // The front door hears about it, urgently.
    let alarm = hub
        .store
        .unrouted_events(100)
        .await
        .unwrap()
        .into_iter()
        .find(|ev| ev.to.as_deref() == Some(FRONT_DOOR) && ev.urgent)
        .expect("an alarm");
    let text = alarm.payload["text"].as_str().unwrap();
    assert!(text.contains("agents/broken") && text.contains("proposal"), "{text}");

    // Everything else works; the invalid agent is never started.
    while router::route_pending(&hub).await.unwrap() > 0 {}
    agents::dispatch_pending(&hub).await.unwrap();
    assert!(e.fake.prompts(&agents::container_name(&hub, FRONT_DOOR)).iter().any(|p| p.contains("agents/broken")));
    assert!(router::spawn(&hub, "broken", None, Some("x"), None, None).await.is_err());
    assert!(!e.fake.containers().contains_key(&agents::container_name(&hub, "broken")));

    // A change that fixes it merges, applies in full and clears the degraded state.
    let fixed = e.push("agents/botler/fix-broken", &[("agents/broken/agent.toml", "kind = \"persistent\"\n")]).await;
    assert_ne!(fixed, head);
    let p = transactions::propose(&hub, FRONT_DOOR, "agents/botler/fix-broken", "Fix the broken agent").await.unwrap();
    let p =
        wait_for(&hub, &p.id, &[ProposalStatus::Reviewing, ProposalStatus::AwaitingUser, ProposalStatus::Failed]).await;
    match p.status {
        ProposalStatus::Reviewing => {
            transactions::submit_review(&hub, &p.reviewer.unwrap(), &p.id, Verdict::Approve, "fixes it").await.unwrap()
        }
        ProposalStatus::AwaitingUser => transactions::on_approval(&hub, &p.id, true, None).await.unwrap(),
        _ => panic!("{:?}", p.review_notes),
    }
    wait_for(&hub, &p.id, &[ProposalStatus::Merged]).await;
    let applied = hub.applied().await;
    assert!(applied.problems.is_empty() && applied.skipped.is_empty());
    assert!(applied.defs.agents.contains_key("broken"));
    drop(applied);
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("broken")
        .payload(json!({"text": "hi"}));
    hub.emit(ev).await.unwrap();
    while router::route_pending(&hub).await.unwrap() > 0 {}
    agents::dispatch_pending(&hub).await.unwrap();
    assert!(e.fake.containers().contains_key(&agents::container_name(&hub, "broken")), "runs once fixed");
}

#[tokio::test]
async fn an_invalid_front_door_still_boots() {
    let e = common::env().await;
    let base = e.hub.repo.main().await.unwrap();
    let head = e.push("agents/test/x", &[("agents/botler/agent.toml", "kind = \"persistent\"\nbogus = 1\n")]).await;
    e.hub.repo.advance_main(&base, &head).await.unwrap();
    let config = HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)));
    let hub = Hub::open(e.hub.paths.clone(), config, Arc::new(e.fake.clone())).await.unwrap();
    let applied = hub.applied().await;
    assert!(applied.skipped.contains(&"agents/botler".to_string()), "{:?}", applied.skipped);
    assert!(!applied.problems.is_empty());
    drop(applied);
    // Nothing breaks without it: routing the alarm reports it cannot be delivered.
    while router::route_pending(&hub).await.unwrap() > 0 {}
    agents::keep_always_on(&hub).await.unwrap();
    // The boot-time template upgrade still runs (it is often the fix).
    botler_hub::template::upgrade_on_boot(&hub).await.unwrap();
}
