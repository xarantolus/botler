//! The System view and "Run now" / "Restart" (fake runtime).

mod common;

use botler_hub::auth::Principal;
use botler_hub::ops::{self, OpError};
use botler_hub::wire::ServiceState;
use botler_hub::{control, services, system};
use botler_store::RunStatus;
use chrono::{Duration, Timelike, Utc};
use chrono_tz::Tz;
use common::job;
use serde_json::{Value, json};

const ONESHOT: &str = "command = [\"python\", \"-m\", \"app\"]\n";

async fn env_with(files: Vec<(String, String)>) -> common::Env {
    let e = common::env().await;
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    e.commit(&refs).await;
    e
}

async fn run_now(e: &common::Env, job: &str) -> Result<Value, OpError> {
    let user = ops::Caller::resolve(&e.hub, Principal::User).await.unwrap();
    ops::call(&e.hub, &user, "run_job", json!({ "job": job })).await
}

#[tokio::test]
async fn shows_schedules_in_the_instance_timezone_with_last_runs_and_services() {
    let tpl = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/botler.toml")).unwrap();
    let mut files = vec![("botler.toml".to_string(), tpl.replace("# timezone = ", "timezone = "))];
    files.extend(job("svc", "lifetime = \"service\"\ncommand = [\"x\"]\nport = 8000\n"));
    let e = env_with(files).await;
    services::reconcile(&e.hub).await.unwrap();

    let started = run_now(&e, "web-health").await.unwrap();
    let v = system::view(&e.hub).await.unwrap();
    assert_eq!(v.timezone, "Europe/Berlin");
    assert_eq!(v.commit.title, "test change");
    let wh = v.jobs.iter().find(|j| j.name == "web-health").expect("the template's web-health job");
    let t = &wh.triggers[0];
    assert!(t.when.contains("Europe/Berlin"), "{}", t.when);
    assert_eq!(t.upcoming.len(), 3);
    let tz: Tz = "Europe/Berlin".parse().unwrap();
    for (i, at) in t.upcoming.iter().enumerate() {
        let local = at.with_timezone(&tz);
        assert_eq!((local.hour(), local.minute()), (6, 23), "fire {i} at 06:23 Berlin time");
        assert!(*at > Utc::now() && *at < Utc::now() + Duration::days(4));
    }
    assert!(t.upcoming.windows(2).all(|w| w[1] - w[0] >= Duration::hours(23)));
    assert_eq!(wh.recent_runs[0].id, started["run_id"], "the last run is joined");
    assert!(wh.service.is_none() && wh.secrets == ["openrouter"]);

    let svc = v.jobs.iter().find(|j| j.name == "svc").unwrap();
    assert_eq!(svc.service.as_ref().unwrap().state, ServiceState::Running);
    assert!(v.agents.iter().any(|a| a.name == "botler" && a.always_on));
}

#[tokio::test]
async fn run_now_starts_once_and_honours_pause_and_overlap() {
    let e = env_with(job("app", ONESHOT)).await;
    let first = run_now(&e, "app").await.unwrap();
    assert_eq!(first["queued"], false);
    // The container starts in the background (image builds take a while).
    let mut runs = e.hub.store.runs_for("app", 10).await.unwrap();
    for _ in 0..100 {
        if runs[0].status != RunStatus::Queued {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        runs = e.hub.store.runs_for("app", 10).await.unwrap();
    }
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Running);
    let ev = e.hub.store.event(runs[0].event_id.as_deref().unwrap()).await.unwrap();
    assert_eq!(ev.kind, "manual");

    // No trigger: the default overlap policy (skip) refuses a second run while one is active.
    assert!(matches!(run_now(&e, "app").await, Err(OpError::Conflict(_))));
    assert_eq!(e.hub.store.runs_for("app", 10).await.unwrap().len(), 1);

    control::pause(&e.hub).await.unwrap();
    assert!(run_now(&e, "app").await.is_err(), "refused while paused");
    control::resume(&e.hub).await.unwrap();

    // A trigger with overlap = queue queues behind the active run.
    let queued = format!("{ONESHOT}[[trigger]]\nname = \"n\"\ncron = \"0 3 * * *\"\noverlap = \"queue\"\n");
    let e = env_with(job("app", &queued)).await;
    run_now(&e, "app").await.unwrap();
    assert_eq!(run_now(&e, "app").await.unwrap()["queued"], true);

    // An undefined job is refused.
    assert!(run_now(&e, "nope").await.is_err());
}

#[tokio::test]
async fn restart_stops_a_service_and_reconcile_starts_it_again() {
    let e = env_with(job("svc", "lifetime = \"service\"\ncommand = [\"x\"]\n")).await;
    services::reconcile(&e.hub).await.unwrap();
    let user = ops::Caller::resolve(&e.hub, Principal::User).await.unwrap();
    let out = ops::call(&e.hub, &user, "restart_service", json!({ "service": "svc" })).await.unwrap();
    let stopped = out["stopped"].as_str().unwrap().to_string();
    assert_eq!(e.hub.store.run(&stopped).await.unwrap().status, RunStatus::Cancelled);
    services::reconcile(&e.hub).await.unwrap();
    let runs = e.hub.store.runs_for("svc", 10).await.unwrap();
    assert_eq!((runs.len(), runs[0].status), (2, RunStatus::Running));
    assert!(ops::call(&e.hub, &user, "run_job", json!({ "job": "svc" })).await.is_err(), "services are not run");
}
