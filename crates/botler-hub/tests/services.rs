//! Services: supervised long-running containers (fake runtime).

mod common;

use std::sync::Arc;

use botler_hub::auth::Principal;
use botler_hub::{agents, control, ops, services};
use botler_runtime::NetworkMode;
use botler_store::{IncidentStatus, Run, RunKind, RunStatus};
use chrono::{Duration, Utc};
use common::job;
use serde_json::json;

const SVC: &str = "lifetime = \"service\"\ncommand = [\"python\", \"-m\", \"app\"]\nport = 8000\n";

fn refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
}

async fn with_service(toml: &str) -> common::Env {
    let e = common::env().await;
    e.commit(&refs(&job("app", toml))).await;
    e
}

async fn runs(e: &common::Env) -> Vec<Run> {
    e.hub.store.runs_for("app", 50).await.unwrap()
}

async fn current(e: &common::Env) -> Run {
    runs(e).await.into_iter().next().expect("a run")
}

fn exit(e: &common::Env, code: i64) {
    e.fake.exit(&services::container_name(&e.hub, "app"), code);
}

#[tokio::test]
async fn service_starts_on_its_network_with_a_stable_name() {
    let e = with_service(SVC).await;
    services::reconcile(&e.hub).await.unwrap();
    let run = current(&e).await;
    assert_eq!((run.kind, run.status), (RunKind::Service, RunStatus::Running));
    let c = &e.fake.containers()[&services::container_name(&e.hub, "app")];
    assert_eq!(c.spec.network, NetworkMode::Named(e.hub.network()));
    assert_eq!(c.spec.network_aliases, ["app"]);
    assert!(e.fake.networks().contains(&e.hub.network()));
    // Running is the desired state: another pass changes nothing.
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(runs(&e).await.len(), 1);
}

#[tokio::test]
async fn exits_restart_with_backoff_and_a_crash_loop_waits_for_resolution() {
    let toml = format!("{SVC}[restart]\ncrashloop_count = 3\ncrashloop_window = \"1d\"\n");
    let e = with_service(&toml).await;
    let t0 = Utc::now();
    services::reconcile_at(&e.hub, t0).await.unwrap();

    exit(&e, 1);
    services::reconcile_at(&e.hub, Utc::now()).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Failed, "backing off (2s after one failure)");
    services::reconcile_at(&e.hub, Utc::now() + Duration::seconds(3)).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Running, "restarted after the backoff");
    let incidents = e.hub.store.open_incidents().await.unwrap();
    assert_eq!((incidents.len(), incidents[0].kind.as_str()), (1, "exit"));

    for i in 1..=2 {
        exit(&e, 1);
        services::reconcile_at(&e.hub, Utc::now() + Duration::hours(i)).await.unwrap();
    }
    assert_eq!(runs(&e).await.len(), 3, "the third failure is a crash loop: no fourth start");
    let incidents = e.hub.store.open_incidents().await.unwrap();
    assert_eq!(incidents.len(), 1, "one incident for the whole storm");
    assert_eq!((incidents[0].kind.as_str(), incidents[0].count), ("crashloop", 4));
    for _ in 0..3 {
        services::reconcile_at(&e.hub, Utc::now() + Duration::hours(5)).await.unwrap();
    }
    assert_eq!(runs(&e).await.len(), 3, "failed stays failed");
    assert_eq!(e.hub.store.open_incidents().await.unwrap()[0].count, 4, "reported once");

    e.hub.store.resolve_incident(&incidents[0].id, "fixed the config file").await.unwrap();
    services::reconcile_at(&e.hub, Utc::now()).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Running, "resolving restarts it at once");
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_or_failed_heartbeats_are_unhealthy() {
    let e = with_service(&format!("{SVC}[health]\nheartbeat = \"1m\"\ngrace = \"1s\"\n")).await;
    services::reconcile(&e.hub).await.unwrap();
    let run = current(&e).await;
    services::reconcile_at(&e.hub, Utc::now() + Duration::seconds(50)).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Running, "within grace + heartbeat");

    let caller = ops::Caller::resolve(&e.hub, Principal::Run { run_id: run.id.clone() }).await.unwrap();
    ops::call(&e.hub, &caller, "health", json!({"ok": true})).await.unwrap();
    let beat = e.hub.store.run(&run.id).await.unwrap().heartbeat_at.unwrap();
    services::reconcile_at(&e.hub, beat + Duration::seconds(50)).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Running);
    services::reconcile_at(&e.hub, beat + Duration::seconds(70)).await.unwrap();
    let ended = e.hub.store.run(&run.id).await.unwrap();
    assert_eq!(ended.status, RunStatus::Failed);
    assert!(ended.reason.unwrap().contains("no health heartbeat"));
    assert_eq!(e.hub.store.open_incidents().await.unwrap()[0].kind, "unhealthy");
    let stale = ops::Caller::resolve(&e.hub, Principal::Run { run_id: run.id.clone() }).await;
    assert!(stale.is_err(), "the token died with the run");

    // The service can also report itself unhealthy.
    services::reconcile_at(&e.hub, Utc::now() + Duration::minutes(10)).await.unwrap();
    let run = current(&e).await;
    assert_eq!(run.status, RunStatus::Running);
    let caller = ops::Caller::resolve(&e.hub, Principal::Run { run_id: run.id.clone() }).await.unwrap();
    ops::call(&e.hub, &caller, "health", json!({"ok": false, "reason": "database gone"})).await.unwrap();
    // Handled apart from the request (stopping the service drops the request it came on).
    let mut ended = e.hub.store.run(&run.id).await.unwrap();
    for _ in 0..100 {
        if ended.status != RunStatus::Running
            && !e.fake.containers().contains_key(&services::container_name(&e.hub, "app"))
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        ended = e.hub.store.run(&run.id).await.unwrap();
    }
    assert!(ended.status == RunStatus::Failed && ended.reason.unwrap().contains("database gone"));
    assert!(!e.fake.containers().contains_key(&services::container_name(&e.hub, "app")));
}

#[tokio::test]
async fn pause_stops_and_resume_restarts() {
    let e = with_service(SVC).await;
    services::reconcile(&e.hub).await.unwrap();
    control::pause(&e.hub).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Cancelled);
    assert!(e.fake.containers().is_empty());
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(runs(&e).await.len(), 1, "nothing starts while paused");
    control::resume(&e.hub).await.unwrap();
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Running);
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty(), "stopping on purpose is no failure");
}

#[tokio::test]
async fn only_changes_to_its_inputs_restart_it_and_removal_stops_it() {
    let e = with_service(SVC).await;
    services::reconcile(&e.hub).await.unwrap();
    let first = current(&e).await;
    e.commit(&[("agents/chat/AGENTS.md", "changed\n")]).await;
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(current(&e).await.id, first.id, "an unrelated merge leaves it alone");

    e.commit(&[("jobs/app/app.py", "print('v2')\n")]).await;
    // Its new image builds first (the old one serving meanwhile), then it is swapped.
    let mut second = current(&e).await;
    for _ in 0..100 {
        services::reconcile(&e.hub).await.unwrap();
        second = current(&e).await;
        if second.id != first.id {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_ne!(second.id, first.id);
    assert_eq!(e.hub.store.run(&first.id).await.unwrap().status, RunStatus::Cancelled);
    assert_eq!(second.status, RunStatus::Running);

    e.commit(&[("jobs/app/job.toml", "command = [\"true\"]\n")]).await; // now a oneshot
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.run(&second.id).await.unwrap().status, RunStatus::Cancelled);
    assert!(!e.fake.containers().contains_key(&services::container_name(&e.hub, "app")));
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
}

#[tokio::test]
async fn published_ports_are_mapped_and_a_taken_port_is_an_incident() {
    let fake = botler_runtime::fake::Fake::new();
    let a = common::env_on(Arc::new(fake.clone()), fake.clone()).await;
    let b = common::env_on(Arc::new(fake.clone()), fake.clone()).await;
    let toml = format!("{SVC}[[publish]]\nport = 8000\nhost_port = 18080\n");
    for e in [&a, &b] {
        e.commit(&refs(&job("app", &toml))).await;
        services::reconcile(&e.hub).await.unwrap();
    }
    let c = &fake.containers()[&services::container_name(&a.hub, "app")];
    assert_eq!(c.spec.ports[0].bind, "0.0.0.0");
    assert_eq!((c.spec.ports[0].host_port, c.spec.ports[0].port), (18080, 8000));
    let status = services::status(&a.hub).await.unwrap();
    assert!(
        status[0].state == botler_hub::wire::ServiceState::Running && status[0].published[0].url.ends_with(":18080"),
        "{status:?}"
    );

    let failed = current(&b).await;
    assert_eq!(failed.status, RunStatus::Failed);
    assert!(failed.reason.as_deref().unwrap().contains("host port 0.0.0.0:18080 is already in use"), "{failed:?}");
    let inc = &b.hub.store.open_incidents().await.unwrap()[0];
    assert_eq!((inc.kind.as_str(), inc.status), ("start", IncidentStatus::Open));
}

#[tokio::test]
async fn hubs_sharing_a_runtime_leave_each_other_alone() {
    let fake = botler_runtime::fake::Fake::new();
    let a = common::env_on(Arc::new(fake.clone()), fake.clone()).await;
    let b = common::env_on(Arc::new(fake.clone()), fake.clone()).await;
    assert_ne!(a.hub.id, b.hub.id);
    for e in [&a, &b] {
        e.commit(&refs(&job("app", SVC))).await;
        services::reconcile(&e.hub).await.unwrap();
    }
    // Both wake their front door: same instance name, different containers.
    for e in [&a, &b] {
        e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
        let ev = botler_core::Event::new(
            botler_core::event::kinds::MESSAGE,
            botler_core::Source::User,
            botler_core::Provenance::Trusted,
            Utc::now(),
        )
        .to("botler")
        .payload(json!({"text": "hi"}));
        e.hub.emit(ev).await.unwrap();
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
    }
    let mine =
        |e: &common::Env| fake.containers().keys().filter(|n| n.starts_with(&format!("botler-{}-", e.hub.id))).count();
    // Each: its front door, `app` and the template's `browser`.
    assert_eq!((mine(&a), mine(&b)), (3, 3));
    agents::stop_all(&b.hub).await;
    // B restarts (removing its leftover agents), pauses and drops its service.
    agents::reconcile_on_boot(&b.hub).await.unwrap();
    control::pause(&b.hub).await.unwrap();
    services::reconcile(&b.hub).await.unwrap();
    assert_eq!((mine(&a), mine(&b)), (3, 0));
    assert_eq!(current(&a).await.status, RunStatus::Running);
    // An exit event of B's service never reaches A's runs, and A's own reconcile keeps it running.
    services::reconcile(&a.hub).await.unwrap();
    assert_eq!(runs(&a).await.len(), 1);
}

#[tokio::test]
async fn granted_agents_join_the_network_and_get_mcp_servers() {
    use botler_core::event::kinds;
    use botler_core::{Event, Provenance, Source};

    let e = with_service(SVC).await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    e.commit(&[
        (
            "agents/user/agent.toml",
            "kind = \"persistent\"\nmcp = [\"app\"]\n[mcp_stdio.fs]\ncommand = [\"npx\", \"-y\", \"fs-mcp\", \".\"]\nenv = { ROOT = \"/home/agent\" }\n",
        ),
        ("agents/user/AGENTS.md", "uses the app"),
    ])
    .await;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("user")
        .payload(json!({"text": "hi"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let c = &e.fake.containers()[&agents::container_name(&e.hub, "user")];
    assert_eq!(c.spec.network, NetworkMode::Named(e.hub.network()));
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir("user").join("mcp.json")).unwrap())
            .unwrap();
    let servers = &mcp["mcpServers"];
    assert_eq!(servers["app"], json!({"url": "http://app:8000/mcp"}));
    assert_eq!(servers["fs"]["command"], "npx");
    assert_eq!(servers["fs"]["args"], json!(["-y", "fs-mcp", "."]));
    assert_eq!(servers["fs"]["env"]["ROOT"], "/home/agent");
    assert!(servers["botler"]["url"].as_str().unwrap().ends_with("/mcp"));
}

/// Real Podman (`BOTLER_PODMAN_TESTS=1`; needs the agent base image, which it never builds): a
/// Python HTTP service is reachable by name from an agent-style container on the hub's network
/// and on its published host port. (Containers reach the hub itself over its socket:
/// `tests/socket.rs`.) Removes its containers, network and images afterwards.
#[tokio::test]
async fn podman_service_is_reachable_by_name_and_on_its_host_port() {
    use botler_runtime::{ContainerSpec, Containers};
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    if rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap().is_none() {
        eprintln!("skipped: no agent base image");
        return;
    }
    let e = common::env_on(rt.clone(), botler_runtime::fake::Fake::new()).await;

    let host_port = 20000 + (std::process::id() % 20000) as u16;
    let toml = format!(
        "lifetime = \"service\"\ncommand = [\"python3\", \"-m\", \"http.server\", \"8000\"]\nport = 8000\n[[publish]]\nport = 8000\nhost_port = {host_port}\n"
    );
    e.commit(&refs(&job("web", &toml))).await;
    services::reconcile(&e.hub).await.unwrap();
    let run = e.hub.store.runs_for("web", 1).await.unwrap().remove(0);
    assert_eq!(run.status, RunStatus::Running, "{run:?}");

    let probe = "import time, urllib.request as u\n\
         def get(url):\n    err = None\n    for _ in range(50):\n        try:\n            return u.urlopen(url, timeout=2).status\n        except Exception as e:\n            err = e\n            time.sleep(0.2)\n    return repr(err)\n\
         print('service', get('http://web:8000/'))\n"
        .to_string();
    let name = format!("botler-{}-probe", e.hub.id);
    let spec = ContainerSpec {
        name: name.clone(),
        image: botler_hub::assets::AGENT_BASE_LATEST.into(),
        command: vec!["python3".into(), "-c".into(), probe],
        labels: e.hub.labels("test"),
        network: botler_runtime::NetworkMode::Named(e.hub.network()),
        ..Default::default()
    };
    rt.run_detached(&spec).await.unwrap();
    while rt.inspect(&name).await.unwrap().is_some_and(|c| c.running) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let logs = rt.logs(&name, 50).await.unwrap();

    // From the host, on the published port.
    let mut from_host = String::new();
    for _ in 0..50 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Ok(mut s) = tokio::net::TcpStream::connect(("127.0.0.1", host_port)).await {
            s.write_all(b"GET / HTTP/1.0\r\n\r\n").await.unwrap();
            s.read_to_string(&mut from_host).await.unwrap();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    rt.remove(&name).await.unwrap();
    services::stop(&e.hub, &run, "test done").await.unwrap();
    let net = tokio::process::Command::new(&rt.info().binary).args(["network", "rm", &e.hub.network()]).output();
    assert!(net.await.unwrap().status.success(), "removing the test network");
    let owned = e.hub.labels("x").into_iter().filter(|(k, _)| k == botler_hub::images::OWNER_LABEL).collect();
    for t in rt.images(&owned).await.unwrap() {
        rt.remove_image(&t).await.unwrap();
    }

    assert!(logs.contains("service 200"), "{logs}");
    assert!(from_host.starts_with("HTTP/1.0 200"), "{from_host}");
}

#[tokio::test]
async fn a_changed_service_keeps_serving_while_its_new_image_builds() {
    let e = with_service(SVC).await;
    services::reconcile(&e.hub).await.unwrap();
    let first = current(&e).await;
    e.fake.set_slow_builds("job-app", std::time::Duration::from_millis(600));
    e.commit(&[("jobs/app/app.py", "print('v2')\n")]).await;
    services::reconcile(&e.hub).await.unwrap();
    assert_eq!(current(&e).await.id, first.id, "the old one serves while the new image builds");
    assert_eq!(current(&e).await.status, RunStatus::Running);
    for _ in 0..100 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        services::reconcile(&e.hub).await.unwrap();
        let now = current(&e).await;
        if now.id != first.id {
            assert_eq!(now.status, RunStatus::Running);
            return;
        }
    }
    panic!("never swapped to the new image");
}

#[tokio::test]
async fn a_service_missing_a_secret_value_asks_the_user_once_instead_of_an_incident() {
    let e = common::env().await;
    let secrets = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/secrets.toml")).unwrap()
        + "\n[[group]]\nname = \"bot\"\nclass = \"container\"\n[[group.secret]]\nname = \"BOT_TOKEN\"\n";
    let mut files = job("app", &format!("{SVC}secrets = [\"bot\"]\n"));
    files.push(("secrets.toml".into(), secrets));
    e.commit(&refs(&files)).await;
    let t0 = Utc::now();
    services::reconcile_at(&e.hub, t0).await.unwrap();
    assert_eq!(current(&e).await.status, RunStatus::Failed);
    // Retried later: still one request.
    services::reconcile_at(&e.hub, t0 + Duration::hours(1)).await.unwrap();
    assert_eq!(runs(&e).await.len(), 2, "it tried again");
    let asks: Vec<_> = e
        .hub
        .store
        .open_dialogs()
        .await
        .unwrap()
        .into_iter()
        .filter(|d| d.about.contains(&"secret-group:bot".to_string()))
        .collect();
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0].instance, "botler");
    assert!(asks[0].detail.as_deref().unwrap().contains("`app` cannot start"));
    assert!(e.hub.store.incidents_for("app", 5).await.unwrap().is_empty(), "nothing to triage");
}
