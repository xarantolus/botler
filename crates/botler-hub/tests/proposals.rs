//! The proposal pipeline when main moves underneath proposals.

mod common;

use std::time::Duration;

use botler_core::change::Verdict;
use botler_hub::{agents, router, transactions};
use botler_store::{DialogKind, InstanceState, Proposal, ProposalStage, ProposalStatus};
use common::env;

fn trigger(name: &str) -> String {
    format!(
        "version = 1\n[[trigger]]\nname = \"{name}\"\ncron = \"0 7 * * 1-5\"\ntarget = {{ resume = \"botler\" }}\nprompt = \"Write my {name}\"\n"
    )
}

/// A capability change: the user has to approve it.
const GRANT: &str =
    "version = 1\nkind = \"persistent\"\nalways_on = true\nimage = \"agent\"\nsecrets = [\"openrouter\"]\n";

async fn propose(e: &common::Env, by: &str, name: &str, files: &[(&str, &str)]) -> Proposal {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    let branch = format!("agents/{by}/{name}");
    let head = e.push(&branch, files).await;
    transactions::open(&e.hub, by, &branch, &head, name).await.unwrap()
}

async fn start(e: &common::Env, name: &str) -> Proposal {
    propose(e, "botler", name, &[(&format!("triggers/{name}.toml"), &trigger(name))]).await
}

/// Wait until the proposal settles in one of `want`: a stuck proposal fails the test.
async fn wait_for(e: &common::Env, id: &str, want: &[ProposalStatus]) -> Proposal {
    for _ in 0..400 {
        let p = e.hub.store.proposal(id).await.unwrap();
        let settled = p.status != ProposalStatus::Reviewing || p.reviewer.is_some();
        if want.contains(&p.status) && settled {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let p = e.hub.store.proposal(id).await.unwrap();
    panic!("proposal stuck at {:?}/{:?}: {:?}", p.status, p.stage, p.review_notes);
}

async fn approve(e: &common::Env, p: &Proposal) {
    transactions::submit_review(&e.hub, p.reviewer.as_deref().unwrap(), &p.id, Verdict::Approve, "ok").await.unwrap();
}

async fn approval_dialogs(e: &common::Env, id: &str) -> usize {
    let open = e.hub.store.open_dialogs().await.unwrap();
    open.iter().filter(|d| d.kind == DialogKind::Approval && d.proposal_id.as_deref() == Some(id)).count()
}

#[tokio::test]
async fn a_proposal_on_an_old_main_merges_after_another_merged_without_a_second_review() {
    let e = env().await;
    let a = start(&e, "alpha").await;
    let b = start(&e, "beta").await;
    let a = wait_for(&e, &a.id, &[ProposalStatus::Reviewing]).await;
    let b = wait_for(&e, &b.id, &[ProposalStatus::Reviewing]).await;
    approve(&e, &a).await;
    let a = wait_for(&e, &a.id, &[ProposalStatus::Merged]).await;
    // beta was checked against the main before alpha merged; it was rebased right away.
    let moved = e.hub.store.proposal(&b.id).await.unwrap();
    assert_eq!(moved.base_commit, a.merged_commit);
    assert_ne!(moved.head_commit, b.head_commit);
    approve(&e, &b).await;
    let b = wait_for(&e, &b.id, &[ProposalStatus::Merged, ProposalStatus::Failed]).await;
    assert_eq!(b.status, ProposalStatus::Merged, "{:?}", b.review_notes);
    assert_eq!(b.carried_over, 1, "re-gated, and the approval carried over");
    assert!(b.stages.iter().filter(|s| s.stage == ProposalStage::Gate).count() == 2, "{:?}", b.stages);
    assert!(b.stages.iter().filter(|s| s.stage == ProposalStage::Review).count() == 1, "reviewed once");
    let defs = &e.hub.applied().await.defs;
    assert!(defs.triggers.keys().any(|k| k.ends_with("#alpha")) && defs.triggers.keys().any(|k| k.ends_with("#beta")));
}

#[tokio::test]
async fn a_proposal_interrupted_mid_merge_recovers_on_restart() {
    let e = env().await;
    let p = start(&e, "alpha").await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    // What the live hub had: approved, stuck at gating/merge after main moved.
    e.commit(&[("README.md", "main moved\n")]).await;
    e.hub.store.set_proposal_stage(&p.id, ProposalStage::Merge, "").await.unwrap();
    e.hub.store.set_proposal_status(&p.id, ProposalStatus::Gating).await.unwrap();
    let interrupted = transactions::interrupted(&e.hub).await.unwrap();
    assert_eq!(interrupted, std::slice::from_ref(&p.id));
    transactions::resume_on_boot(&e.hub, interrupted).await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Merged, ProposalStatus::Failed]).await;
    assert_eq!(p.status, ProposalStatus::Merged, "{:?}", p.review_notes);
    assert_eq!(p.carried_over, 1);
}

#[tokio::test]
async fn a_changed_patch_loses_the_approval_and_is_reviewed_again() {
    let e = env().await;
    let p = start(&e, "alpha").await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    e.commit(&[("README.md", "main moved\n")]).await;
    e.hub.store.set_proposal_stage(&p.id, ProposalStage::Merge, "").await.unwrap();
    e.hub.store.set_proposal_approval(&p.id, Some(("some other patch", &[]))).await.unwrap();
    transactions::pipeline(&e.hub, &p.id).await.unwrap();
    let again = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    assert_ne!(again.reviewer, p.reviewer, "a new review");
    assert_eq!(again.approved_patch, None);
    e.route().await;
    let updates = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some("proposal.updated".into()), limit: 20, ..Default::default() })
        .await
        .unwrap();
    assert!(updates.iter().any(|u| u.payload["detail"].as_str().unwrap().contains("not the same patch")));
}

#[tokio::test]
async fn a_conflict_with_the_new_main_fails_the_proposal_and_wakes_its_agent() {
    let e = env().await;
    let chat = router::spawn(&e.hub, "dev", Some("botler"), Some("start"), None, None).await.unwrap();
    let p = propose(&e, &chat, "grant", &[("agents/botler/agent.toml", GRANT)]).await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    approve(&e, &p).await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::AwaitingUser]).await;
    assert_eq!(approval_dialogs(&e, &p.id).await, 1);
    agents::reap_idle(&e.hub, chrono::Utc::now() + chrono::Duration::hours(2)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Dormant);

    e.commit(&[(
        "agents/botler/agent.toml",
        "version = 1\nkind = \"persistent\"\nalways_on = false\nimage = \"agent\"\n",
    )])
    .await;
    transactions::main_moved(&e.hub).await.unwrap();
    let p = e.hub.store.proposal(&p.id).await.unwrap();
    assert_eq!(p.status, ProposalStatus::Failed);
    assert!(p.review_notes.unwrap().contains("no longer rebases"));
    assert_eq!(approval_dialogs(&e, &p.id).await, 0, "the user is not asked about a dead proposal");

    e.route().await;
    agents::dispatch_pending(&e.hub).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(e.hub.store.instance(&chat).await.unwrap().state, InstanceState::Running, "woken");
    let prompts = e.fake.prompts(&agents::container_name(&e.hub, &chat));
    assert!(prompts.iter().any(|t| t.contains("no longer rebases")), "{prompts:?}");
}

#[tokio::test]
async fn a_clean_rebase_keeps_an_awaiting_proposal_awaiting_and_the_approval_is_asked_once() {
    let e = env().await;
    let p = propose(&e, "botler", "grant", &[("agents/botler/agent.toml", GRANT)]).await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    approve(&e, &p).await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::AwaitingUser]).await;
    e.commit(&[("README.md", "main moved\n")]).await;
    transactions::main_moved(&e.hub).await.unwrap();
    let q = e.hub.store.proposal(&p.id).await.unwrap();
    assert_eq!(q.status, ProposalStatus::AwaitingUser);
    assert_eq!(q.base_commit.as_deref(), Some(e.hub.repo.main().await.unwrap().as_str()));
    assert_ne!(q.head_commit, p.head_commit, "the pinned head moved");
    assert_eq!(approval_dialogs(&e, &p.id).await, 1, "still asking");
    let pinned = e.hub.repo.rev(&format!("refs/proposals/{}", p.id)).await.unwrap();
    assert_eq!(pinned, q.head_commit);

    transactions::on_approval(&e.hub, &p.id, true, None).await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Merged, ProposalStatus::Failed]).await;
    assert_eq!(p.status, ProposalStatus::Merged, "{:?}", p.review_notes);
    assert_eq!(p.carried_over, 1, "gated again, merged without asking again");
    assert_eq!(approval_dialogs(&e, &p.id).await, 0);
    let asked = e.hub.store.recent_dialogs(50).await.unwrap();
    assert_eq!(asked.iter().filter(|d| d.proposal_id.as_deref() == Some(p.id.as_str())).count(), 1);
}

#[tokio::test]
async fn explain_says_where_it_stands_and_who_acts() {
    let e = env().await;
    let p = start(&e, "alpha").await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    assert!(
        transactions::explain(&p, true).contains("is reviewing it. Nothing for you to do"),
        "{}",
        transactions::explain(&p, true)
    );
    let mut q = p.clone();
    q.status = ProposalStatus::Gating;
    q.stage = Some(ProposalStage::Gate);
    q.approved_patch = Some("x".into());
    q.decided_by = Some(transactions::USER.into());
    let s = transactions::explain(&q, true);
    assert!(s.starts_with("Approved by the user, but main moved") && s.contains("without asking again"), "{s}");
    q.status = ProposalStatus::Failed;
    assert!(transactions::explain(&q, true).contains("propose_change again"));
    assert!(!transactions::explain(&q, false).contains("propose_change"), "the user is not told what the agent does");
    q.status = ProposalStatus::AwaitingUser;
    q.stage = Some(ProposalStage::Approval);
    assert!(transactions::explain(&q, true).contains("waiting for the user"));
    approve(&e, &p).await;
    let p = wait_for(&e, &p.id, &[ProposalStatus::Merged]).await;
    assert!(transactions::explain(&p, true).starts_with("Merged as"));
}
