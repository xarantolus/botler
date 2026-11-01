mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::{agents, router};
use botler_store::InstanceState;
use chrono::{Duration, Utc};
use common::env;
use serde_json::{Value, json};

async fn with_key(e: &common::Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

async fn settle() {
    // let the pump task process the fake pi's events
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn message_wakes_botler_and_is_delivered_once() {
    let e = env().await;
    with_key(&e).await;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("botler")
        .payload(json!({"text": "hello"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let name = agents::container_name(&e.hub, "botler");
    let c = &e.fake.containers()[&name];
    let cmd = c.spec.command.join(" ");
    assert!(cmd.contains("--mode rpc") && cmd.contains("--session /botler/sessions/botler.jsonl"), "{cmd}");
    assert!(cmd.contains("--model openrouter/deepseek/deepseek-v4.1-flash"), "{cmd}");
    assert_eq!(c.spec.env["OPENROUTER_API_KEY"], "sk-test");
    assert_eq!(c.spec.env["PI_OFFLINE"], "1");
    assert!(
        c.spec.mounts.iter().any(|m| matches!(m, botler_runtime::Mount::Tmpfs { dst } if dst == "/botler/agent-dir"))
    );
    let models = std::fs::read_to_string(e.hub.paths.agent_dir("botler").join("models.json")).unwrap();
    assert!(models.contains("\"$OPENROUTER_API_KEY\"") && models.contains("16384"), "{models}");
    assert!(!models.contains("null"), "pi ignores models.json containing nulls: {models}");
    assert!(!models.contains("sk-test"), "secret values never land in rendered files");
    // Turn-ending ops: registered by the extension from the hub's own schemas, hidden over MCP.
    let read_json = |f: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(e.hub.paths.agent_dir("botler").join(f)).unwrap()).unwrap()
    };
    let turn = read_json("turn-tools.json");
    let names: Vec<&str> = turn.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "propose_change",
            "trial_run",
            "replay_event",
            "request_secrets",
            "request_access",
            "handover_ready",
            "ask_user",
            "request_visit",
            "invoke_action"
        ],
        "botler is no reviewer"
    );
    let ask = turn.as_array().unwrap().iter().find(|t| t["name"] == "ask_user").unwrap();
    assert_eq!(ask["parameters"]["required"], json!(["question"]), "{ask}");
    let hidden = &read_json("mcp.json")["mcpServers"]["botler"]["toolExposure"];
    for op in botler_hub::ops::turn_ending() {
        assert_eq!(hidden[op], "hidden", "{hidden}");
    }
    assert_eq!(hidden["attach_files"], "direct", "files it shows are in the transcript at its call");
    assert!(e.hub.paths.workspace("botler").join(".git").exists());

    let prompts = e.fake.prompts(&name);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].starts_with("botler:delivery ") && prompts[0].contains("hello"));
    // Local time in the instance's zone, which follows the host when botler.toml names none.
    let tz = e.hub.applied().await.defs.timezone;
    assert_eq!(tz, botler_hub::localtime::host());
    let env: serde_json::Value = serde_json::from_str(prompts[0].trim_start_matches("botler:delivery ")).unwrap();
    assert!(env["at"].as_str().unwrap().ends_with(&format!(" {}", tz.name())), "{env}");
    assert!(env.get("gap").is_none(), "first delivery has no gap: {env}");
    // The container runs in the same zone.
    assert_eq!(c.spec.env["TZ"], tz.name());
    if std::path::Path::new(botler_hub::localtime::ZONEINFO).join(tz.name()).is_file() {
        for dst in ["/etc/localtime".to_string(), format!("/usr/share/zoneinfo/{}", tz.name())] {
            assert!(
                c.spec
                    .mounts
                    .iter()
                    .any(|m| matches!(m, botler_runtime::Mount::Bind { dst: d, read_only: true, .. } if *d == dst)),
                "{dst} not mounted"
            );
        }
    }
    assert!(e.hub.store.pending_deliveries("botler").await.unwrap().is_empty());

    // A redelivery of the same delivery id (crash between prompt and mark_delivered) is dropped
    // by the extension, so the agent sees it once.
    let ev = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some("message".into()), limit: 1, ..Default::default() })
        .await
        .unwrap()
        .remove(0);
    let first_id: String = serde_json::from_str::<serde_json::Value>(prompts[0].trim_start_matches("botler:delivery "))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .into();
    let delivery = e.hub.store.delivery(&first_id).await.unwrap();
    let agent = e.hub.agents.get("botler").await.unwrap();
    let now = Utc::now();
    let text = agents::envelope(&delivery, &ev, tz, now, Some(now - Duration::days(3)), &[]).unwrap();
    assert!(text.contains("\"gap\":\"first message in 3 days\""), "{text}");
    let again = agent.pi.prompt(&text, &[], None).await.unwrap();
    assert_eq!(again, botler_runtime::Disposition::Handled);
    assert_eq!(e.fake.prompts(&name).len(), 1);
    settle().await;
    assert_eq!(e.hub.store.instance("botler").await.unwrap().state, InstanceState::Running);
}

#[tokio::test]
async fn idle_chats_sleep_and_always_on_botler_stays() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("botler"), Some("build a thing"), None, None).await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("botler")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert!(e.fake.prompts(&agents::container_name(&e.hub, &child))[0].contains("build a thing"));

    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(2)).await.unwrap();
    settle().await;
    // `botler` is always_on in the template, so it is never reaped.
    assert_eq!(e.hub.store.instance("botler").await.unwrap().state, InstanceState::Running);
    assert_eq!(
        e.hub.store.instance(&child).await.unwrap().state,
        InstanceState::Dormant,
        "idle chats sleep, never end"
    );
    assert_eq!(e.hub.agents.all().await.len(), 1);
}

#[tokio::test]
async fn missing_secret_defers_delivery() {
    let e = env().await;
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("botler")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.pending_deliveries("botler").await.unwrap().len(), 1, "waits until the key is entered");
    with_key(&e).await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert!(e.hub.store.pending_deliveries("botler").await.unwrap().is_empty());
}

#[tokio::test]
async fn always_on_botler_starts_without_a_message() {
    let e = env().await;
    with_key(&e).await;
    agents::keep_always_on(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.instance("botler").await.unwrap().state, InstanceState::Running);
    assert!(e.fake.containers().contains_key(&agents::container_name(&e.hub, "botler")));
    assert!(e.fake.prompts(&agents::container_name(&e.hub, "botler")).is_empty(), "no prompt needed to be up");
}

#[tokio::test]
async fn crashing_always_on_agent_backs_off_and_opens_an_incident() {
    let e = env().await;
    with_key(&e).await;
    e.fake.set_crash_on_start(true);
    let mut starts = 0;
    for _ in 0..20 {
        if agents::keep_always_on(&e.hub).await.is_ok() && e.hub.agents.get("botler").await.is_some() {
            starts += 1;
        }
        settle().await;
    }
    assert!(starts <= 2, "backoff must stop a hot restart loop (started {starts} times)");
}

#[tokio::test]
async fn emit_event_validates_and_explains() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller, OpError};
    let e = env().await;
    e.commit(&[(
        "events/mail.toml",
        "[[event]]\ntype = \"mail.received\"\nschema = { type = \"object\", required = [\"subject\"], properties = { subject = { type = \"string\" } } }\n",
    )])
    .await;
    let caller = Caller::resolve(&e.hub, Principal::User).await.unwrap();
    let ok = ops::call(&e.hub, &caller, "emit_event", json!({"type": "mail.received", "payload": {"subject": "hi"}}))
        .await
        .unwrap();
    assert!(ok.get("event_id").is_some());
    match ops::call(&e.hub, &caller, "emit_event", json!({"type": "mail.received", "payload": {"subject": 5}})).await {
        Err(OpError::InvalidEvent(msg)) => {
            assert!(msg.contains("`/subject`") && msg.contains("Expected schema"), "{msg}")
        }
        other => panic!("{other:?}"),
    }
    match ops::call(&e.hub, &caller, "emit_event", json!({"type": "mail.recieved", "payload": {}})).await {
        Err(OpError::InvalidEvent(msg)) => assert!(msg.contains("Did you mean: `mail.received`"), "{msg}"),
        other => panic!("{other:?}"),
    }
    let types = ops::call(&e.hub, &caller, "list_event_types", json!({})).await.unwrap();
    let declared: Vec<&Value> = types["types"].as_array().unwrap().iter().filter(|t| t["builtin"] == false).collect();
    assert!(declared.iter().any(|t| t["type"] == "mail.received"), "{declared:?}");
    assert!(declared.iter().any(|t| t["type"] == "example.greeting"), "pydantic events (events.manifest.json) too");
}

#[tokio::test]
async fn sleeping_chat_is_woken_by_a_message_with_its_workspace() {
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("botler"), Some("start"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let marker = e.hub.paths.workspace(&chat).join("notes.txt");
    std::fs::write(&marker, "work in progress").unwrap();
    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(2)).await.unwrap();
    settle().await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);

    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to(chat.clone())
                .payload(json!({"text": "continue"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running);
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "work in progress", "workspace survives sleep");
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, &chat));
    assert!(prompts.last().unwrap().contains("continue"));
}

#[tokio::test]
async fn archive_hands_over_and_messaging_restores() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller, OpError};
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("botler"), Some("build x"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let wake = e.hub.agents.get(&chat).await.unwrap().wake.clone();
    let me = Caller::resolve(&e.hub, Principal::Agent { instance: chat.clone(), wake }).await.unwrap();
    let report = json!({
        "summary": "Built half of x on branch agents/dev/work; tests pass for the parser.",
        "outstanding": ["wire x into the scheduler"],
        "state": ["branch agents/<me>/work", "notes in memory: x-design"],
        "handoff": { "spawn": "dev", "message": "Wire x into the scheduler; see branch agents/<me>/work." }
    });
    assert!(
        matches!(ops::call(&e.hub, &me, "handover_ready", report.clone()).await, Err(OpError::BadRequest(_))),
        "only after a request"
    );

    e.hub.store.request_archive(&chat).await.unwrap();
    let out = ops::call(&e.hub, &me, "handover_ready", report).await.unwrap();
    let successor = out["handed_to"].as_str().unwrap().to_string();
    assert!(successor.starts_with("dev-") && successor != chat);
    let archived = e.hub.store.instance(&chat).await.unwrap();
    assert_eq!(archived.state, InstanceState::Archived);
    assert_eq!(
        archived.archive_report.as_ref().unwrap().report.as_ref().unwrap().outstanding[0],
        "wire x into the scheduler"
    );
    assert_eq!(e.hub.store.pending_deliveries(&successor).await.unwrap().len(), 1, "successor got the handoff");
    assert!(e.hub.paths.workspace(&chat).exists(), "nothing is deleted");

    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to(chat.clone())
                .payload(json!({"text": "one more thing"})),
        )
        .await
        .unwrap();
    e.route().await;
    assert_ne!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Archived, "messaging restores");
}

fn bind_src(c: &botler_runtime::fake::FakeContainer, dst: &str) -> Option<std::path::PathBuf> {
    c.spec.mounts.iter().find_map(|m| match m {
        botler_runtime::Mount::Bind { src, dst: d, .. } if d == dst => Some(src.clone()),
        _ => None,
    })
}

async fn message(e: &common::Env, to: &str, text: &str) {
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to(to.to_string())
                .payload(json!({"text": text})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
}

async fn sleep_all(e: &common::Env) {
    for a in e.hub.agents.all().await {
        e.hub.store.set_instance_state(&a.instance, InstanceState::Dormant, None).await.unwrap();
    }
    agents::stop_all(&e.hub).await;
    settle().await;
}

#[tokio::test]
async fn secret_groups_and_shared_homes_reach_the_container() {
    use botler_runtime::Mount;
    let e = env().await;
    with_key(&e).await;
    let secrets = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/secrets.toml")).unwrap()
        + "\n[[group]]\nname = \"station\"\nclass = \"container\"\n[[group.secret]]\nname = \"STATION_KEY\"\nfile = \"/home/agent/.ssh/id_ed25519\"\n[[group.secret]]\nname = \"STATION_HOST\"\n";
    e.commit(&[
        ("secrets.toml", secrets.as_str()),
        ("agents/station/agent.toml", "kind = \"persistent\"\nsecrets = [\"station\"]\nhome = \"groundstation\"\n"),
        ("agents/station/AGENTS.md", "station agent"),
        ("agents/antenna/agent.toml", "workspace = \"none\"\nhome = \"groundstation\"\n"),
        ("agents/antenna/AGENTS.md", "antenna agent"),
    ])
    .await;
    e.hub.store.set_secret(&e.hub.secret_key, "STATION_KEY", "-----BEGIN KEY-----").await.unwrap();
    e.hub.store.set_secret(&e.hub.secret_key, "STATION_HOST", "station.lan").await.unwrap();
    message(&e, "station", "hi").await;

    let c = &e.fake.containers()[&agents::container_name(&e.hub, "station")];
    assert_eq!(c.spec.env["STATION_HOST"], "station.lan");
    assert!(!c.spec.env.contains_key("STATION_KEY"), "file secrets are not env vars");
    let key_mount = c.spec.mounts.iter().find_map(|m| match m {
        Mount::Bind { src, dst, read_only } if dst == "/home/agent/.ssh/id_ed25519" => Some((src.clone(), *read_only)),
        _ => None,
    });
    let (src, ro) = key_mount.expect("key mounted");
    assert!(ro);
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "-----BEGIN KEY-----");
    let shared = e.hub.paths.shared_home("groundstation");
    assert_eq!(bind_src(c, "/home/agent").unwrap(), shared);

    // Two instances of another definition naming the same home share it too.
    let a = router::spawn(&e.hub, "antenna", None, Some("align"), None, None).await.unwrap();
    let b = router::spawn(&e.hub, "antenna", None, Some("align again"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    for i in [&a, &b] {
        let c = &e.fake.containers()[&agents::container_name(&e.hub, i)];
        assert_eq!(bind_src(c, "/home/agent").unwrap(), shared, "{i}");
        assert!(bind_src(c, "/home/agent/monorepo").is_none(), "workspace = none has no clone");
    }
}

#[tokio::test]
async fn home_persists_across_wakes_and_archiving() {
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("botler"), Some("clone the repo"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let name = agents::container_name(&e.hub, &chat);
    let c = &e.fake.containers()[&name];
    let home = e.hub.paths.home(&chat);
    assert_eq!(bind_src(c, "/home/agent").unwrap(), home);
    assert_eq!(bind_src(c, "/home/agent/monorepo").unwrap(), e.hub.paths.workspace(&chat));
    assert_eq!(c.spec.env["HOME"], "/home/agent");
    assert_eq!(c.spec.workdir.as_deref(), Some("/home/agent"));
    assert!(
        !c.spec
            .mounts
            .iter()
            .any(|m| matches!(m, botler_runtime::Mount::Bind { dst, .. } if dst == "/scratch" || dst == "/workspace"))
    );
    std::fs::create_dir_all(home.join("groundstation")).unwrap();
    std::fs::write(home.join("groundstation/README"), "cloned").unwrap();

    // Sleep, wake: same host directory, contents intact.
    sleep_all(&e).await;
    message(&e, &chat, "continue").await;
    let c = &e.fake.containers()[&name];
    assert_eq!(bind_src(c, "/home/agent").unwrap(), home);
    assert_eq!(std::fs::read_to_string(home.join("groundstation/README")).unwrap(), "cloned");

    // Archive, then message it: restored with the same home.
    sleep_all(&e).await;
    let forced = botler_store::ArchiveRecord {
        confirmed_by_agent: false,
        forced: true,
        reason: None,
        report: None,
        handed_to: None,
        at: Utc::now(),
        freed_bytes: None,
    };
    e.hub.store.archive_instance(&chat, &forced).await.unwrap();
    message(&e, &chat, "one more thing").await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running);
    let c = &e.fake.containers()[&name];
    assert_eq!(bind_src(c, "/home/agent").unwrap(), home);
    assert!(home.join("groundstation/README").exists());
}

#[tokio::test]
async fn old_sessions_and_private_volumes_move_to_the_home() {
    let e = env().await;
    with_key(&e).await;
    let chat = router::new_instance(&e.hub, "chat", None, None).await.unwrap();
    // A chat from before homes: a private /workspace volume and a session started in /workspace.
    let legacy = e.hub.paths.workspace(&chat);
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("draft.md"), "draft").unwrap();
    let sessions = e.hub.paths.sessions(&chat);
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{chat}.jsonl")),
        "{\"type\":\"session\",\"version\":3,\"cwd\":\"/workspace\"}\n",
    )
    .unwrap();
    message(&e, &chat, "hello again").await;
    assert_eq!(std::fs::read_to_string(e.hub.paths.home(&chat).join("workspace/draft.md")).unwrap(), "draft");
    assert!(!legacy.exists());
    let header = std::fs::read_to_string(sessions.join(format!("{chat}.jsonl"))).unwrap();
    assert!(header.contains("\"cwd\":\"/home/agent\""), "{header}");
}

#[tokio::test]
async fn parent_hears_when_a_spawned_chat_finishes_its_turn() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("botler"), Some("look into x"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    // The pump task records the end of the turn in its own time: poll for the notice.
    let query = botler_store::EventQuery { kind: Some(kinds::AGENT_IDLE.into()), limit: 5, ..Default::default() };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let idle = loop {
        e.route().await;
        let idle = e.hub.store.events(&query).await.unwrap();
        if !idle.is_empty() {
            break idle;
        }
        assert!(std::time::Instant::now() < deadline, "no idle notice within 10s");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    assert_eq!(idle.len(), 1, "one turn, one idle notice");
    assert_eq!(idle[0].to.as_deref(), Some("botler"));
    assert_eq!(idle[0].payload["instance"], child.as_str());
    assert_eq!(idle[0].payload["last_reply"], "fake reply");
    assert!(idle[0].correlation.is_some(), "chained to the delivery that started the turn");
    assert!(e.hub.agents.phase(&child).await.is_none(), "no phase once running");
}

#[tokio::test]
async fn secret_requests_are_async_and_declare_on_submit() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller, OpError};
    use botler_hub::secret_requests::{InputError, Submission, submit};
    let e = env().await;
    with_key(&e).await;
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("botler")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let wake = e.hub.agents.get("botler").await.unwrap().wake.clone();
    let front = Caller::resolve(&e.hub, Principal::Agent { instance: "botler".into(), wake }).await.unwrap();

    // Invalid requests are refused immediately with a reason.
    let bad =
        ops::call(&e.hub, &front, "request_secrets", json!({"group": "gitlab", "reason": "x", "fill": ["NOPE"]})).await;
    assert!(matches!(bad, Err(OpError::BadRequest(_))), "{bad:?}");

    let r = ops::call(
        &e.hub,
        &front,
        "request_secrets",
        json!({
            "group": "gitlab", "reason": "Deploy the station repo", "description": "GitLab access",
            "add": [{"name": "GITLAB_TOKEN_STATION", "description": "Project token with write_repository"},
                    {"name": "GITLAB_SSH_KEY", "description": "Deploy key", "file": "/root/.ssh/id_ed25519"}]
        }),
    )
    .await
    .unwrap();
    let id = r["request_id"].as_str().unwrap().to_string();
    let listed = ops::call(&e.hub, &front, "list_requests", json!({})).await.unwrap();
    assert_eq!(listed["requests"][0]["id"], id.as_str());
    assert!(
        !e.hub.applied().await.defs.secret_groups.contains_key("gitlab"),
        "nothing is declared before the user answers"
    );

    let missing = submit(
        &e.hub,
        &id,
        Submission { values: [("GITLAB_TOKEN_STATION".into(), "glpat".into())].into(), decline: false },
    )
    .await;
    assert!(missing.is_err(), "every field needs a value");
    // Binary values (uploaded files) are only accepted for file secrets.
    let binary = botler_hub::secret_requests::SecretInput::Bytes { base64: "AJ+Slv8=".into() };
    let env_binary = submit(
        &e.hub,
        &id,
        Submission {
            values: [("GITLAB_TOKEN_STATION".into(), binary.clone()), ("GITLAB_SSH_KEY".into(), "KEY".into())].into(),
            decline: false,
        },
    )
    .await;
    assert!(
        matches!(env_binary, Err(botler_hub::secret_requests::RequestError::Input(InputError::BinaryEnv(_)))),
        "{env_binary:?}"
    );
    let out = submit(
        &e.hub,
        &id,
        Submission {
            values: [("GITLAB_TOKEN_STATION".into(), "glpat".into()), ("GITLAB_SSH_KEY".into(), binary)].into(),
            decline: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(out.added.len(), 2);
    assert_eq!(
        e.hub.store.secret_bytes(&e.hub.secret_key, "GITLAB_SSH_KEY").await.unwrap().as_deref(),
        Some(&[0u8, 159, 146, 150, 255][..]),
        "file secrets are stored byte-exact"
    );
    let defs = &e.hub.applied().await.defs;
    assert_eq!(defs.secret_groups["gitlab"].secrets.len(), 2, "declared and applied");
    assert_eq!(e.hub.store.secret(&e.hub.secret_key, "GITLAB_TOKEN_STATION").await.unwrap().as_deref(), Some("glpat"));
    let toml = e.hub.repo.read_file(&e.hub.repo.main().await.unwrap(), "secrets.toml").await.unwrap().unwrap();
    assert!(toml.contains("# Secret declarations"), "comments preserved: {toml}");

    // The agent hears back with names, never values.
    e.route().await;
    let answered = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some("dialog.answered".into()), limit: 1, ..Default::default() })
        .await
        .unwrap();
    let text = answered[0].payload.to_string();
    assert!(text.contains("GITLAB_SSH_KEY") && !text.contains("glpat") && !text.contains("\"KEY\""), "{text}");

    // A request answered on the Secrets page instead closes once every field it asks for has a
    // new value; the old value does not count.
    let fill = ops::call(
        &e.hub,
        &front,
        "request_secrets",
        json!({"group": "gitlab", "reason": "rotate the token", "fill": ["GITLAB_TOKEN_STATION"]}),
    )
    .await
    .unwrap();
    let fill = fill["request_id"].as_str().unwrap().to_string();
    botler_hub::secret_requests::resolve_satisfied(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.dialog(&fill).await.unwrap().status, botler_store::DialogStatus::Open);
    e.hub.store.set_secret(&e.hub.secret_key, "GITLAB_TOKEN_STATION", "glpat2").await.unwrap();
    botler_hub::secret_requests::resolve_satisfied(&e.hub).await.unwrap();
    let closed = e.hub.store.dialog(&fill).await.unwrap();
    assert_eq!((closed.status, closed.answer.as_deref()), (botler_store::DialogStatus::Answered, Some("provided")));

    // Remove one field later; cancel works and blocks late answers.
    let r2 = ops::call(
        &e.hub,
        &front,
        "request_secrets",
        json!({"group": "gitlab", "reason": "rotate", "remove": ["GITLAB_SSH_KEY"]}),
    )
    .await
    .unwrap();
    let id2 = r2["request_id"].as_str().unwrap().to_string();
    ops::call(&e.hub, &front, "cancel_request", json!({"id": id2, "reason": "not needed"})).await.unwrap();
    assert!(submit(&e.hub, &id2, Submission { values: Default::default(), decline: false }).await.is_err());
    let r3 = ops::call(
        &e.hub,
        &front,
        "request_secrets",
        json!({"group": "gitlab", "reason": "drop key", "remove": ["GITLAB_SSH_KEY"]}),
    )
    .await
    .unwrap();
    submit(&e.hub, r3["request_id"].as_str().unwrap(), Submission { values: Default::default(), decline: false })
        .await
        .unwrap();
    assert!(!e.hub.applied().await.defs.secrets.contains_key("GITLAB_SSH_KEY"));
    assert_eq!(e.hub.store.secret(&e.hub.secret_key, "GITLAB_SSH_KEY").await.unwrap(), None, "value deleted");
}

#[tokio::test]
async fn waiting_on_the_user_sleeps_after_a_grace_and_the_answer_wakes_it() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller};
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("botler"), Some("ask the user"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    let wake = e.hub.agents.get(&chat).await.unwrap().wake.clone();
    let me = Caller::resolve(&e.hub, Principal::Agent { instance: chat.clone(), wake }).await.unwrap();
    let asked = ops::call(&e.hub, &me, "ask_user", json!({"question": "Which colour?", "options": ["red", "blue"]}))
        .await
        .unwrap();
    let dialog = asked["dialog_id"].as_str().unwrap().to_string();

    // Well inside the idle timeout, but past the grace: it only waits for the user, so it sleeps.
    agents::reap_idle(&e.hub, Utc::now() + Duration::seconds(30)).await.unwrap();
    settle().await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running, "grace not over yet");
    agents::reap_idle(&e.hub, Utc::now() + Duration::minutes(3)).await.unwrap();
    // The exit is recorded by the agent's watcher task in its own time.
    for _ in 0..100 {
        settle().await;
        if e.hub.store.instance(&chat).await.unwrap().state == InstanceState::Dormant {
            break;
        }
    }
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);

    // The answer wakes it and is delivered.
    let d = e.hub.store.answer_dialog(&dialog, "blue").await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::DIALOG_ANSWERED, Source::User, Provenance::Trusted, Utc::now())
                .to(d.instance.clone())
                .payload(json!({"dialog_id": d.id, "question": d.question, "answer": "blue"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running);
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, &chat));
    assert!(prompts.last().unwrap().contains("dialog.answered") && prompts.last().unwrap().contains("blue"));

    // Without anything to wait for, the same idle time does not stop it.
    agents::reap_idle(&e.hub, Utc::now() + Duration::minutes(3)).await.unwrap();
    settle().await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running);
}

#[tokio::test]
async fn deep_agent_trees_report_up_without_tripping_and_depth_is_bounded() {
    use botler_hub::HubError;
    let e = env().await;
    with_key(&e).await;
    // Every agent may spawn: a 5-deep tree of dev agents under a chat.
    let mut chain = vec![router::new_instance(&e.hub, "chat", None, None).await.unwrap()];
    for level in 1..=5 {
        let parent = chain.last().unwrap().clone();
        let child = router::spawn(&e.hub, "dev", Some(&parent), Some(&format!("level {level} work")), None, None)
            .await
            .unwrap();
        assert_eq!(e.hub.store.instance(&child).await.unwrap().depth, level);
        chain.push(child);
    }
    // Each level ends its turn with a report, which reaches its parent and wakes it; the parent's
    // turn then ends with its own report to its parent, up to the root.
    for (level, name) in chain.iter().enumerate() {
        e.fake.set_last_reply(
            &agents::container_name(&e.hub, name),
            &format!("level {level}: built the parser and its tests"),
        );
    }
    for _ in 0..8 {
        agents::dispatch_pending(&e.hub).await.unwrap();
        settle().await;
        e.route().await;
    }
    let root_prompts = e.fake.prompts(&agents::container_name(&e.hub, &chain[0]));
    assert!(root_prompts.iter().any(|p| p.contains("level 1: built the parser")), "{root_prompts:?}");
    assert!(e.hub.store.breakers().await.unwrap().is_empty(), "{:?}", e.hub.store.breakers().await.unwrap());

    // Depth is bounded by max_spawn_depth (8 by default).
    let mut parent = chain.last().unwrap().clone();
    for _ in 6..=8 {
        parent = router::new_instance(&e.hub, "dev", Some(&parent), None).await.unwrap();
    }
    assert_eq!(e.hub.store.instance(&parent).await.unwrap().depth, 8);
    match router::spawn(&e.hub, "dev", Some(&parent), Some("go deeper"), None, None).await {
        Err(e @ HubError::SpawnTooDeep { .. }) => assert!(e.to_string().contains("do the work yourself"), "{e}"),
        other => panic!("{other:?}"),
    }
}
