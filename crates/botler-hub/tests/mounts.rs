//! Network shares (`mounts/`): on the fake runtime, which container gets FUSE, the entrypoint and
//! the share's connection, on every wake and after a revoke, and how a failed mount is reported;
//! on real Podman (`BOTLER_PODMAN_TESTS=1`), a chat granted an SFTP share sees its files.

mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::ops::{self, Caller};
use botler_hub::{agents, grants};
use chrono::{Duration, Utc};
use common::{Env, env};
use serde_json::{Value, json};

fn secrets(group: &str) -> String {
    common::secrets_with(&format!(
        "[[group]]\nname = \"{group}\"\nclass = \"container\"\n[[group.secret]]\nname = \"NAS_HOST\"\n\
         [[group.secret]]\nname = \"NAS_PORT\"\n[[group.secret]]\nname = \"NAS_USER\"\n[[group.secret]]\nname = \"NAS_PASS\"\n"
    ))
}

const SHARE: &str = "type = \"sftp\"\nsecrets = \"nas\"\nremote_path = \"photos\"\npath = \"/mnt/nas-photos\"\n";

async fn setup(e: &Env, host: &str, port: &str, pass: &str) {
    e.commit(&[("secrets.toml", &secrets("nas")), ("mounts/nas-photos.toml", SHARE)]).await;
    for (k, v) in
        [("OPENROUTER_API_KEY", "sk"), ("NAS_HOST", host), ("NAS_PORT", port), ("NAS_USER", "u"), ("NAS_PASS", pass)]
    {
        e.hub.store.set_secret(&e.hub.secret_key, k, v).await.unwrap();
    }
}

fn user() -> Caller {
    Caller { principal: botler_hub::auth::Principal::User, capabilities: vec![], runnable: None }
}

async fn op(e: &Env, name: &str, args: Value) -> Value {
    ops::call(&e.hub, &user(), name, args).await.unwrap()
}

async fn run(e: &Env) {
    for _ in 0..4 {
        e.route().await;
        agents::dispatch_pending(&e.hub).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

async fn message(e: &Env, to: &str, text: &str) {
    let ev =
        Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now()).to(to).payload(json!({"text": text}));
    e.hub.emit(ev).await.unwrap();
}

async fn sleep_all(e: &Env) {
    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(3)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

fn spec(e: &Env, instance: &str) -> botler_runtime::ContainerSpec {
    e.fake.containers()[&agents::container_name(&e.hub, instance)].spec.clone()
}

/// The FUSE device, capability, entrypoint and share variables a container got.
fn shares(s: &botler_runtime::ContainerSpec) -> (bool, Option<String>) {
    let fuse = s.devices == ["/dev/fuse"] && s.caps == ["SYS_ADMIN"];
    let wrapped = s.command[..4] == ["sh", "/opt/botler/botler-mounts", "up", "--"]
        && s.command[4..7] == ["node", agents::FORWARDER, "pi"];
    assert_eq!(fuse, wrapped, "FUSE and the entrypoint come together: {s:?}");
    (fuse, s.env.get("RCLONE_CONFIG_NAS_PHOTOS_PASS").cloned())
}

#[tokio::test]
async fn a_granted_chat_gets_the_share_on_every_wake_until_revoked() {
    let e = env().await;
    setup(&e, "nas.local", "22", "hunter2").await;
    let out =
        op(&e, "spawn_agent", json!({"definition": "dev", "task": "sort photos", "env": {"mounts": ["nas-photos"]}}))
            .await;
    assert_eq!(out["granted"], json!(["mount:nas-photos"]));
    let dev = out["instance"].as_str().unwrap().to_string();
    let other = op(&e, "spawn_agent", json!({"definition": "dev", "task": "something else"})).await;
    let other = other["instance"].as_str().unwrap().to_string();
    run(&e).await;

    let s = spec(&e, &dev);
    assert_eq!(shares(&s), (true, Some("hunter2".into())), "applied on the first wake");
    assert_eq!(s.env["RCLONE_CONFIG_NAS_PHOTOS_TYPE"], "sftp");
    assert_eq!(s.env["RCLONE_CONFIG_NAS_PHOTOS_HOST"], "nas.local");
    let status = botler_hub::mounts::status_dir(&e.hub, &s.name);
    assert_eq!(
        std::fs::read_to_string(status.join("list")).unwrap(),
        "nas-photos|nas_photos|photos|/mnt/nas-photos|ro|\n"
    );
    assert!(s.mounts.iter().any(|m| matches!(m, botler_runtime::Mount::Bind { dst, .. } if dst == "/botler/mounts")));
    assert_eq!(shares(&spec(&e, &other)), (false, None), "a chat without the grant gets nothing");

    // Every wake gets it again. Read-only: nothing to drain before it sleeps.
    sleep_all(&e).await;
    assert!(e.fake.execs().is_empty(), "{:?}", e.fake.execs());
    message(&e, &dev, "again").await;
    run(&e).await;
    assert_eq!(shares(&spec(&e, &dev)), (true, Some("hunter2".into())), "and on the next");
    let prompts = e.fake.prompts(&s.name);
    assert!(!prompts.iter().any(|p| p.contains("unavailable")), "{prompts:?}");

    // Revoked: gone from the next wake on.
    let c = op(&e, "list_connections", json!({"chat": dev})).await;
    let grant = c["grants"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(c["grants"][0]["mode"], json!(null));
    op(&e, "revoke_access", json!({"grant": grant})).await;
    run(&e).await;
    assert_eq!(shares(&spec(&e, &dev)), (false, None), "gone after revoking");
}

#[tokio::test]
async fn a_share_that_fails_is_reported_not_fatal() {
    let e = env().await;
    setup(&e, "nas.local", "22", "wrong").await;
    e.fake.set_mount_failure("nas-photos", Some("Failed to lsjson: couldn't connect SSH: unable to authenticate"));
    let out = op(&e, "spawn_agent", json!({"definition": "dev", "task": "t", "env": {"mounts": ["nas-photos"]}})).await;
    let dev = out["instance"].as_str().unwrap().to_string();
    run(&e).await;

    assert!(e.hub.agents.get(&dev).await.is_some(), "the chat runs without the share");
    let name = agents::container_name(&e.hub, &dev);
    let notes = e.fake.notes(&name);
    let note = "mount nas-photos unavailable: Failed to lsjson: couldn't connect SSH: unable to authenticate";
    assert!(notes.iter().any(|n| n.contains(note)), "told with its first delivery: {notes:?}");
    let incidents = e.hub.store.open_incidents().await.unwrap();
    assert!(incidents.iter().any(|i| i.runnable == "mount:nas-photos" && i.kind == "mount"), "{incidents:?}");

    // A share whose secret has no value is reported the same way, without starting rclone.
    sleep_all(&e).await;
    e.fake.set_mount_failure("nas-photos", None);
    e.hub.store.delete_secret("NAS_PASS").await.unwrap();
    message(&e, &dev, "again").await;
    run(&e).await;
    let notes = e.fake.notes(&name);
    assert!(
        notes.iter().any(|n| n.contains("mount nas-photos unavailable: secret NAS_PASS has no value")),
        "{notes:?}"
    );
    assert!(
        !std::fs::read_to_string(botler_hub::mounts::status_dir(&e.hub, &name).join("list")).unwrap().contains("nas")
    );
}

#[tokio::test]
async fn rw_needs_a_writable_share_and_the_user() {
    let e = env().await;
    setup(&e, "nas.local", "22", "pw").await;
    let out = op(&e, "spawn_agent", json!({"definition": "dev", "task": "t"})).await;
    let dev = out["instance"].as_str().unwrap().to_string();
    let r: botler_core::defs::Resource = "mount:nas-photos".parse().unwrap();
    let rw = Some(botler_core::defs::MountMode::ReadWrite);
    let refused = grants::request(&e.hub, &dev, &r, "write back", rw).await;
    assert!(matches!(refused, Err(ops::OpError::BadRequest(m)) if m.contains("read_only = true")));
    e.commit(&[("mounts/nas-photos.toml", &format!("{SHARE}read_only = false\n"))]).await;
    // Held read-only is not held read-write: the user is asked.
    grants::give(&e.hub, &dev, &r, None, "user", false).await.unwrap();
    assert!(matches!(grants::request(&e.hub, &dev, &r, "x", None).await.unwrap(), grants::Requested::Held));
    assert!(matches!(grants::request(&e.hub, &dev, &r, "write back", rw).await.unwrap(), grants::Requested::Asked(_)));

    // Granted read-write: mounted so, and drained before it sleeps.
    grants::give(&e.hub, &dev, &r, rw, "user", false).await.unwrap();
    message(&e, &dev, "go").await;
    run(&e).await;
    let name = agents::container_name(&e.hub, &dev);
    let list = std::fs::read_to_string(botler_hub::mounts::status_dir(&e.hub, &name).join("list")).unwrap();
    assert_eq!(list, "nas-photos|nas_photos|photos|/mnt/nas-photos|rw|\n");
    sleep_all(&e).await;
    let down = ["sh", "/opt/botler/botler-mounts", "down"].map(String::from).to_vec();
    assert_eq!(e.fake.execs(), [(name, down)]);
}

#[tokio::test]
async fn a_service_mounts_its_shares_strictly() {
    let e = env().await;
    setup(&e, "nas.local", "22", "pw").await;
    let toml = "lifetime = \"service\"\ncommand = [\"watch\"]\nmounts = { nas-photos = \"ro\" }\n";
    let files = common::job("photo-watch", toml);
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    botler_hub::services::reconcile(&e.hub).await.unwrap();
    let c = &e.fake.containers()[&botler_hub::services::container_name(&e.hub, "photo-watch")];
    assert_eq!(c.spec.command, ["sh", "/opt/botler/botler-mounts", "run", "--", "watch"]);
    assert_eq!(
        (c.spec.devices.as_slice(), c.spec.caps.as_slice()),
        (&["/dev/fuse".to_string()][..], &["SYS_ADMIN".to_string()][..])
    );
    assert_eq!(c.spec.env["RCLONE_CONFIG_NAS_PHOTOS_USER"], "u");
}

/// Real Podman (`BOTLER_PODMAN_TESTS=1`): an SFTP server (`rclone serve sftp`, published on a free
/// host port) and a `chat` granted the share. The chat sees the files after a wake and again after
/// sleeping and waking; with a wrong password it still runs and the share is reported as failed.
/// Needs the current agent base as `latest` (never built here); removes the agent image and every
/// container it started afterwards. No model is called.
#[tokio::test]
async fn podman_a_granted_chat_sees_the_share() {
    use botler_runtime::{ContainerSpec, Containers, Mount, PortMapping};
    use std::sync::Arc;
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    // The hub's API, as `botler serve` listens, so pi in the container reaches it.
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let config = botler_hub::config::HubConfig::new(listener.local_addr().unwrap());
    let e = common::env_with(rt.clone(), botler_runtime::fake::Fake::new(), config).await;
    tokio::spawn(axum::serve(listener, botler_hub::api::router(e.hub.clone())).into_future());
    // `latest` is shared with every hub on this host, so this test never builds or moves it.
    let base = botler_hub::assets::agent_base_tag();
    let current = rt.image_id(&base).await.unwrap();
    if current.is_none() || current != rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap() {
        eprintln!("skipped: {base} (with rclone) is not built or not `latest`; start the hub once to build it");
        return;
    }
    let host_fuse = || std::fs::read_to_string("/proc/self/mounts").unwrap().matches("fuse.rclone").count();
    let fuse_before = host_fuse();

    let data = e.hub.paths.root.join("sftp-data");
    std::fs::create_dir_all(data.join("photos")).unwrap();
    std::fs::write(data.join("photos/hello.txt"), "hello from the share\n").unwrap();
    let port = 20000 + (std::process::id() % 20000) as u16;
    let server = format!("botler-{}-sftp", e.hub.id);
    let pass = format!("pw-{}", uuid::Uuid::now_v7().simple());
    rt.run_detached(&ContainerSpec {
        name: server.clone(),
        image: "docker.io/rclone/rclone:1.75.2".into(),
        command: ["serve", "sftp", "/data", "--addr", ":2022", "--user", "u", "--pass", &pass]
            .map(String::from)
            .to_vec(),
        mounts: vec![Mount::Bind { src: data, dst: "/data".into(), read_only: false }],
        ports: vec![PortMapping { bind: "0.0.0.0".into(), host_port: port, port: 2022 }],
        labels: e.hub.labels("test"),
        remove_on_exit: true,
        ..Default::default()
    })
    .await
    .unwrap();

    let outcome = async {
        e.commit(&[("images/agent/Dockerfile", &format!("FROM {base}\n"))]).await;
        setup(&e, "host.containers.internal", &port.to_string(), &pass).await;
        let out =
            op(&e, "spawn_agent", json!({"definition": "chat", "task": "t", "env": {"mounts": ["nas-photos"]}})).await;
        let chat = out["instance"].as_str().unwrap().to_string();
        let container = agents::container_name(&e.hub, &chat);
        let cat = ["cat", "/mnt/nas-photos/hello.txt"].map(String::from);
        let mut seen = Vec::new();
        for round in 0..3 {
            if round == 2 {
                e.hub.store.set_secret(&e.hub.secret_key, "NAS_PASS", "wrong").await.unwrap();
            }
            let inst = e.hub.store.instance(&chat).await.unwrap();
            agents::ensure_awake(&e.hub, &inst).await.unwrap();
            let state = botler_hub::mounts::status_dir(&e.hub, &container).join("nas-photos.state");
            for _ in 0..600 {
                if state.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            let failed = botler_hub::mounts::failures(&e.hub, &container, &env_of(&e, &chat).await).await.unwrap();
            let read = rt.exec(&container, &cat).await.map_err(|err| err.to_string());
            seen.push((read, failed));
            // Nothing is delivered (no model is called), so nothing may keep it awake.
            for d in e.hub.store.pending_deliveries(&chat).await.unwrap() {
                e.hub.store.drop_delivery(&d.id, "test").await.unwrap();
            }
            sleep_all(&e).await;
            while rt.inspect(&container).await.unwrap().is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
        seen
    }
    .await;

    // Started with --rm: stopping removes it.
    match rt.stop(&server, 2).await {
        Ok(()) | Err(botler_runtime::ContainerError::NotFound(_)) => {}
        Err(err) => panic!("stopping {server}: {err}"),
    }
    let owned = e.hub.labels("x").into_iter().filter(|(k, _)| k == botler_hub::images::OWNER_LABEL).collect();
    for t in rt.images(&owned).await.unwrap() {
        rt.remove_image(&t).await.unwrap();
    }

    assert_eq!(outcome[0].0.as_deref(), Ok("hello from the share\n"), "{outcome:?}");
    assert!(outcome[0].1.is_empty(), "{outcome:?}");
    assert_eq!(outcome[1].0.as_deref(), Ok("hello from the share\n"), "after sleeping and waking: {outcome:?}");
    assert!(outcome[2].0.is_err(), "wrong password: no share: {outcome:?}");
    let (name, why) = &outcome[2].1[0];
    assert_eq!(name, "nas-photos");
    assert!(why.contains("unable to authenticate"), "{why}");
    assert_eq!(host_fuse(), fuse_before, "no rclone mount leaked onto the host");
}

async fn env_of(e: &Env, chat: &str) -> botler_core::defs::Environment {
    let inst = e.hub.store.instance(chat).await.unwrap();
    let applied = e.hub.applied().await;
    let def = &applied.defs.agents[&inst.definition];
    botler_hub::agent_env::compute(&e.hub, &inst, def, &applied, Some(String::new())).await.unwrap().env
}
