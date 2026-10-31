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
    assert!(read("mcp.json")["mcpServers"].get("browser").is_none(), "no MCP endpoint for a plain service");
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
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir(&chat).join("mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["web"]["url"], "http://web:8000/mcp");
}
