//! Containers reach the hub over its Unix socket: agents through a forwarder on 127.0.0.1:7420
//! inside their container, jobs through the SDK. With real Podman (`BOTLER_PODMAN_TESTS=1`), an
//! agent-style container without any network still reaches MCP and git.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::auth::Principal;
use botler_hub::config::{CONTAINER_RUN_DIR, FORWARD_URL};
use botler_hub::{agents, jobs, router};
use botler_runtime::Mount;
use botler_runtime::fake::FakeContainer;
use chrono::Utc;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn mount<'a>(c: &'a FakeContainer, dst: &str) -> Option<&'a Mount> {
    c.spec.mounts.iter().find(|m| matches!(m, Mount::Bind { dst: d, .. } if d == dst))
}

/// The hub's API answers on its socket, and binding again replaces a socket left behind.
#[tokio::test]
async fn the_api_is_served_on_the_socket() {
    let e = common::env().await;
    let path = e.hub.paths.socket().unwrap();
    drop(botler_hub::serve::bind_socket(&path).unwrap());
    let listener = botler_hub::serve::bind_socket(&path).unwrap();
    tokio::spawn(axum::serve(listener, botler_hub::api::router(e.hub.clone())).into_future());
    let mut s = tokio::net::UnixStream::connect(&path).await.unwrap();
    s.write_all(b"GET /healthz HTTP/1.0\r\nHost: hub\r\n\r\n").await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.0 200") && out.ends_with("ok"), "{out}");
}

/// The socket lives in a short runtime directory, so a hub with a long data directory path
/// (sockets allow about 108 bytes) still starts and serves on it.
#[tokio::test]
async fn a_long_data_directory_still_serves_on_the_socket() {
    let fake = botler_runtime::fake::Fake::new();
    let config = botler_hub::config::HubConfig::new(std::net::SocketAddr::from(([127, 0, 0, 1], 0)));
    let e = common::env_at(std::sync::Arc::new(fake.clone()), fake, config, &"d".repeat(150)).await;
    assert!(e.hub.paths.root.as_os_str().len() > 150);
    let path = e.hub.paths.socket().unwrap();
    assert!(path.as_os_str().len() < 108, "{}", path.display());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let served = botler_hub::serve::serve(e.hub.clone(), async move {
        if stopped.await.is_err() {
            eprintln!("test ended without stopping the hub");
        }
    });
    let client = async {
        let mut out = String::new();
        for _ in 0..200 {
            if let Ok(mut s) = tokio::net::UnixStream::connect(&path).await {
                s.write_all(b"GET /healthz HTTP/1.0\r\nHost: hub\r\n\r\n").await.unwrap();
                s.read_to_string(&mut out).await.unwrap();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        stop.send(()).unwrap();
        out
    };
    let (served, out) = tokio::join!(served, client);
    served.unwrap();
    assert!(out.starts_with("HTTP/1.0 200") && out.ends_with("ok"), "{out}");

    // A path too long anyway says so.
    let long = std::path::PathBuf::from(format!("/tmp/{}/hub.sock", "x".repeat(120)));
    let err = botler_hub::serve::bind_socket(&long).unwrap_err().to_string();
    assert!(err.contains("more than the 107") && err.contains("XDG_RUNTIME_DIR"), "{err}");
}

/// Agents run pi behind the forwarder and talk to it; jobs get the socket for the SDK.
#[tokio::test]
async fn containers_get_the_socket_and_agents_the_forwarder() {
    let e = common::env().await;
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk").await.unwrap();
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("botler")
        .payload(json!({"text": "hi"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&agents::container_name(&e.hub, "botler")];
    assert_eq!(c.spec.command[..3], ["node", agents::FORWARDER, "pi"]);
    let socket = format!("{CONTAINER_RUN_DIR}/hub.sock");
    assert_eq!(c.spec.env["BOTLER_SOCKET"], socket);
    assert_eq!(c.spec.env["BOTLER_URL"], FORWARD_URL);
    assert!(
        matches!(mount(c, CONTAINER_RUN_DIR), Some(Mount::Bind { src, read_only: true, .. }) if *src == e.hub.paths.run_dir().unwrap())
    );
    assert!(mount(c, "/botler/forward").is_some());
    let forwarder = e.hub.paths.assets().join("forward/forward.mjs");
    assert!(std::fs::read_to_string(forwarder).unwrap().contains("127.0.0.1"));
    // Clones made when the hub was reached over TCP keep working.
    assert_eq!(c.spec.env["GIT_CONFIG_KEY_1"], format!("url.{FORWARD_URL}/.insteadOf"));
    let mcp = std::fs::read_to_string(e.hub.paths.agent_dir("botler").join("mcp.json")).unwrap();
    assert!(mcp.contains(&format!("{FORWARD_URL}/mcp")), "{mcp}");

    e.commit(
        &common::job("tick", "command = [\"python3\", \"-c\", \"pass\"]\n")
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect::<Vec<_>>(),
    )
    .await;
    let run = jobs::start(&e.hub, "tick", None, None).await.unwrap().unwrap();
    let j = &e.fake.containers()[&jobs::container_name(&e.hub, &run)];
    assert_eq!(j.spec.env["BOTLER_SOCKET"], socket);
    assert!(mount(j, CONTAINER_RUN_DIR).is_some());
}

/// Real Podman: a container with `--network none`, running the forwarder, reaches the hub's MCP
/// endpoint and git over the socket. Uses the agent base image (never builds it) and removes its
/// container afterwards.
#[tokio::test]
async fn podman_agent_without_network_reaches_mcp_and_git() {
    use botler_runtime::{ContainerSpec, Containers};
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = std::sync::Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    if rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap().is_none() {
        eprintln!("skipped: no agent base image");
        return;
    }
    let e = common::env_on(rt.clone(), botler_runtime::fake::Fake::new()).await;
    let listener = botler_hub::serve::bind_socket(&e.hub.paths.socket().unwrap()).unwrap();
    tokio::spawn(axum::serve(listener, botler_hub::api::router(e.hub.clone())).into_future());

    let instance = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    e.hub.agents.begin_wake(&instance, "w1").await;
    let token = e.hub.tokens.issue(
        &Principal::Agent { instance: instance.clone(), wake: "w1".into() },
        chrono::Duration::hours(1),
        Utc::now(),
    );
    let probe = r#"
init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"probe","version":"1"}}}'
curl -s -o /dev/null -w 'mcp %{http_code}\n' -X POST -H "Authorization: Bearer $BOTLER_TOKEN" \
  -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' --data "$init" "$BOTLER_URL/mcp"
git -c "http.extraHeader=Authorization: Bearer $BOTLER_TOKEN" ls-remote "$BOTLER_URL/git/monorepo.git" refs/heads/main \
  | sed 's/^/git /'
curl -s -o /dev/null --max-time 3 http://1.1.1.1/ && echo 'outside reachable' || echo 'outside unreachable'
"#;
    let name = format!("botler-{}-socket-probe", e.hub.id);
    let mut spec = ContainerSpec {
        name: name.clone(),
        image: botler_hub::assets::AGENT_BASE_LATEST.into(),
        command: vec!["node".into(), agents::FORWARDER.into(), "sh".into(), "-c".into(), probe.into()],
        labels: e.hub.labels("test"),
        network: botler_runtime::NetworkMode::None,
        ..Default::default()
    };
    spec.mounts = vec![
        Mount::Bind { src: e.hub.paths.run_dir().unwrap(), dst: CONTAINER_RUN_DIR.into(), read_only: true },
        Mount::Bind { src: e.hub.paths.assets().join("forward"), dst: "/botler/forward".into(), read_only: true },
    ];
    spec.env.insert("BOTLER_SOCKET".into(), format!("{CONTAINER_RUN_DIR}/hub.sock"));
    spec.env.insert("BOTLER_URL".into(), FORWARD_URL.into());
    spec.env.insert("BOTLER_TOKEN".into(), token);
    rt.run_detached(&spec).await.unwrap();
    while rt.inspect(&name).await.unwrap().is_some_and(|c| c.running) {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let logs = rt.logs(&name, 50).await.unwrap();
    rt.remove(&name).await.unwrap();

    let main = e.hub.repo.main().await.unwrap();
    assert!(logs.contains("mcp 200"), "{logs}");
    assert!(logs.contains(&format!("git {main}\trefs/heads/main")), "{logs}");
    assert!(logs.contains("outside unreachable"), "really without network: {logs}");
}
