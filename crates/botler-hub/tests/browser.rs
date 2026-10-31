//! Real Podman (`BOTLER_PODMAN_TESTS=1`; needs the agent base image, which it never builds): the
//! template's browser service (images/browser + jobs/browser-example), enabled on a throwaway hub
//! listening on port 7436 and pointed at a test page. noVNC answers on the published port with the
//! announced password only, CDP works by IP from a container on the services network, the bridge
//! emits polled and pushed items, and the login check flips when the logged-in marker goes.
//! Removes by exact name what it made: the service and probe containers, the network, the
//! `botler-job-browser-example` volume and the images labelled with its data directory.

mod common;

use std::sync::Arc;
use std::time::Duration;

use botler_hub::{images, services};
use botler_runtime::{ContainerSpec, Containers, NetworkMode};
use botler_store::{EventQuery, RunStatus};

const HOST_PORT: u16 = 16080;
const VOLUME: &str = "botler-job-browser-example";

const CONFIG: &str = r#"import urllib.parse

SITE = "test"
PAGE = """<html><body><h1>test</h1><div id="account">me</div><script>
setTimeout(() => botlerSignal(JSON.stringify({id: "pushed", text: "from the page", url: location.href})), 3000);
setTimeout(() => document.getElementById("account").remove(), 30000);
</script></body></html>"""
START_URL = "data:text/html," + urllib.parse.quote(PAGE)
LOGGED_IN_JS = "document.querySelector('#account') !== null"
POLL_JS: str | None = "[{id: 'polled', text: document.querySelector('h1').textContent, url: location.href}]"
PUSH_JS: str | None = None
POLL_SECONDS = 3.0
"#;

const PROBE: &str = r#"
import json, socket, sys, urllib.request
from Crypto.Cipher import DES
from websockets.sync.client import connect

ip = socket.gethostbyname("browser-example")
v = json.load(urllib.request.urlopen(f"http://{ip}:9222/json/version", timeout=10))
print("cdp", v["Browser"].split("/")[0])

def vnc(password):
    url = "ws://host.containers.internal:16080/websockify"
    with connect(url, subprotocols=["binary"], open_timeout=10) as ws:
        buf = b""
        def read(n):
            nonlocal buf
            while len(buf) < n:
                m = ws.recv(timeout=10)
                buf += m if isinstance(m, bytes) else m.encode()
            out, buf = buf[:n], buf[n:]
            return out
        ws.send(read(12))
        types = read(read(1)[0])
        if 2 not in types:
            return f"no password auth: {list(types)}"
        ws.send(bytes([2]))
        key = bytes(int(f"{b:08b}"[::-1], 2) for b in (password.encode() + bytes(8))[:8])
        ws.send(DES.new(key, DES.MODE_ECB).encrypt(read(16)))
        return "ok" if int.from_bytes(read(4), "big") == 0 else "denied"

print("novnc page", urllib.request.urlopen("http://host.containers.internal:16080/vnc.html", timeout=10).status)
print("vnc right", vnc(sys.argv[1]))
print("vnc wrong", vnc("wrong123"))
"#;

async fn wait_for(e: &common::Env, kind: &str, secs: u64) -> Vec<botler_core::Event> {
    let q = EventQuery { kind: Some(kind.into()), correlation: None, since: None, limit: 20 };
    for _ in 0..secs {
        let found = e.hub.store.events(&q).await.unwrap();
        if !found.is_empty() {
            return found;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn podman_browser_service_logs_in_bridges_and_notices_logout() {
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    if rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap().is_none() {
        eprintln!("skipped: no agent base image");
        return;
    }
    let config = botler_hub::config::HubConfig::new(([0, 0, 0, 0], 7436).into());
    let e = common::env_with(rt.clone(), botler_runtime::fake::Fake::new(), config).await;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7436").await.unwrap();
    tokio::spawn(axum::serve(listener, botler_hub::api::router(e.hub.clone())).into_future());
    tokio::spawn(botler_hub::supervisor::run(e.hub.clone()));

    let tpl = format!("{}/../../template/jobs/browser-example/job.toml", env!("CARGO_MANIFEST_DIR"));
    let job = std::fs::read_to_string(tpl)
        .unwrap()
        .replace("enabled = false", "enabled = true")
        .replace("host_port = 6080", &format!("host_port = {HOST_PORT}"));
    e.commit(&[
        ("jobs/browser-example/job.toml", &job),
        ("jobs/browser-example/src/browser_example/config.py", CONFIG),
    ])
    .await;
    // Builds images/browser, then the job on it; starts the service.
    services::reconcile(&e.hub).await.unwrap();
    let run = e.hub.store.runs_for("browser-example", 1).await.unwrap().remove(0);
    let svc = services::container_name(&e.hub, "browser-example");

    wait_for(&e, "example.page_item", 120).await;
    let logged_in = wait_for(&e, "example.logged_in", 30).await;
    let needed = wait_for(&e, "example.login_needed", 90).await;
    // By now (30 s after the page loaded) it has pushed its item too.
    let items = wait_for(&e, "example.page_item", 1).await;
    let password = needed.first().map(|ev| ev.payload["vnc_password"].as_str().unwrap().to_string());

    let mut probe_logs = String::new();
    if let Some(pw) = &password {
        let name = "tpl-browser-probe".to_string();
        let spec = ContainerSpec {
            name: name.clone(),
            image: botler_hub::assets::AGENT_BASE_LATEST.into(),
            command: [
                "uv",
                "run",
                "-q",
                "--no-project",
                "--with",
                "websockets==15.0.1",
                "--with",
                "pycryptodome==3.23.0",
                "python3",
                "-c",
                PROBE,
                pw,
            ]
            .map(String::from)
            .to_vec(),
            labels: e.hub.labels("test"),
            network: NetworkMode::Named(e.hub.network()),
            ..Default::default()
        };
        rt.run_detached(&spec).await.unwrap();
        while rt.inspect(&name).await.unwrap().is_some_and(|c| c.running) {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        probe_logs = rt.logs(&name, 100).await.unwrap();
        rt.remove(&name).await.unwrap();
    }
    let still = e.hub.store.run(&run.id).await.unwrap();
    let svc_logs = rt.logs(&svc, 200).await.unwrap_or_default();

    // Clean up by exact name before asserting.
    services::stop(&e.hub, &still, "test done").await.unwrap();
    let podman = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        tokio::process::Command::new(&rt.info().binary).args(args).output()
    };
    assert!(podman(&["network", "rm", &e.hub.network()]).await.unwrap().status.success());
    assert!(podman(&["volume", "rm", VOLUME]).await.unwrap().status.success());
    let owned = e.hub.labels("x").into_iter().filter(|(k, _)| k == images::OWNER_LABEL).collect();
    for t in rt.images(&owned).await.unwrap() {
        rt.remove_image(&t).await.unwrap();
    }

    eprintln!("service logs:\n{svc_logs}\nprobe:\n{probe_logs}");
    let ids: Vec<&str> = items.iter().filter_map(|ev| ev.payload["id"].as_str()).collect();
    assert!(ids.contains(&"polled") && ids.contains(&"pushed"), "{ids:?}");
    assert_eq!(items[0].provenance, botler_core::Provenance::External);
    assert_eq!(logged_in.len(), 1, "logged in first");
    assert_eq!(needed.len(), 1, "the marker went: login needed");
    assert_eq!(still.status, RunStatus::Running, "logged out is no failure: {still:?}");
    assert!(still.heartbeat_at.is_some());
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
    for line in ["cdp Chrome", "novnc page 200", "vnc right ok", "vnc wrong denied"] {
        assert!(probe_logs.contains(line), "{line}: {probe_logs}");
    }
}
