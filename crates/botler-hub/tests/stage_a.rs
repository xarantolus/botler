mod common;

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::{jobs, router, scheduler, supervisor};
use botler_runtime::ContainerEvent;
use botler_store::{DialogKind, DialogStatus, NewDialog, NewTimer, Priority, RunStatus};
use chrono::{Duration, Utc};
use common::{env, job};
use serde_json::json;

fn refs(v: &[(String, String)]) -> Vec<(&str, &str)> {
    v.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect()
}

#[tokio::test]
async fn timer_wakes_botler_with_a_delivery() {
    let e = env().await;
    e.hub
        .store
        .create_timer(&NewTimer {
            fire_at: Utc::now() - Duration::seconds(1),
            target: botler_core::defs::Target::Resume("botler".into()),
            prompt: Some("Check the vacuum.".into()),
            payload: json!({"note": "vacuum"}),
            created_by: "agent:botler".into(),
        })
        .await
        .unwrap();
    scheduler::tick(&e.hub, Utc::now()).await.unwrap();
    let pending = e.hub.store.pending_deliveries("botler").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].prompt.as_deref(), Some("Check the vacuum."));
    // fired exactly once
    scheduler::tick(&e.hub, Utc::now()).await.unwrap();
    assert_eq!(e.hub.store.pending_deliveries("botler").await.unwrap().len(), 1);
}

#[tokio::test]
async fn cron_trigger_runs_job_and_success_is_reported() {
    let e = env().await;
    let mut files = job("poll", "command = [\"python\", \"-m\", \"poll\"]\n");
    files.push((
        "triggers/poll.toml".into(),
        "[[trigger]]\nname = \"poll\"\ncron = \"* * * * *\"\ntarget = { run = \"poll\" }\n".into(),
    ));
    e.commit(&refs(&files)).await;
    let now = Utc::now();
    scheduler::tick(&e.hub, now).await.unwrap(); // anchors
    scheduler::tick(&e.hub, now + Duration::seconds(61)).await.unwrap(); // due
    let runs = e.hub.store.runs_for("poll", 10).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, RunStatus::Running);
    let name = jobs::container_name(&runs[0].id);
    let c = &e.fake.containers()[&name];
    assert!(c.spec.env.contains_key("BOTLER_TOKEN"));
    assert_eq!(c.spec.env["TZ"], e.hub.applied().await.defs.timezone.name(), "jobs run in local time");
    assert!(
        c.spec.mounts.iter().any(|m| matches!(m, botler_runtime::Mount::Bind { dst, .. } if dst == "/opt/botler/sdk"))
    );
    assert!(e.fake.builds().iter().any(|b| b.contains("jobs/poll/Dockerfile")));

    e.fake.exit(&name, 0);
    supervisor::on_event(&e.hub, &ContainerEvent { name: name.clone(), status: "died".into(), exit_code: Some(0) })
        .await
        .unwrap();
    assert_eq!(e.hub.store.run(&runs[0].id).await.unwrap().status, RunStatus::Succeeded);
    assert!(!e.fake.containers().contains_key(&name), "finished containers are removed");
    let finished = e
        .hub
        .store
        .events(&botler_store::EventQuery {
            kind: Some(kinds::RUNNABLE_FINISHED.into()),
            limit: 5,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(finished.len(), 1);
}

#[tokio::test]
async fn failures_open_one_incident_and_spawn_triage_once() {
    let e = env().await;
    e.commit(&refs(&job("flaky", "command = [\"false\"]\n"))).await;
    for _ in 0..2 {
        let run = jobs::start(&e.hub, "flaky", None, None).await.unwrap().unwrap();
        let name = jobs::container_name(&run);
        e.fake.exit(&name, 1);
        supervisor::on_event(&e.hub, &ContainerEvent { name, status: "died".into(), exit_code: Some(1) })
            .await
            .unwrap();
    }
    let incidents = e.hub.store.open_incidents().await.unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].count, 2);
    let triage: Vec<_> =
        e.hub.store.live_instances().await.unwrap().into_iter().filter(|i| i.definition == "triage").collect();
    assert_eq!(triage.len(), 1, "only the first failure spawns triage");
    let d = e.hub.store.pending_deliveries(&triage[0].name).await.unwrap();
    assert_eq!(d.len(), 1);
    assert!(d[0].prompt.as_deref().unwrap().contains("flaky"));
}

#[tokio::test]
async fn missing_job_reports_to_sender() {
    let e = env().await;
    let ev = Event::new("schedule", Source::Agent("botler".into()), Provenance::Trusted, Utc::now());
    let r = router::fire(&e.hub, &botler_core::defs::Target::Run("nope".into()), None, None, ev).await;
    assert!(r.is_err());
    e.route().await;
    let pending = e.hub.store.pending_deliveries("botler").await.unwrap();
    assert_eq!(pending.len(), 1, "the sender is told routing failed");
}

#[tokio::test]
async fn timeouts_are_swept() {
    let e = env().await;
    e.commit(&refs(&job("slow", "command = [\"sleep\", \"1000\"]\ntimeout = \"1m\"\n"))).await;
    let run = jobs::start(&e.hub, "slow", None, None).await.unwrap().unwrap();
    jobs::sweep_timeouts(&e.hub, Utc::now()).await.unwrap();
    assert_eq!(e.hub.store.run(&run).await.unwrap().status, RunStatus::Running);
    jobs::sweep_timeouts(&e.hub, Utc::now() + Duration::minutes(2)).await.unwrap();
    assert_eq!(e.hub.store.run(&run).await.unwrap().status, RunStatus::Timeout);
    assert_eq!(e.hub.store.open_incidents().await.unwrap()[0].kind, "timeout");
}

#[tokio::test]
async fn events_abort_dialogs_about_them() {
    let e = env().await;
    let new = |about: &str| NewDialog {
        instance: "botler".into(),
        kind: DialogKind::Approval,
        question: "merge?".into(),
        detail: None,
        ctas: vec!["approve".into(), "reject".into()],
        about: vec![about.into()],
        priority: Priority::Blocking,
        proposal_id: None,
        spec: None,
        expires_at: None,
    };
    let hit = e.hub.store.create_dialog(&new("proposal:p1")).await.unwrap();
    let miss = e.hub.store.create_dialog(&new("proposal:p2")).await.unwrap();
    e.hub
        .emit(
            Event::new(kinds::PROPOSAL_UPDATED, Source::Hub, Provenance::Trusted, Utc::now())
                .payload(json!({"proposal_id": "p1"})),
        )
        .await
        .unwrap();
    e.route().await;
    assert_eq!(e.hub.store.dialog(&hit.id).await.unwrap().status, DialogStatus::Aborted);
    assert_eq!(e.hub.store.dialog(&miss.id).await.unwrap().status, DialogStatus::Open);
    let aborted = e.hub.store.pending_deliveries("botler").await.unwrap();
    assert_eq!(aborted.len(), 1, "the asking agent is told");
}

#[tokio::test]
async fn ping_pong_trips_the_cycle_breaker() {
    let e = env().await;
    let root = Event::new(kinds::MESSAGE, Source::Agent("a".into()), Provenance::Trusted, Utc::now()).to("botler");
    let mut last = root.clone();
    for _ in 0..=botler_core::limits::MAX_CHAIN_HOPS {
        last = Event::new(kinds::MESSAGE, Source::Agent("b".into()), Provenance::Trusted, Utc::now())
            .to("botler")
            .in_reply_to(&last);
    }
    e.hub.emit(last.clone()).await.unwrap();
    e.route().await;
    assert!(e.hub.store.breaker(&format!("chain:{}", root.id)).await.unwrap().is_some());
    // later events on the chain are dropped
    let next = Event::new(kinds::MESSAGE, Source::Agent("b".into()), Provenance::Trusted, Utc::now())
        .to("botler")
        .in_reply_to(&root);
    let before = e.hub.store.pending_deliveries("botler").await.unwrap().len();
    e.hub.emit(next).await.unwrap();
    e.route().await;
    assert_eq!(e.hub.store.pending_deliveries("botler").await.unwrap().len(), before);
}

#[tokio::test]
async fn paused_hub_fires_nothing() {
    let e = env().await;
    e.hub.store.set_paused(true).await.unwrap();
    e.hub
        .store
        .create_timer(&NewTimer {
            fire_at: Utc::now() - Duration::seconds(1),
            target: botler_core::defs::Target::Resume("botler".into()),
            prompt: None,
            payload: json!({}),
            created_by: "user".into(),
        })
        .await
        .unwrap();
    scheduler::tick(&e.hub, Utc::now()).await.unwrap();
    assert!(e.hub.store.pending_deliveries("botler").await.unwrap().is_empty());
    e.hub.store.set_paused(false).await.unwrap();
    scheduler::tick(&e.hub, Utc::now()).await.unwrap();
    assert_eq!(e.hub.store.pending_deliveries("botler").await.unwrap().len(), 1);
}
