//! `trial_run`: a job or service from an agent's branch, its effects recorded, never routed
//! (fake runtime).

mod common;

use botler_core::Source;
use botler_core::payloads::{BuiltinPayload, TrialFinishedPayload, TrialOutcome};
use botler_hub::auth::Principal;
use botler_hub::ops::{self, Caller, OpError};
use botler_hub::{jobs, router};
use botler_runtime::Mount;
use botler_store::{EventQuery, Run, RunStatus};
use chrono::Duration;
use serde_json::{Value, json};

const SECRETS: &str = "[[group]]\nname = \"other\"\nclass = \"container\"\n[[group.secret]]\nname = \"OTHER_KEY\"\n";

const WATCH: &str = "lifetime = \"service\"\ncommand = [\"python\", \"-m\", \"watch\"]\n\
    secrets = [\"openrouter\", \"other\"]\nvolume = true\nport = 8000\n\
    [[publish]]\nport = 8000\nhost_port = 18080\n";

struct Setup {
    e: common::Env,
    agent: Caller,
    instance: String,
    branch: String,
}

/// An agent `w` holding `openrouter` (not `other`), with `watch` pushed on its branch.
async fn setup(job_toml: &str) -> Setup {
    let e = common::env().await;
    e.commit(&[
        ("secrets.toml", &common::secrets_with(SECRETS)),
        ("agents/w/agent.toml", "secrets = [\"openrouter\"]\n"),
        ("agents/w/AGENTS.md", "w"),
    ])
    .await;
    for name in ["OPENROUTER_API_KEY", "OTHER_KEY"] {
        e.hub.store.set_secret(&e.hub.secret_key, name, "s3cret").await.unwrap();
    }
    let instance = router::new_instance(&e.hub, "w", None, None).await.unwrap();
    let agent = Caller {
        principal: Principal::Agent { instance: instance.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let branch = format!("agents/{instance}/watch");
    let files = [
        ("jobs/watch/job.toml", job_toml),
        ("jobs/watch/Dockerfile", common::JOB_DOCKERFILE),
        // Declared on the branch only: a trial checks its events against the branch.
        ("events/watch.toml", "version = 1\n[[event]]\ntype = \"watch.seen\"\ndescription = \"seen\"\n"),
    ];
    e.push(&branch, &files).await;
    Setup { e, agent, instance, branch }
}

/// Start a trial and wait until its container runs (the image builds in the background).
async fn start(s: &Setup, minutes: u32) -> Run {
    let out =
        ops::call(&s.e.hub, &s.agent, "trial_run", json!({"job": "watch", "branch": s.branch, "minutes": minutes}))
            .await
            .unwrap();
    let id = out["run_id"].as_str().unwrap().to_string();
    for _ in 0..200 {
        let run = s.e.hub.store.run(&id).await.unwrap();
        if run.status != RunStatus::Queued {
            return run;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("trial {id} never started");
}

async fn finished(s: &Setup) -> Vec<(Option<String>, TrialFinishedPayload)> {
    let q = EventQuery { kind: Some("trial.finished".into()), correlation: None, since: None, limit: 10 };
    let mut out = Vec::new();
    for ev in s.e.hub.store.events(&q).await.unwrap() {
        match BuiltinPayload::of(&ev).unwrap().unwrap() {
            BuiltinPayload::TrialFinished(p) => out.push((ev.to.clone(), p)),
            other => panic!("unexpected {other:?}"),
        }
    }
    out
}

async fn count(s: &Setup, kind: &str) -> usize {
    let q = EventQuery { kind: Some(kind.into()), correlation: None, since: None, limit: 100 };
    s.e.hub.store.events(&q).await.unwrap().len()
}

#[tokio::test]
async fn a_trial_records_its_effects_isolates_state_and_stops_on_time() {
    let s = setup(WATCH).await;
    let run = start(&s, 1).await;
    assert_eq!(run.status, RunStatus::Running);

    // Only held secrets, a throwaway /data, no port, not reachable as the service.
    let c = &s.e.fake.containers()[&jobs::container_name(&s.e.hub, &run.id)];
    assert_eq!(c.spec.env.get("OPENROUTER_API_KEY").map(String::as_str), Some("s3cret"));
    assert!(!c.spec.env.contains_key("OTHER_KEY"), "a group the agent does not hold is withheld");
    assert!(c.spec.ports.is_empty() && c.spec.network_aliases.is_empty());
    assert!(c.spec.mounts.iter().any(|m| matches!(m, Mount::Tmpfs { dst } if dst == "/data")));
    assert!(!c.spec.mounts.iter().any(|m| matches!(m, Mount::Volume { .. })), "never the production volume");
    assert_eq!(c.spec.labels["botler.role"], "trial");

    // One trial at a time per agent.
    let again =
        ops::call(&s.e.hub, &s.agent, "trial_run", json!({"job": "watch", "branch": s.branch, "minutes": 1})).await;
    assert!(matches!(again, Err(OpError::Hub(_))), "{again:?}");

    // Effects are recorded, answered with dry_run, and never become events or timers.
    let prod = Source::Runnable("watch".into()).key();
    s.e.hub.store.kv_set(&prod, "cursor", "41").await.unwrap();
    let me = Caller::resolve(&s.e.hub, Principal::Run { run_id: run.id.clone() }).await.unwrap();
    let call = |op: &'static str, input: Value| {
        let (hub, me) = (s.e.hub.clone(), me.clone());
        async move { ops::call(&hub, &me, op, input).await.unwrap() }
    };
    let emitted = call("emit_event", json!({"type": "watch.seen", "payload": {"name": "ada"}})).await;
    assert_eq!(emitted["dry_run"], true);
    let bad = ops::call(&s.e.hub, &me, "emit_event", json!({"type": "example.greeting", "payload": {}})).await;
    assert!(matches!(bad, Err(OpError::InvalidEvent(_))), "payloads are still checked: {bad:?}");
    assert_eq!(call("send_message", json!({"to": "botler", "text": "hi"})).await["dry_run"], true);
    assert_eq!(call("set_timer", json!({"in": "1h", "prompt": "later", "instance": "botler"})).await["dry_run"], true);
    assert_eq!(call("health", json!({"ok": true})).await["ok"], true);
    assert_eq!(call("state_get", json!({"key": "cursor"})).await["value"], Value::Null, "production state unseen");
    call("state_set", json!({"key": "cursor", "value": "1"})).await;
    assert_eq!(call("state_get", json!({"key": "cursor"})).await["value"], "1");
    assert_eq!(s.e.hub.store.kv_get(&prod, "cursor").await.unwrap().as_deref(), Some("41"), "production untouched");
    assert_eq!(count(&s, "watch.seen").await, 0);
    assert_eq!(count(&s, "message").await, 0);
    assert!(s.e.hub.store.pending_timers(None).await.unwrap().is_empty());

    // Still within its minute; then stopped.
    let started = run.started_at.unwrap();
    jobs::sweep_timeouts(&s.e.hub, started + Duration::seconds(50)).await.unwrap();
    assert_eq!(s.e.hub.store.run(&run.id).await.unwrap().status, RunStatus::Running);
    jobs::sweep_timeouts(&s.e.hub, started + Duration::seconds(61)).await.unwrap();
    assert_eq!(s.e.hub.store.run(&run.id).await.unwrap().status, RunStatus::Cancelled);
    assert!(s.e.fake.containers().is_empty(), "its container is gone");
    assert!(Caller::resolve(&s.e.hub, Principal::Run { run_id: run.id.clone() }).await.is_err());

    let done = finished(&s).await;
    assert_eq!(done.len(), 1);
    let (to, p) = &done[0];
    assert_eq!(to.as_deref(), Some(s.instance.as_str()), "delivered to the agent that started it");
    assert_eq!((p.outcome, p.job.as_str(), p.branch.as_str()), (TrialOutcome::TimeUp, "watch", s.branch.as_str()));
    assert_eq!(p.withheld_secrets, ["other"]);
    let ops: Vec<&str> = p.effects.iter().map(|e| e.op.as_str()).collect();
    assert_eq!(ops, ["emit_event", "send_message", "set_timer"]);
    assert_eq!(p.effects[0].detail["type"], "watch.seen");
    assert_eq!(p.effects[0].detail["payload"], json!({"name": "ada"}));
    assert!(s.e.hub.store.open_incidents().await.unwrap().is_empty(), "trials never open incidents");
    assert!(s.e.hub.store.runs_for("watch", 10).await.unwrap().is_empty(), "nor count as the job's runs");
}

#[tokio::test]
async fn a_failing_trial_reports_back_without_an_incident() {
    let s = setup("command = [\"python\", \"-m\", \"watch\"]\n").await;
    let run = start(&s, 5).await;
    jobs::on_exit(&s.e.hub, &run.id, Some(3), false).await.unwrap();
    let done = finished(&s).await;
    let p = &done[0].1;
    assert_eq!((p.outcome, p.exit_code), (TrialOutcome::Failed, Some(3)));
    assert!(p.effects.is_empty());
    assert!(s.e.hub.store.open_incidents().await.unwrap().is_empty());

    // Unhealthy reports end a trial the same way.
    let run = start(&s, 5).await;
    let me = Caller::resolve(&s.e.hub, Principal::Run { run_id: run.id.clone() }).await.unwrap();
    ops::call(&s.e.hub, &me, "health", json!({"ok": false, "reason": "imap login failed"})).await.unwrap();
    // Finished apart from the request (stopping the trial drops the request it came on).
    let mut done = finished(&s).await;
    for _ in 0..100 {
        if done.len() == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        done = finished(&s).await;
    }
    assert_eq!(done.len(), 2);
    assert!(done.iter().any(|(_, p)| p.reason.as_deref().is_some_and(|r| r.contains("imap login failed"))));
    assert!(s.e.hub.store.open_incidents().await.unwrap().is_empty());

    // The kill switch stops trials, and none start while paused.
    start(&s, 5).await;
    botler_hub::control::pause(&s.e.hub).await.unwrap();
    assert!(finished(&s).await.iter().any(|(_, p)| p.reason.as_deref() == Some("botler was paused")));
    assert!(s.e.fake.containers().is_empty());
    let r = ops::call(&s.e.hub, &s.agent, "trial_run", json!({"job": "watch", "branch": s.branch})).await;
    assert!(r.unwrap_err().to_string().contains("paused"));
}

#[tokio::test]
async fn an_agent_stops_its_trial_and_a_restart_ends_one_that_never_started() {
    let s = setup(WATCH).await;
    let none = ops::call(&s.e.hub, &s.agent, "stop_trial", json!({})).await.unwrap_err();
    assert!(none.to_string().contains("no running trial"), "{none}");

    let run = start(&s, 5).await;
    let out = ops::call(&s.e.hub, &s.agent, "stop_trial", json!({})).await.unwrap();
    assert_eq!(out["run_id"], run.id);
    assert_eq!(s.e.hub.store.run(&run.id).await.unwrap().status, RunStatus::Failed);
    assert!(s.e.fake.containers().is_empty());
    let done = finished(&s).await;
    assert_eq!(done[0].1.reason.as_deref(), Some("stopped by the agent that started it"));

    // A trial the old hub was still launching: the new one ends it, so another can start.
    let queued =
        s.e.hub
            .store
            .create_run(&botler_store::NewRun {
                runnable: "watch".into(),
                kind: botler_store::RunKind::Trial,
                trigger_id: None,
                event_id: None,
                commit_id: None,
            })
            .await
            .unwrap();
    s.e.hub.store.create_trial(&queued.id, &s.instance, &s.branch, 5, &[]).await.unwrap();
    botler_hub::trials::end_unstarted_on_boot(&s.e.hub).await.unwrap();
    assert_eq!(s.e.hub.store.run(&queued.id).await.unwrap().status, RunStatus::Failed);
    assert!(finished(&s).await.iter().any(|(_, p)| p.run_id == queued.id
        && p.reason.as_deref() == Some("the hub restarted before it started; start it again")));
    start(&s, 1).await;

    // A run that ended while its container was starting does not come back to life.
    assert!(!s.e.hub.store.start_run(&queued.id, "ctr").await.unwrap());
}

#[tokio::test]
async fn trials_are_checked() {
    let s = setup(WATCH).await;
    let call = |input: Value| ops::call(&s.e.hub, &s.agent, "trial_run", input);
    let err = |r: Result<Value, OpError>| r.unwrap_err().to_string();
    assert!(err(call(json!({"job": "watch", "branch": s.branch, "minutes": 11})).await).contains("1 to 10"));
    assert!(err(call(json!({"job": "watch", "branch": "agents/other/x"})).await).contains("your own branches"));
    assert!(err(call(json!({"job": "nope", "branch": s.branch})).await).contains("no job `nope`"));
    let missing = format!("agents/{}/missing", s.instance);
    assert!(err(call(json!({"job": "watch", "branch": missing})).await).contains("push"));
    let user = Caller { principal: Principal::User, capabilities: vec![], runnable: None };
    let r = ops::call(&s.e.hub, &user, "trial_run", json!({"job": "watch", "branch": s.branch})).await;
    assert!(err(r).contains("only agents"));
}

/// Real Podman (`BOTLER_PODMAN_TESTS=1`; needs the agent base image, which it never builds): the
/// template's example watcher, tried from a branch that ticks every 5 seconds, on a throwaway hub
/// listening on port 7435. It builds the job image, reaches the hub for `state`, `emit` and
/// `health`, is stopped after its minute, and its ticks come back as recorded effects. Removes the
/// images it built (its own data directory's label) afterwards; the trial removes its container.
#[tokio::test]
async fn podman_trial_of_the_example_watcher() {
    use std::sync::Arc;

    use botler_runtime::Containers;
    if std::env::var("BOTLER_PODMAN_TESTS").as_deref() != Ok("1") {
        return;
    }
    let rt = Arc::new(botler_runtime::Cli::detect(None).await.unwrap());
    if rt.image_id(botler_hub::assets::AGENT_BASE_LATEST).await.unwrap().is_none() {
        eprintln!("skipped: no agent base image");
        return;
    }
    let config = botler_hub::config::HubConfig::new(([0, 0, 0, 0], 7435).into());
    let e = common::env_with(rt.clone(), botler_runtime::fake::Fake::new(), config).await;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:7435").await.unwrap();
    tokio::spawn(axum::serve(listener, botler_hub::api::router(e.hub.clone())).into_future());
    tokio::spawn(botler_hub::supervisor::run(e.hub.clone()));

    let instance = router::new_instance(&e.hub, "botler", None, None).await.unwrap();
    let agent = Caller {
        principal: Principal::Agent { instance: instance.clone(), wake: "w".into() },
        capabilities: vec![],
        runnable: None,
    };
    let main_py = "jobs/example-watcher/src/example_watcher/__main__.py";
    let code = std::fs::read_to_string(format!("{}/../../template/{main_py}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let fast = code.replace("INTERVAL_SECONDS = 30.0", "INTERVAL_SECONDS = 5.0");
    assert_ne!(code, fast);
    let branch = format!("agents/{instance}/watch");
    e.push(&branch, &[(main_py, &fast)]).await;

    let input = json!({"job": "example-watcher", "branch": branch, "minutes": 1});
    let out = ops::call(&e.hub, &agent, "trial_run", input).await.unwrap();
    let run_id = out["run_id"].as_str().unwrap().to_string();
    let q = EventQuery { kind: Some("trial.finished".into()), correlation: None, since: None, limit: 5 };
    let mut done = Vec::new();
    for _ in 0..600 {
        done = e.hub.store.events(&q).await.unwrap();
        if !done.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    let run = e.hub.store.run(&run_id).await.unwrap();
    let leftover = rt.inspect(&jobs::container_name(&e.hub, &run_id)).await.unwrap();
    let owned = e.hub.labels("x").into_iter().filter(|(k, _)| k == botler_hub::images::OWNER_LABEL).collect();
    for t in rt.images(&owned).await.unwrap() {
        rt.remove_image(&t).await.unwrap();
    }

    assert_eq!(done.len(), 1, "no trial.finished; run: {run:?}");
    let Some(Ok(BuiltinPayload::TrialFinished(p))) = BuiltinPayload::of(&done[0]) else { panic!("{:?}", done[0]) };
    eprintln!("trial.finished: {}", serde_json::to_string_pretty(&p).unwrap());
    assert_eq!(p.outcome, TrialOutcome::TimeUp, "{p:?}");
    let counts: Vec<i64> = p.effects.iter().filter_map(|e| e.detail["payload"]["count"].as_i64()).collect();
    assert!(counts.len() >= 5 && counts[..3] == [1, 2, 3], "{counts:?}");
    assert!(p.effects.iter().all(|e| e.op == "emit_event" && e.detail["type"] == "example.tick"));
    assert!(p.logs_tail.contains("ticked"), "{}", p.logs_tail);
    assert!(run.heartbeat_at.is_some(), "health.ok() reached the hub");
    assert!(leftover.is_none(), "the trial removed its container");
    let ticks = EventQuery { kind: Some("example.tick".into()), correlation: None, since: None, limit: 5 };
    assert!(e.hub.store.events(&ticks).await.unwrap().is_empty(), "nothing was routed");
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_restart_launches_the_run_the_old_hub_was_starting() {
    let e = common::env().await;
    e.commit(&[
        ("jobs/hourly/job.toml", "command = [\"python\", \"-m\", \"hourly\"]\n"),
        ("jobs/hourly/Dockerfile", common::JOB_DOCKERFILE),
    ])
    .await;
    // Queued while its image built, then the hub stopped: nothing would ever launch it.
    let run = e
        .hub
        .store
        .create_run(&botler_store::NewRun {
            runnable: "hourly".into(),
            kind: botler_store::RunKind::Oneshot,
            trigger_id: None,
            event_id: None,
            commit_id: None,
        })
        .await
        .unwrap();
    jobs::resume_queued_on_boot(&e.hub).await.unwrap();
    for _ in 0..200 {
        if e.hub.store.run(&run.id).await.unwrap().status == RunStatus::Running {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the queued run never launched");
}
