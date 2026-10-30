//! Real container runtime tests. Run with `BOTLER_PODMAN_TESTS=1 cargo test -p botler-runtime`.

use std::collections::BTreeMap;
use std::time::Duration;

use botler_runtime::{Cli, ContainerSpec, Containers, NetworkMode};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const IMAGE: &str = "docker.io/library/alpine:3";

fn enabled() -> bool {
    std::env::var("BOTLER_PODMAN_TESTS").is_ok_and(|v| v == "1")
}

fn name(suffix: &str) -> String {
    format!("botler-test-{}-{suffix}", std::process::id())
}

#[tokio::test]
async fn detached_lifecycle_events_and_logs() {
    if !enabled() {
        return;
    }
    let rt = Cli::detect(None).await.unwrap();
    let n = name("detached");
    let mut events = rt.events().await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await; // let the event stream attach
    let spec = ContainerSpec {
        name: n.clone(),
        image: IMAGE.into(),
        command: vec!["sh".into(), "-c".into(), "echo hello-from-job; exit 3".into()],
        labels: BTreeMap::from([("botler.role".into(), "job".into())]),
        network: NetworkMode::None,
        ..Default::default()
    };
    rt.run_detached(&spec).await.unwrap();
    let died = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let ev = events.recv().await.expect("event stream ended");
            if ev.name == n && ev.status == "died" {
                return ev;
            }
        }
    })
    .await
    .expect("no died event");
    assert_eq!(died.exit_code, Some(3));

    let info = rt.inspect(&n).await.unwrap().unwrap();
    assert!(!info.running);
    assert_eq!(info.exit_code, Some(3));
    assert_eq!(info.labels.get("botler.role").map(String::as_str), Some("job"));
    let listed = rt.list(&BTreeMap::from([("botler.role".into(), "job".into())])).await.unwrap();
    assert!(listed.iter().any(|c| c.name == n));
    assert!(rt.logs(&n, 10).await.unwrap().contains("hello-from-job"));
    rt.remove(&n).await.unwrap();
    assert!(rt.inspect(&n).await.unwrap().is_none());
    rt.remove(&n).await.unwrap(); // idempotent
}

#[tokio::test]
async fn attached_stdio_roundtrip() {
    if !enabled() {
        return;
    }
    let rt = Cli::detect(None).await.unwrap();
    let spec = ContainerSpec {
        name: name("attached"),
        image: IMAGE.into(),
        command: vec!["sh".into(), "-c".into(), "read line; echo got:$line".into()],
        remove_on_exit: true,
        ..Default::default()
    };
    let mut att = rt.run_attached(&spec).await.unwrap();
    att.stdin.write_all(b"ping\n").await.unwrap();
    att.stdin.flush().await.unwrap();
    let mut lines = BufReader::new(att.stdout).lines();
    let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await.unwrap().unwrap();
    assert_eq!(line.as_deref(), Some("got:ping"));
    let code = tokio::time::timeout(Duration::from_secs(30), att.exited).await.unwrap().unwrap();
    assert_eq!(code, Some(0));
}
