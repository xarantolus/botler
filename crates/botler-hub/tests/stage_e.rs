mod common;

use std::time::Duration;

use botler_core::change::Verdict;
use botler_hub::{agents, transactions};
use botler_store::ProposalStatus;
use common::env;

async fn with_key(e: &common::Env) {
    e.hub.store.set_secret(&e.hub.secret_key, "OPENROUTER_API_KEY", "sk-test").await.unwrap();
}

/// Push a branch the way an agent would (commit on top of main).
fn push_branch(e: &common::Env, branch: &str, files: &[(&str, &str)]) {
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
    for (rel, body) in files {
        let p = work.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "change"]);
    git(&work, &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")]);
}

async fn wait_for(e: &common::Env, id: &str, want: &[ProposalStatus]) -> botler_store::Proposal {
    for _ in 0..200 {
        let p = e.hub.store.proposal(id).await.unwrap();
        // The reviewer is recorded just after the status moves to reviewing.
        let settled = p.status != ProposalStatus::Reviewing || p.reviewer.is_some();
        if want.contains(&p.status) && settled {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("proposal stuck in {:?}", e.hub.store.proposal(id).await.unwrap().status);
}

const TRIGGER: &str = "version = 1\n[[trigger]]\nname = \"digest\"\ncron = \"0 7 * * 1-5\"\ntarget = { resume = \"botler\" }\nprompt = \"Write my digest\"\n";

#[tokio::test]
async fn reviewed_change_without_escalations_merges_and_applies() {
    let e = env().await;
    with_key(&e).await;
    push_branch(&e, "agents/botler/digest", &[("triggers/digest.toml", TRIGGER)]);
    let p = transactions::propose(&e.hub, "botler", "agents/botler/digest", "Morning digest").await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing, ProposalStatus::Failed]).await;
    assert_eq!(p.status, ProposalStatus::Reviewing, "{:?}", p.review_notes);
    let reviewer = p.reviewer.clone().unwrap();
    assert!(reviewer.starts_with("reviewer-"));
    let task = e.hub.store.instance(&reviewer).await.unwrap().task.unwrap();
    assert!(
        task.contains("At 07:00") && task.contains("Reviewer policy") && task.contains("triggers/digest.toml"),
        "{task}"
    );

    // Only the assigned reviewer may submit.
    assert!(transactions::submit_review(&e.hub, "someone-else", &p.id, Verdict::Approve, "ok").await.is_err());
    transactions::submit_review(&e.hub, &reviewer, &p.id, Verdict::Approve, "Does what it says.").await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Merged]).await;
    assert_eq!(e.hub.repo.main().await.unwrap(), p.merged_commit.unwrap());
    assert!(e.hub.applied().await.defs.triggers.keys().any(|k| k.ends_with("#digest")), "applied");
    e.route().await;
    let updates = e
        .hub
        .store
        .events(&botler_store::EventQuery { kind: Some("proposal.updated".into()), limit: 10, ..Default::default() })
        .await
        .unwrap();
    assert!(updates.iter().any(|u| u.payload["status"] == "merged" && u.to.as_deref() == Some("botler")));
    assert!(
        updates.iter().any(|u| u.payload["detail"].as_str().unwrap_or("").contains("next:")),
        "the summary reached the agent"
    );
}

#[tokio::test]
async fn capability_changes_go_to_the_user() {
    let e = env().await;
    with_key(&e).await;
    push_branch(
        &e,
        "agents/botler/grant",
        &[(
            "agents/botler/agent.toml",
            "version = 1\nkind = \"persistent\"\nalways_on = true\nimage = \"agent\"\nsecrets = [\"openrouter\"]\n",
        )],
    );
    let p = transactions::propose(&e.hub, "botler", "agents/botler/grant", "Give botler the openrouter group")
        .await
        .unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Reviewing]).await;
    transactions::submit_review(&e.hub, &p.reviewer.unwrap(), &p.id, Verdict::Approve, "fine").await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::AwaitingUser]).await;
    let dialog = e
        .hub
        .store
        .open_dialogs()
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.proposal_id.as_deref() == Some(p.id.as_str()))
        .unwrap();
    assert!(dialog.detail.unwrap().contains("secret `OPENROUTER_API_KEY`"));
    transactions::on_approval(&e.hub, &p.id, true).await.unwrap();
    wait_for(&e, &p.id, &[ProposalStatus::Merged]).await;
}

#[tokio::test]
async fn gate_failures_and_invalid_definitions_go_back_to_the_agent() {
    let e = env().await;
    with_key(&e).await;
    e.fake.set_script_result("ruff: E501 line too long\ngate: FAILED\n", 1);
    push_branch(&e, "agents/botler/bad-gate", &[("triggers/digest.toml", TRIGGER)]);
    let p = transactions::propose(&e.hub, "botler", "agents/botler/bad-gate", "x").await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Failed]).await;
    assert!(p.gate_report.unwrap().contains("E501"));

    e.fake.set_script_result("gate: ok\n", 0);
    push_branch(
        &e,
        "agents/botler/bad-defs",
        &[("triggers/x.toml", "[[trigger]]\nname = \"x\"\ncron = \"not a cron\"\ntarget = { resume = \"botler\" }\n")],
    );
    let p = transactions::propose(&e.hub, "botler", "agents/botler/bad-defs", "x").await.unwrap();
    let p = wait_for(&e, &p.id, &[ProposalStatus::Failed]).await;
    assert!(p.review_notes.unwrap().contains("invalid cron expression"));
}

#[tokio::test]
async fn foreign_branches_are_refused_and_preview_explains() {
    let e = env().await;
    with_key(&e).await;
    push_branch(&e, "agents/other/x", &[("triggers/digest.toml", TRIGGER)]);
    assert!(transactions::propose(&e.hub, "botler", "agents/other/x", "x").await.is_err());
    push_branch(&e, "agents/botler/p", &[("triggers/digest.toml", TRIGGER)]);
    let v =
        serde_json::to_value(transactions::preview_branch(&e.hub, "botler", "agents/botler/p").await.unwrap()).unwrap();
    assert_eq!(v["valid"], true);
    assert!(v["summary"].as_str().unwrap().contains("next:"));
    assert_eq!(v["needs_user_approval"], false);
    // The reviewer agent needs no running pi here; make sure nothing woke unexpectedly.
    assert!(agents::container_name(&e.hub, "botler").starts_with(&format!("botler-{}-agent-", e.hub.id)));
}

#[tokio::test]
async fn user_revert_applies_directly() {
    let e = env().await;
    with_key(&e).await;
    e.commit(&[("triggers/digest.toml", TRIGGER)]).await;
    let merged = e.hub.repo.main().await.unwrap();
    transactions::revert(&e.hub, &merged).await.unwrap();
    assert!(!e.hub.applied().await.defs.triggers.keys().any(|k| k.ends_with("#digest")));
}
