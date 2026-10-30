//! Real Podman: `BOTLER_PODMAN_TESTS=1`. Containers set up by `localtime::apply` tell the host's
//! local time with `date`, Python and Node, also in images without tzdata (alpine).

use std::collections::BTreeMap;
use std::path::Path;

use botler_hub::localtime;
use botler_runtime::{Cli, ContainerSpec, Containers, NetworkMode};
use chrono::Utc;
use tokio::io::AsyncReadExt;

const SCRIPT: &str = "echo date $(date +%z); \
    command -v python3 >/dev/null && python3 -c 'import datetime;print(\"python\", datetime.datetime.now().astimezone().strftime(\"%z\"))'; \
    command -v node >/dev/null && node -e 'const m=-new Date().getTimezoneOffset(),a=Math.abs(m);console.log(\"node \"+(m<0?\"-\":\"+\")+String(Math.floor(a/60)).padStart(2,\"0\")+String(a%60).padStart(2,\"0\"))'; true";

#[tokio::test]
async fn containers_tell_local_time() {
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = Cli::detect(None).await.unwrap();
    let tz = localtime::host();
    let want = Utc::now().with_timezone(&tz).format("%z").to_string();
    for (image, tools) in [
        ("localhost/botler/agent-base:latest", &["date", "python", "node"][..]),
        ("docker.io/library/alpine:3", &["date"]),
    ] {
        let (mut env, mut mounts) = (BTreeMap::new(), Vec::new());
        localtime::apply(tz, Path::new(localtime::ZONEINFO), &mut env, &mut mounts);
        let spec = ContainerSpec {
            name: format!("botler-test-{}-tz", std::process::id()),
            image: image.into(),
            command: vec!["sh".into(), "-c".into(), SCRIPT.into()],
            env,
            mounts,
            network: NetworkMode::None,
            remove_on_exit: true,
            ..Default::default()
        };
        let mut att = rt.run_attached(&spec).await.unwrap();
        let mut out = String::new();
        att.stdout.read_to_string(&mut out).await.unwrap();
        for tool in tools {
            assert!(out.lines().any(|l| l == format!("{tool} {want}")), "{image}: {tool} not at {want} ({tz}):\n{out}");
        }
    }
}
