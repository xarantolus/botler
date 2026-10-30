use botler_core::defs::Target;
use botler_core::{Event, Provenance, Source};
use botler_store::*;
use chrono::{Duration, Utc};
use serde_json::json;

async fn store() -> Store {
    Store::memory().await.unwrap()
}

fn ev(kind: &str) -> Event {
    Event::new(kind, Source::Agent("main".into()), Provenance::Trusted, Utc::now())
}

#[tokio::test]
async fn events_are_idempotent_and_queryable() {
    let s = store().await;
    let a = ev("mail.received").payload(json!({"n": 1}));
    s.append_event(&a).await.unwrap();
    s.append_event(&a).await.unwrap(); // retry after crash: no duplicate
    let b = ev("mail.sent").in_reply_to(&a);
    s.append_event(&b).await.unwrap();
    s.append_event(&ev("other")).await.unwrap();

    assert_eq!(s.event(&a.id.to_string()).await.unwrap(), a);
    let mail = s.events(&EventQuery { kind: Some("mail.*".into()), limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(mail.len(), 2);
    let thread =
        s.events(&EventQuery { correlation: Some(a.id.to_string()), limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(thread.len(), 2);

    assert_eq!(s.unrouted_events(10).await.unwrap().len(), 3);
    s.mark_routed(&a.id.to_string()).await.unwrap();
    assert_eq!(s.unrouted_events(10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn like_wildcards_in_kind_are_literal() {
    let s = store().await;
    s.append_event(&ev("a_b.x")).await.unwrap();
    s.append_event(&ev("aXb.x")).await.unwrap();
    let hits = s.events(&EventQuery { kind: Some("a_b.*".into()), limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn instance_and_delivery_flow() {
    let s = store().await;
    let inst = s
        .create_instance(&NewInstance {
            name: "main".into(),
            definition: "main".into(),
            persistent: true,
            session_file: "main/session.jsonl".into(),
            branch: None,
            parent: None,
            task: None,
        })
        .await
        .unwrap();
    assert_eq!(inst.state, InstanceState::Dormant);
    assert!(matches!(
        s.create_instance(&NewInstance {
            name: "main".into(),
            definition: "main".into(),
            persistent: true,
            session_file: "x".into(),
            branch: None,
            parent: None,
            task: None
        })
        .await,
        Err(Error::InstanceExists(_))
    ));

    let e = ev("message");
    s.append_event(&e).await.unwrap();
    let d1 = s.enqueue_delivery(&e.id.to_string(), "main", None, None).await.unwrap();
    let d2 = s.enqueue_delivery(&e.id.to_string(), "main", Some("t#x"), Some("hi")).await.unwrap();
    assert_eq!(s.instances_with_pending().await.unwrap(), vec!["main".to_string()]);
    let pending = s.pending_deliveries("main").await.unwrap();
    assert_eq!(pending.iter().map(|d| &d.id).collect::<Vec<_>>(), vec![&d1.id, &d2.id]);

    s.mark_delivered(&d1.id).await.unwrap();
    s.drop_delivery(&d2.id, "instance ended").await.unwrap();
    assert!(s.pending_deliveries("main").await.unwrap().is_empty());

    s.set_instance_state("main", InstanceState::Running, Some("abc")).await.unwrap();
    assert_eq!(s.reset_running_instances().await.unwrap(), 1);
    let i = s.instance("main").await.unwrap();
    assert_eq!(i.state, InstanceState::Dormant);
    assert_eq!(i.container_id, None);
}

#[tokio::test]
async fn timers_fire_once() {
    let s = store().await;
    let now = Utc::now();
    let t = s
        .create_timer(&NewTimer {
            fire_at: now - Duration::seconds(1),
            target: Target::Resume("main".into()),
            prompt: Some("wake up".into()),
            payload: json!({}),
            created_by: "agent:main".into(),
        })
        .await
        .unwrap();
    s.create_timer(&NewTimer {
        fire_at: now + Duration::hours(1),
        target: Target::Run("poll".into()),
        prompt: None,
        payload: json!({"x": 1}),
        created_by: "agent:main".into(),
    })
    .await
    .unwrap();
    assert_eq!(s.due_timers(now).await.unwrap().len(), 1);
    assert!(s.next_timer_at().await.unwrap().unwrap() < now);
    assert!(s.set_timer_status(&t.id, TimerStatus::Fired).await.unwrap());
    assert!(!s.set_timer_status(&t.id, TimerStatus::Cancelled).await.unwrap(), "already fired");
    assert!(s.due_timers(now).await.unwrap().is_empty());
}

#[tokio::test]
async fn schedule_state_anchors_once() {
    let s = store().await;
    let t0 = Utc::now();
    let st = s.schedule_state("a#b", t0).await.unwrap();
    let later = s.schedule_state("a#b", t0 + Duration::hours(1)).await.unwrap();
    assert_eq!(st.anchor, later.anchor);
    s.record_schedule("a#b", t0, 3).await.unwrap();
    assert_eq!(s.schedule_state("a#b", t0).await.unwrap().skipped, 3);
    assert_eq!(s.prune_schedules(&[]).await.unwrap(), 1);
}

#[tokio::test]
async fn incidents_coalesce_until_resolved() {
    let s = store().await;
    let (a, new_a) = s.record_failure("poller", "exit", Some("r1")).await.unwrap();
    let (b, new_b) = s.record_failure("poller", "crashloop", Some("r2")).await.unwrap();
    assert!(new_a && !new_b);
    assert_eq!(a.id, b.id);
    assert_eq!(b.count, 2);
    assert_eq!(b.kind, "crashloop");
    s.resolve_incident(&a.id, "fixed selector").await.unwrap();
    let (c, new_c) = s.record_failure("poller", "exit", None).await.unwrap();
    assert!(new_c);
    assert_ne!(c.id, a.id);
    assert_eq!(s.incidents_for("poller", 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn runs_and_crashloop_counting() {
    let s = store().await;
    let r = s
        .create_run(&NewRun {
            runnable: "poller".into(),
            kind: RunKind::Service,
            trigger_id: None,
            event_id: None,
            commit_id: Some("c1".into()),
        })
        .await
        .unwrap();
    s.start_run(&r.id, "ctr").await.unwrap();
    assert_eq!(s.active_runs().await.unwrap().len(), 1);
    assert!(s.finish_run(&r.id, RunStatus::Failed, Some(1), Some("boom")).await.unwrap());
    assert!(!s.finish_run(&r.id, RunStatus::Succeeded, Some(0), None).await.unwrap(), "terminal: ignored");
    let r = s.run(&r.id).await.unwrap();
    assert_eq!(r.status, RunStatus::Failed);
    assert_eq!(s.failures_since("poller", Utc::now() - Duration::minutes(5)).await.unwrap(), 1);
}

#[tokio::test]
async fn dialogs_reject_late_and_invalid_answers() {
    let s = store().await;
    let new = |q: &str| NewDialog {
        instance: "main".into(),
        kind: DialogKind::Question,
        question: q.into(),
        detail: None,
        ctas: vec!["yes".into(), "no".into()],
        about: vec!["proposal:1".into()],
        priority: Priority::Blocking,
        proposal_id: None,
        spec: None,
        expires_at: None,
    };
    let d = s.create_dialog(&new("deploy?")).await.unwrap();
    assert!(matches!(s.answer_dialog(&d.id, "maybe").await, Err(Error::AnswerNotAllowed { .. })));
    let answered = s.answer_dialog(&d.id, "yes").await.unwrap();
    assert_eq!(answered.status, DialogStatus::Answered);

    let d2 = s.create_dialog(&new("again?")).await.unwrap();
    assert!(s.close_dialog(&d2.id, DialogStatus::Aborted, "proposal changed").await.unwrap());
    match s.answer_dialog(&d2.id, "yes").await {
        Err(Error::DialogClosed { status: DialogStatus::Aborted, reason: Some(r) }) => {
            assert_eq!(r, "proposal changed")
        }
        other => panic!("{other:?}"),
    }
    assert!(s.open_dialogs().await.unwrap().is_empty());
}

#[tokio::test]
async fn secrets_roundtrip_and_bind_name() {
    let s = store().await;
    let key = SecretKey::ephemeral();
    s.set_secret(&key, "GITLAB_TOKEN", "glpat-123").await.unwrap();
    assert_eq!(s.secret(&key, "GITLAB_TOKEN").await.unwrap().as_deref(), Some("glpat-123"));
    assert_eq!(s.secret(&key, "MISSING").await.unwrap(), None);
    assert_eq!(s.secret_names().await.unwrap(), vec!["GITLAB_TOKEN"]);

    // Moving a ciphertext to another name must not decrypt.
    sqlx::query("INSERT INTO secrets (name, nonce, ciphertext, updated_at) SELECT 'OTHER', nonce, ciphertext, updated_at FROM secrets WHERE name = 'GITLAB_TOKEN'")
        .execute(s.pool())
        .await
        .unwrap();
    assert!(matches!(s.secret(&key, "OTHER").await, Err(Error::Decrypt(_))));
    // Wrong key must not decrypt.
    assert!(matches!(s.secret(&SecretKey::ephemeral(), "GITLAB_TOKEN").await, Err(Error::Decrypt(_))));
}

#[tokio::test]
async fn secret_key_file_is_created_private_and_reloaded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secrets.key");
    let s = store().await;
    let k1 = SecretKey::load_or_create(&path).unwrap();
    s.set_secret(&k1, "A", "v").await.unwrap();
    let k2 = SecretKey::load_or_create(&path).unwrap();
    assert_eq!(s.secret(&k2, "A").await.unwrap().as_deref(), Some("v"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[tokio::test]
async fn kv_usage_breakers_meta() {
    let s = store().await;
    s.kv_set("job:poller", "cursor", "42").await.unwrap();
    assert_eq!(s.kv_get("job:poller", "cursor").await.unwrap().as_deref(), Some("42"));
    assert_eq!(s.kv_get("job:other", "cursor").await.unwrap(), None);
    assert!(matches!(s.kv_set("job:poller", "big", &"x".repeat(70_000)).await, Err(Error::ValueTooLarge { .. })));

    let t = Utc::now();
    s.record_usage("main", t, "m", 100, 0.01).await.unwrap();
    s.record_usage("main", t, "m", 50, 0.02).await.unwrap();
    s.record_usage("triage", t, "m", 10, 0.5).await.unwrap();
    let main = s.usage_since(Some("main"), t - Duration::hours(1)).await.unwrap();
    assert_eq!((main.tokens, main.calls), (150, 2));
    assert!((s.usage_since(None, t - Duration::hours(1)).await.unwrap().cost_usd - 0.53).abs() < 1e-9);

    assert!(s.trip_breaker("agent:a", "cycle", "ping-pong").await.unwrap());
    assert!(!s.trip_breaker("agent:a", "cycle", "again").await.unwrap());
    assert_eq!(s.breaker("agent:a").await.unwrap().unwrap().reason, "ping-pong");
    assert!(s.clear_breaker("agent:a").await.unwrap());

    assert!(!s.paused().await.unwrap());
    s.set_paused(true).await.unwrap();
    assert!(s.paused().await.unwrap());
    s.set_last_applied_commit("abc").await.unwrap();
    assert_eq!(s.last_applied_commit().await.unwrap().as_deref(), Some("abc"));
}

#[tokio::test]
async fn file_database_survives_reopen_and_backs_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hub.db");
    {
        let s = Store::open(&path).await.unwrap();
        s.append_event(&ev("persisted")).await.unwrap();
        s.backup(&dir.path().join("backup.db")).await.unwrap();
    }
    let s = Store::open(&path).await.unwrap();
    assert_eq!(s.unrouted_events(10).await.unwrap().len(), 1);
    let b = Store::open(&dir.path().join("backup.db")).await.unwrap();
    assert_eq!(b.unrouted_events(10).await.unwrap()[0].kind, "persisted");
}

#[tokio::test]
async fn slugs_are_unique_aliases() {
    let s = store().await;
    for n in ["chat-a1", "chat-b2"] {
        s.create_instance(&NewInstance {
            name: n.into(),
            definition: "chat".into(),
            persistent: false,
            session_file: format!("{n}.jsonl"),
            branch: None,
            parent: None,
            task: None,
        })
        .await
        .unwrap();
    }
    s.set_instance_title("chat-a1", "Antenna fix", "groundstation-antenna-fix").await.unwrap();
    assert_eq!(s.resolve_instance("groundstation-antenna-fix").await.unwrap().unwrap().name, "chat-a1");
    assert_eq!(s.resolve_instance("chat-b2").await.unwrap().unwrap().name, "chat-b2");
    assert!(matches!(
        s.set_instance_title("chat-b2", "x", "groundstation-antenna-fix").await,
        Err(Error::SlugTaken(_))
    ));
    // a slug may not shadow another instance's id
    assert!(matches!(s.set_instance_title("chat-a1", "x", "chat-b2").await, Err(Error::SlugTaken(_))));
}
