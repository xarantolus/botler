mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::{agents, router};
use botler_store::InstanceState;
use chrono::{Duration, Utc};
use common::env;
use serde_json::json;

async fn with_key(e: &common::Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

async fn settle() {
    // let the pump task process the fake pi's events
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn message_wakes_main_and_is_delivered_once() {
    let e = env().await;
    with_key(&e).await;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to("main")
        .payload(json!({"text": "hello"}));
    e.hub.emit(ev).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let name = agents::container_name("main");
    let c = &e.fake.containers()[&name];
    let cmd = c.spec.command.join(" ");
    assert!(cmd.contains("--mode rpc") && cmd.contains("--session /botler/sessions/main.jsonl"), "{cmd}");
    assert!(cmd.contains("--model openrouter/deepseek/deepseek-v4.1-flash"), "{cmd}");
    assert_eq!(c.spec.env["OPENROUTER_API_KEY"], "sk-test");
    assert_eq!(c.spec.env["PI_OFFLINE"], "1");
    assert!(
        c.spec.mounts.iter().any(|m| matches!(m, botler_runtime::Mount::Tmpfs { dst } if dst == "/botler/agent-dir"))
    );
    let models = std::fs::read_to_string(e.hub.paths.agent_dir("main").join("models.json")).unwrap();
    assert!(models.contains("\"$OPENROUTER_API_KEY\"") && models.contains("16384"), "{models}");
    assert!(!models.contains("null"), "pi ignores models.json containing nulls: {models}");
    assert!(!models.contains("sk-test"), "secret values never land in rendered files");
    assert!(e.hub.paths.workspace("main").join(".git").exists());

    let prompts = e.fake.prompts(&name);
    assert_eq!(prompts.len(), 1);
    assert!(prompts[0].starts_with("botler:delivery ") && prompts[0].contains("hello"));
    assert!(e.hub.store.pending_deliveries("main").await.unwrap().is_empty());

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
    let agent = e.hub.agents.get("main").await.unwrap();
    let again = agent.pi.prompt(&agents::envelope(&delivery, &ev), &[], None).await.unwrap();
    assert_eq!(again, botler_runtime::Disposition::Handled);
    assert_eq!(e.fake.prompts(&name).len(), 1);
    settle().await;
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
}

#[tokio::test]
async fn idle_chats_sleep_and_always_on_main_stays() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("main"), Some("build a thing"), None, None).await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("main")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    assert!(e.fake.prompts(&agents::container_name(&child))[0].contains("build a thing"));

    agents::reap_idle(&e.hub, Utc::now() + Duration::hours(2)).await.unwrap();
    settle().await;
    // `main` is always_on in the template, so it is never reaped.
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
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
                .to("main")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.pending_deliveries("main").await.unwrap().len(), 1, "waits until the key is entered");
    with_key(&e).await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    assert!(e.hub.store.pending_deliveries("main").await.unwrap().is_empty());
}

#[tokio::test]
async fn always_on_main_starts_without_a_message() {
    let e = env().await;
    with_key(&e).await;
    agents::keep_always_on(&e.hub).await.unwrap();
    assert_eq!(e.hub.store.instance("main").await.unwrap().state, InstanceState::Running);
    assert!(e.fake.containers().contains_key(&agents::container_name("main")));
    assert!(e.fake.prompts(&agents::container_name("main")).is_empty(), "no prompt needed to be up");
}

#[tokio::test]
async fn crashing_always_on_agent_backs_off_and_opens_an_incident() {
    let e = env().await;
    with_key(&e).await;
    e.fake.set_crash_on_start(true);
    let mut starts = 0;
    for _ in 0..20 {
        if agents::keep_always_on(&e.hub).await.is_ok() && e.hub.agents.get("main").await.is_some() {
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
    assert_eq!(types["declared"][0]["type"], "mail.received");
}

#[tokio::test]
async fn sleeping_chat_is_woken_by_a_message_with_its_workspace() {
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("main"), Some("start"), None, None).await.unwrap();
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
    let prompts = e.fake.prompts(&agents::container_name(&chat));
    assert!(prompts.last().unwrap().contains("continue"));
}

#[tokio::test]
async fn archive_hands_over_and_messaging_restores() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller, OpError};
    let e = env().await;
    with_key(&e).await;
    let chat = router::spawn(&e.hub, "dev", Some("main"), Some("build x"), None, None).await.unwrap();
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
        matches!(ops::call(&e.hub, &me, "archive_ready", report.clone()).await, Err(OpError::BadRequest(_))),
        "only after a request"
    );

    e.hub.store.request_archive(&chat).await.unwrap();
    let out = ops::call(&e.hub, &me, "archive_ready", report).await.unwrap();
    let successor = out["handed_to"].as_str().unwrap().to_string();
    assert!(successor.starts_with("dev-") && successor != chat);
    let archived = e.hub.store.instance(&chat).await.unwrap();
    assert_eq!(archived.state, InstanceState::Archived);
    assert_eq!(archived.archive_report.as_ref().unwrap()["report"]["outstanding"][0], "wire x into the scheduler");
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

#[tokio::test]
async fn secret_groups_and_scratch_reach_the_container() {
    use botler_runtime::Mount;
    let e = env().await;
    with_key(&e).await;
    let secrets = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/secrets.toml")).unwrap()
        + "\n[[group]]\nname = \"station\"\nclass = \"container\"\n[[group.secret]]\nname = \"STATION_KEY\"\nfile = \"/root/.ssh/id_ed25519\"\n[[group.secret]]\nname = \"STATION_HOST\"\n";
    e.commit(&[
        ("secrets.toml", secrets.as_str()),
        ("agents/station/agent.toml", "kind = \"persistent\"\nsecrets = [\"station\"]\nscratch = \"groundstation\"\n"),
        ("agents/station/AGENTS.md", "station agent"),
    ])
    .await;
    e.hub.store.set_secret(&e.hub.secret_key, "STATION_KEY", "-----BEGIN KEY-----").await.unwrap();
    e.hub.store.set_secret(&e.hub.secret_key, "STATION_HOST", "station.lan").await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("station")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();

    let c = &e.fake.containers()[&agents::container_name("station")];
    assert_eq!(c.spec.env["STATION_HOST"], "station.lan");
    assert!(!c.spec.env.contains_key("STATION_KEY"), "file secrets are not env vars");
    let key_mount = c.spec.mounts.iter().find_map(|m| match m {
        Mount::Bind { src, dst, read_only } if dst == "/root/.ssh/id_ed25519" => Some((src.clone(), *read_only)),
        _ => None,
    });
    let (src, ro) = key_mount.expect("key mounted");
    assert!(ro);
    assert_eq!(std::fs::read_to_string(&src).unwrap(), "-----BEGIN KEY-----");
    let scratch = c.spec.mounts.iter().find_map(|m| match m {
        Mount::Bind { src, dst, .. } if dst == "/scratch" => Some(src.clone()),
        _ => None,
    });
    assert_eq!(scratch.unwrap(), e.hub.paths.scratch_space("groundstation"));
    let main_scratch = e
        .fake
        .containers()
        .values()
        .find(|c| c.spec.name == agents::container_name("main"))
        .map(|c| c.spec.mounts.clone());
    if let Some(m) = main_scratch {
        assert!(
            m.iter()
                .any(|m| matches!(m, Mount::Bind { src, dst, .. } if dst == "/scratch" && src.ends_with("chat-main")))
        );
    }
}

#[tokio::test]
async fn parent_hears_when_a_spawned_chat_finishes_its_turn() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("main"), Some("look into x"), None, None).await.unwrap();
    agents::dispatch_pending(&e.hub).await.unwrap();
    settle().await;
    e.route().await;
    let idle = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some(kinds::AGENT_IDLE.into()), limit: 5, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(idle.len(), 1, "one turn, one idle notice");
    assert_eq!(idle[0].to.as_deref(), Some("main"));
    assert_eq!(idle[0].payload["instance"], child.as_str());
    assert_eq!(idle[0].payload["last_reply"], "fake reply");
    assert!(idle[0].correlation.is_some(), "chained to the delivery that started the turn");
    assert!(e.hub.agents.phase(&child).await.is_none(), "no phase once running");
}

#[tokio::test]
async fn secret_requests_are_async_and_declare_on_submit() {
    use botler_hub::auth::Principal;
    use botler_hub::ops::{self, Caller, OpError};
    use botler_hub::secret_requests::{Submission, submit};
    let e = env().await;
    with_key(&e).await;
    e.hub
        .emit(
            Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
                .to("main")
                .payload(json!({"text": "hi"})),
        )
        .await
        .unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    let wake = e.hub.agents.get("main").await.unwrap().wake.clone();
    let main = Caller::resolve(&e.hub, Principal::Agent { instance: "main".into(), wake }).await.unwrap();

    // Invalid requests are refused immediately with a reason.
    let bad =
        ops::call(&e.hub, &main, "request_secrets", json!({"group": "gitlab", "reason": "x", "fill": ["NOPE"]})).await;
    assert!(matches!(bad, Err(OpError::BadRequest(_))), "{bad:?}");

    let r = ops::call(
        &e.hub,
        &main,
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
    let listed = ops::call(&e.hub, &main, "list_requests", json!({})).await.unwrap();
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
    let out = submit(
        &e.hub,
        &id,
        Submission {
            values: [("GITLAB_TOKEN_STATION".into(), "glpat".into()), ("GITLAB_SSH_KEY".into(), "KEY".into())].into(),
            decline: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(out["added"].as_array().unwrap().len(), 2);
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

    // Remove one field later; cancel works and blocks late answers.
    let r2 = ops::call(
        &e.hub,
        &main,
        "request_secrets",
        json!({"group": "gitlab", "reason": "rotate", "remove": ["GITLAB_SSH_KEY"]}),
    )
    .await
    .unwrap();
    let id2 = r2["request_id"].as_str().unwrap().to_string();
    ops::call(&e.hub, &main, "cancel_request", json!({"id": id2, "reason": "not needed"})).await.unwrap();
    assert!(submit(&e.hub, &id2, Submission { values: Default::default(), decline: false }).await.is_err());
    let r3 = ops::call(
        &e.hub,
        &main,
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
