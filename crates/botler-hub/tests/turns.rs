//! Failed turns (retries, give-up, compaction), `reply.overdue`, and the reviewer fallback.

mod common;

use std::future::Future;
use std::time::{Duration, Instant};

use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_hub::phase::WakePhase;
use botler_hub::{agents, router, transactions, turns};
use botler_store::{EventQuery, Proposal, ProposalStatus};
use chrono::Utc;
use common::env;
use serde_json::json;

async fn with_key(e: &common::Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

/// Poll `check` until it returns `Some`, for up to 30 s (the workspace tests run in parallel).
async fn until<T, F: Future<Output = Option<T>>>(what: &str, mut check: impl FnMut() -> F) -> T {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn events(e: &common::Env, kind: &str) -> Vec<Event> {
    e.route().await;
    e.hub.store.events(&EventQuery { kind: Some(kind.into()), limit: 20, ..Default::default() }).await.unwrap()
}

/// Spawn a `dev` chat under `botler` whose next turns fail with `errors`, and start it.
async fn failing_child(e: &common::Env, errors: &[&str]) -> String {
    with_key(e).await;
    let child = router::new_instance(&e.hub, "dev", Some("botler"), None).await.unwrap();
    e.fake.fail_turns(&child, errors);
    let task = Event::new(kinds::MESSAGE, Source::Agent("botler".into()), Provenance::Trusted, Utc::now())
        .to(child.clone())
        .payload(json!({"text": "look into x"}));
    e.hub.emit(task).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    child
}

/// Wait until the failed turn's retry is scheduled, then send it (as the scheduler would once due).
async fn run_retry(e: &common::Env, child: &str) -> chrono::DateTime<Utc> {
    let at =
        until("a scheduled retry", || async { e.hub.store.turn_retry(child).await.unwrap().and_then(|r| r.next_at) })
            .await;
    turns::sweep(&e.hub, at).await.unwrap();
    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    at
}

fn prompts(e: &common::Env, child: &str) -> Vec<String> {
    e.fake.prompts(&agents::container_name(&e.hub, child))
}

#[tokio::test]
async fn transient_error_is_retried_with_backoff_and_succeeds() {
    let e = env().await;
    let child = failing_child(&e, &["Connection error."]).await;
    let before = Utc::now();
    let at = run_retry(&e, &child).await;
    let wait = (at - before).num_seconds();
    assert!((29..=45).contains(&wait), "first retry after ~30s, got {wait}s");

    let idle = until("the parent hears the result", || async {
        Some(events(&e, kinds::AGENT_IDLE).await).filter(|v| !v.is_empty())
    })
    .await;
    assert_eq!(idle[0].to.as_deref(), Some("botler"));
    let p = prompts(&e, &child);
    assert_eq!(p.len(), 2, "the task, then one retry");
    assert!(p[1].contains("transient provider error (Connection error.)"), "{}", p[1]);
    assert!(e.hub.store.turn_retry(&child).await.unwrap().is_none(), "retries forgotten once it succeeds");
    assert!(events(&e, kinds::AGENT_TURN_FAILED).await.is_empty());
    assert!(e.hub.store.open_incidents().await.unwrap().is_empty());
}

#[tokio::test]
async fn exhausted_retries_open_an_incident_and_tell_the_parent() {
    let e = env().await;
    let child = failing_child(&e, &["529 overloaded"; 5]).await;
    let mut gaps = Vec::new();
    for _ in 0..4 {
        let now = Utc::now();
        gaps.push((run_retry(&e, &child).await - now).num_seconds());
    }
    // Measured from before each failed turn, so a slow machine adds a few seconds.
    let expected = [30, 120, 600, 1800];
    assert!(gaps.iter().zip(expected).all(|(g, want)| (want - 1..=want + 15).contains(g)), "backoff: {gaps:?}");
    let failed = until("agent.turn_failed", || async {
        Some(events(&e, kinds::AGENT_TURN_FAILED).await).filter(|v| !v.is_empty())
    })
    .await;
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].to.as_deref(), Some("botler"));
    assert_eq!(failed[0].payload["class"], "transient");
    assert_eq!(failed[0].payload["attempts"], 5);
    assert!(failed[0].correlation.is_some(), "chained to the task");
    assert_eq!(prompts(&e, &child).len(), 5, "no fifth retry");
    let incidents = e.hub.store.open_incidents().await.unwrap();
    assert!(incidents.iter().any(|i| i.runnable == format!("agent:{child}")), "{incidents:?}");
    assert!(matches!(e.hub.agents.phase(&child).await, Some(WakePhase::Failed { error }) if error.contains("529")));
    assert!(e.hub.store.turn_retry(&child).await.unwrap().is_none());
}

#[tokio::test]
async fn permanent_error_is_not_retried() {
    let e = env().await;
    let child = failing_child(&e, &["404 No endpoints found for model x/y"]).await;
    let failed = until("agent.turn_failed", || async {
        Some(events(&e, kinds::AGENT_TURN_FAILED).await).filter(|v| !v.is_empty())
    })
    .await;
    assert_eq!(failed[0].payload["class"], "permanent");
    assert_eq!(failed[0].payload["attempts"], 1);
    assert!(e.hub.store.turn_retry(&child).await.unwrap().is_none());
    assert!(e.hub.store.next_wait_at().await.unwrap().is_none(), "nothing scheduled");
    assert_eq!(prompts(&e, &child).len(), 1);
}

#[tokio::test]
async fn context_overflow_compacts_and_retries_once() {
    let e = env().await;
    let child = failing_child(&e, &["400 This model's maximum context length is 128000 tokens"; 2]).await;
    run_retry(&e, &child).await;
    assert_eq!(e.fake.compactions(&child), 1);
    let failed = until("agent.turn_failed", || async {
        Some(events(&e, kinds::AGENT_TURN_FAILED).await).filter(|v| !v.is_empty())
    })
    .await;
    assert_eq!(failed[0].payload["class"], "context_length");
    assert_eq!(failed[0].payload["attempts"], 2);
    assert_eq!(e.fake.compactions(&child), 1, "compacted once");
    assert_eq!(prompts(&e, &child).len(), 2, "retried once");
}

#[tokio::test]
async fn billing_error_pauses_and_asks_the_user() {
    let e = env().await;
    let child = failing_child(&e, &["402 Insufficient credits"]).await;
    let d = until("a dialog for the user", || async {
        e.hub.store.open_dialogs().await.unwrap().into_iter().find(|d| d.instance == child)
    })
    .await;
    assert!(d.question.contains("out of credits"), "{}", d.question);
    assert_eq!(d.priority, botler_store::Priority::Blocking);
    assert!(e.hub.store.breaker(&format!("instance:{child}")).await.unwrap().is_some(), "paused");
    assert_eq!(events(&e, kinds::AGENT_TURN_FAILED).await[0].payload["class"], "billing");
}

#[tokio::test]
async fn overdue_answers_are_reported_and_replies_clear_them() {
    let e = env().await;
    with_key(&e).await;
    let child = router::spawn(&e.hub, "dev", Some("botler"), Some("x"), None, None).await.unwrap();
    turns::expect_reply(&e.hub, "botler", &child, None, None).await.unwrap();
    // A reply to the waiting agent clears the expectation.
    let reply = Event::new(kinds::MESSAGE, Source::Agent(child.clone()), Provenance::Trusted, Utc::now())
        .to("botler")
        .payload(json!({"text": "done"}));
    e.hub.emit(reply).await.unwrap();
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::hours(1)).await.unwrap();
    assert!(events(&e, kinds::REPLY_OVERDUE).await.is_empty());

    turns::expect_reply(&e.hub, "botler", &child, None, None).await.unwrap();
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::minutes(19)).await.unwrap();
    assert!(events(&e, kinds::REPLY_OVERDUE).await.is_empty(), "not due before reply_within (20m)");
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::minutes(21)).await.unwrap();
    let overdue = events(&e, kinds::REPLY_OVERDUE).await;
    assert_eq!(overdue.len(), 1);
    assert_eq!(overdue[0].to.as_deref(), Some("botler"));
    assert_eq!(overdue[0].payload["instance"], child.as_str());
    let pending = e.hub.store.pending_deliveries("botler").await.unwrap();
    assert!(pending.iter().any(|d| d.event_id == overdue[0].id.to_string()), "delivered to the waiting agent");
}

// --- the reviewer -------------------------------------------------------------------------------

const TRIGGER: &str = "version = 1\n[[trigger]]\nname = \"digest\"\ncron = \"0 7 * * 1-5\"\ntarget = { resume = \"botler\" }\nprompt = \"Write my digest\"\n";

fn push_branch(e: &common::Env, branch: &str) {
    let work = e._dir.path().join(format!("agent-clone-{}", uuid::Uuid::now_v7()));
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "a")
            .env("GIT_AUTHOR_EMAIL", "a@a")
            .env("GIT_COMMITTER_NAME", "a")
            .env("GIT_COMMITTER_EMAIL", "a@a")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(e._dir.path(), &["clone", "-q", e.hub.repo.git_dir().to_str().unwrap(), work.to_str().unwrap()]);
    std::fs::create_dir_all(work.join("triggers")).unwrap();
    std::fs::write(work.join("triggers/digest.toml"), TRIGGER).unwrap();
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "change"]);
    git(&work, &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
}

/// A proposal waiting for its reviewer.
async fn in_review(e: &common::Env) -> (Proposal, String) {
    with_key(e).await;
    push_branch(e, "agents/botler/digest");
    let p = transactions::propose(&e.hub, "botler", "agents/botler/digest", "Morning digest").await.unwrap();
    let p = until("the review", || async {
        let p = e.hub.store.proposal(&p.id).await.unwrap();
        (p.status == ProposalStatus::Reviewing && p.reviewer.is_some()).then_some(p)
    })
    .await;
    let reviewer = p.reviewer.clone().unwrap();
    // The deadline is registered right after the reviewer is recorded.
    until("the review deadline", || async {
        e.hub.store.expectations_on(&reviewer).await.unwrap().first().map(|_| ())
    })
    .await;
    (p, reviewer)
}

async fn awaiting_user(e: &common::Env, id: &str) -> Proposal {
    until("the user's approval", || async {
        let p = e.hub.store.proposal(id).await.unwrap();
        (p.status == ProposalStatus::AwaitingUser).then_some(p)
    })
    .await
}

fn approval_detail(dialogs: &[botler_store::Dialog], id: &str) -> String {
    dialogs.iter().find(|d| d.proposal_id.as_deref() == Some(id)).and_then(|d| d.detail.clone()).unwrap()
}

#[tokio::test]
async fn failing_reviewer_hands_the_proposal_to_the_user() {
    let e = env().await;
    let (p, reviewer) = in_review(&e).await;
    e.fake.fail_turns(&reviewer, &["401 Unauthorized"]);
    agents::dispatch_pending(&e.hub).await.unwrap();
    let p = awaiting_user(&e, &p.id).await;
    let notes = p.review_notes.unwrap();
    assert!(notes.starts_with("reviewer unavailable: provider rejected the API key"), "{notes}");
    let dialogs = e.hub.store.open_dialogs().await.unwrap();
    assert!(approval_detail(&dialogs, &p.id).contains("reviewer unavailable"));
    // A late verdict cannot merge it behind the user's back.
    assert!(
        transactions::submit_review(&e.hub, &reviewer, &p.id, botler_core::change::Verdict::Approve, "ok")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn silent_reviewer_times_out_to_the_user() {
    let e = env().await;
    let (p, _) = in_review(&e).await;
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::minutes(10)).await.unwrap();
    assert_eq!(e.hub.store.proposal(&p.id).await.unwrap().status, ProposalStatus::Reviewing);
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::minutes(21)).await.unwrap();
    let p = awaiting_user(&e, &p.id).await;
    assert!(p.review_notes.unwrap().contains("reviewer unavailable: no answer by"));
}

#[tokio::test]
async fn review_without_a_deadline_gets_one_at_boot() {
    let e = env().await;
    let (p, reviewer) = in_review(&e).await;
    // As if the review started before the hub kept deadlines (or the row was lost).
    e.hub.store.clear_expectations(None, &reviewer).await.unwrap();
    transactions::resume_on_boot(&e.hub, vec![]).await.unwrap();
    let x = e.hub.store.expectations_on(&reviewer).await.unwrap();
    assert_eq!(x.len(), 1, "deadline restored");
    transactions::resume_on_boot(&e.hub, vec![]).await.unwrap();
    assert_eq!(e.hub.store.expectations_on(&reviewer).await.unwrap().len(), 1, "idempotent");
    turns::sweep(&e.hub, x[0].due_at).await.unwrap();
    awaiting_user(&e, &p.id).await;
}

#[tokio::test]
async fn review_timeout_after_the_user_rejected_does_nothing() {
    let e = env().await;
    let (p, _) = in_review(&e).await;
    transactions::on_approval(&e.hub, &p.id, false, Some("not now")).await.unwrap();
    turns::sweep(&e.hub, Utc::now() + chrono::Duration::minutes(21)).await.unwrap();
    let p = e.hub.store.proposal(&p.id).await.unwrap();
    assert_eq!(p.status, ProposalStatus::Rejected);
    assert_eq!(p.decision.as_deref(), Some("rejected by you while it was being reviewed"));
    assert!(p.review_notes.is_none_or(|n| !n.contains("reviewer unavailable")));
    let dialogs = e.hub.store.open_dialogs().await.unwrap();
    assert!(dialogs.iter().all(|d| d.proposal_id.as_deref() != Some(p.id.as_str())), "no approval asked");
}
