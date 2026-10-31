//! The proposal pipeline when main moves underneath proposals.

mod common;

use std::time::Duration;

use botler_core::change::Verdict;
use botler_hub::transactions;
use botler_store::{Proposal, ProposalStage, ProposalStatus};
use common::env;

fn trigger(name: &str) -> String {
    format!(
        "version = 1\n[[trigger]]\nname = \"{name}\"\ncron = \"0 7 * * 1-5\"\ntarget = {{ resume = \"botler\" }}\nprompt = \"Write my {name}\"\n"
    )
}

async fn start(e: &common::Env, name: &str) -> Proposal {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
    let branch = format!("agents/botler/{name}");
    let path = format!("triggers/{name}.toml");
    e.push(&branch, &[(&path, &trigger(name))]).await;
    transactions::propose(&e.hub, "botler", &branch, name).await.unwrap()
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

/// Approve whatever review it is waiting for, until it finishes.
async fn approve_until_done(e: &common::Env, id: &str) -> Proposal {
    loop {
        let p = wait_for(e, id, &[ProposalStatus::Reviewing, ProposalStatus::Merged, ProposalStatus::Failed]).await;
        if p.status != ProposalStatus::Reviewing {
            return p;
        }
        transactions::submit_review(&e.hub, p.reviewer.as_deref().unwrap(), id, Verdict::Approve, "ok").await.unwrap();
        // The merge (or its re-run) starts from here; wait until the review is no longer pending.
        for _ in 0..400 {
            let q = e.hub.store.proposal(id).await.unwrap();
            if q.status != ProposalStatus::Reviewing || q.reviewer != p.reviewer {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

#[tokio::test]
async fn a_proposal_on_an_old_main_merges_after_another_merged() {
    let e = env().await;
    let a = start(&e, "alpha").await;
    let b = start(&e, "beta").await;
    let a = approve_until_done(&e, &a.id).await;
    assert_eq!(a.status, ProposalStatus::Merged, "{:?}", a.review_notes);
    // beta was based on the main before alpha merged.
    let b = approve_until_done(&e, &b.id).await;
    assert_eq!(b.status, ProposalStatus::Merged, "{:?}", b.review_notes);
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
    e.hub.store.set_proposal_stage(&p.id, ProposalStage::Merge).await.unwrap();
    e.hub.store.set_proposal_status(&p.id, ProposalStatus::Gating).await.unwrap();
    let interrupted = transactions::interrupted(&e.hub).await.unwrap();
    assert_eq!(interrupted, std::slice::from_ref(&p.id));
    transactions::resume_on_boot(&e.hub, interrupted).await.unwrap();
    let p = approve_until_done(&e, &p.id).await;
    assert_eq!(p.status, ProposalStatus::Merged, "{:?}", p.review_notes);
}
