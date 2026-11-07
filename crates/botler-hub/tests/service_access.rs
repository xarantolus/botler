//! Reaching a service without MCP: a `service:<name>` grant (or `services = [...]`) joins the
//! services network and lists the service's address in `services.json`, not in `mcp.json`.

mod common;

use botler_core::defs::{Resource, ResourceKind};
use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::{agents, grants, router, services};
use botler_runtime::NetworkMode;
use chrono::Utc;
use serde_json::{Value, json};

const BROWSER: &str =
    "lifetime = \"service\"\ncommand = [\"chromium\", \"--remote-debugging-port=9222\"]\nport = 9222\n";

#[tokio::test]
async fn a_service_grant_joins_the_network_without_an_mcp_entry() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let files = common::job("browser", BROWSER);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    services::reconcile(&e.hub).await.unwrap();

    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    let browser = Resource::new(ResourceKind::Service, "browser");
    grants::give(&e.hub, &chat, &browser, None, "user", false).await.unwrap();
    assert!(grants::holds(&e.hub, &chat, &browser, None).await.unwrap());
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(chat.clone())
        .payload(json!({"text": "open the page"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let c = &e.fake.containers()[&agents::container_name(&e.hub, &chat)];
    assert_eq!(c.spec.network, NetworkMode::Named(e.hub.network()), "joins the services network");
    let read = |f: &str| -> Value {
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir(&chat).join(f)).unwrap()).unwrap()
    };
    assert!(read("mcp-services.json").get("browser").is_none(), "no MCP endpoint for a plain service");
    let listed = read("services.json");
    assert_eq!(listed[0]["name"], "browser");
    assert_eq!((listed[0]["host"].clone(), listed[0]["port"].clone()), (json!("browser"), json!(9222)));
    assert!(listed[0]["ip"].as_str().is_some_and(|ip| ip.starts_with("10.89.")), "{listed}");
    let c_spec = &c.spec.mounts;
    assert!(
        c_spec
            .iter()
            .any(|m| matches!(m, botler_runtime::Mount::Bind { dst, .. } if dst == "/botler/agent-dir/services.json"))
    );
}

#[tokio::test]
async fn a_grant_to_a_service_that_serves_mcp_gives_its_tools() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let files = common::job("web", "lifetime = \"service\"\ncommand = [\"serve\"]\nport = 8000\nserves_mcp = true\n");
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    services::reconcile(&e.hub).await.unwrap();

    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    grants::give(&e.hub, &chat, &Resource::new(ResourceKind::Service, "web"), None, "user", false).await.unwrap();
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(chat.clone())
        .payload(json!({"text": "browse"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let mcp: Value =
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir(&chat).join("mcp-services.json")).unwrap())
            .unwrap();
    assert_eq!(mcp["web"]["url"], "http://web:8000/mcp");
}

/// A job lists a service in `services` and reaches it by name on the services network (an MCP
/// server is plain HTTP to it); the first change that lets it asks the user.
#[tokio::test]
async fn a_job_reaches_a_service_it_lists() {
    let e = common::env().await;
    let mut files = common::job("browser", BROWSER);
    files.extend(common::job("watch", "command = [\"w\"]\nservices = [\"browser\"]\n"));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let before = e.hub.applied().await.defs.clone();
    e.commit(&refs).await;
    let after = e.hub.applied().await.defs.clone();
    let summary = botler_core::change::preview(&before, &after, &["jobs/watch/job.toml".into()], Utc::now());
    let reach = botler_core::change::Escalation::ServiceReached { job: "watch".into(), service: "browser".into() };
    assert!(summary.escalations.contains(&reach), "{:?}", summary.escalations);

    let run = botler_hub::jobs::start(&e.hub, "watch", None, None).await.unwrap().unwrap();
    let c = &e.fake.containers()[&botler_hub::jobs::container_name(&e.hub, &run)];
    assert_eq!(c.spec.network, NetworkMode::Named(e.hub.network()));
}

#[tokio::test]
async fn a_chat_that_woke_while_its_service_was_down_reconnects_when_it_is_back() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let files = common::job("browser", BROWSER);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    // Not reconciled yet: the service is down while the chat wakes.
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    grants::give(&e.hub, &chat, &Resource::new(ResourceKind::Service, "browser"), None, "user", false).await.unwrap();
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(chat.clone())
        .payload(json!({"text": "open the page"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let services_json = e.hub.paths.agent_dir(&chat).join("services.json");
    let listed: Value = serde_json::from_str(&std::fs::read_to_string(&services_json).unwrap()).unwrap();
    assert!(listed[0]["ip"].is_null(), "{listed}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await; // the turn ends

    agents::reconnect_services(&e.hub).await.unwrap();
    assert!(e.hub.agents.get(&chat).await.is_some(), "still down: nothing to reconnect to");
    services::reconcile(&e.hub).await.unwrap();
    agents::reconnect_services(&e.hub).await.unwrap();
    let mut reconnected = false;
    for _ in 0..100 {
        if e.hub.agents.get(&chat).await.is_none() {
            reconnected = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reconnected, "the chat kept its session without the service");

    // An agent may restart the service itself.
    let agent = botler_hub::ops::Caller {
        principal: botler_hub::auth::Principal::Agent { instance: chat.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let out = botler_hub::ops::call(&e.hub, &agent, "restart_service", json!({"service": "browser"})).await.unwrap();
    let stopped = out["stopped"].as_str().unwrap().to_string();
    let run = e.hub.store.run(&stopped).await.unwrap();
    assert_eq!(run.reason.as_deref(), Some(format!("restarted by {chat}").as_str()));

    // A chat that woke while it ran reconnects once a restart (a deploy) brought other tools.
    services::reconcile(&e.hub).await.unwrap();
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(chat.clone())
        .payload(json!({"text": "again"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let listed: Value = serde_json::from_str(&std::fs::read_to_string(&services_json).unwrap()).unwrap();
    assert!(listed[0]["run"].is_string(), "{listed}");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    agents::reconnect_services(&e.hub).await.unwrap();
    assert!(e.hub.agents.get(&chat).await.is_some(), "restarted though its service did not");
    botler_hub::ops::call(&e.hub, &agent, "restart_service", json!({"service": "browser"})).await.unwrap();
    services::reconcile(&e.hub).await.unwrap();
    agents::reconnect_services(&e.hub).await.unwrap();
    let mut reconnected = false;
    for _ in 0..100 {
        if e.hub.agents.get(&chat).await.is_none() {
            reconnected = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(reconnected, "the chat kept the tools of the service's previous run");
}
