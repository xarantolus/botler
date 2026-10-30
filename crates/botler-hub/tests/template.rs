//! Template upgrades: a newer built-in template is proposed as a merge into main.

mod common;

use std::path::Path;

use botler_core::FRONT_DOOR;
use botler_hub::template::{self, Upgrade};
use botler_repo::TEMPLATE_REF;
use botler_store::ProposalStatus;

/// The tree of the real template with `edit` applied to `rel`.
async fn template_with(e: &common::Env, name: &str, rel: &str, edit: impl Fn(&str) -> String) -> String {
    let dir = e._dir.path().join(name);
    common::copy_dir(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template")), &dir);
    let old = std::fs::read_to_string(dir.join(rel)).unwrap();
    std::fs::write(dir.join(rel), edit(&old)).unwrap();
    e.hub.repo.write_tree_from(&dir).await.unwrap()
}

async fn pending(e: &common::Env) -> Vec<botler_store::Proposal> {
    template::pending(&e.hub).await.unwrap()
}

#[tokio::test]
async fn unchanged_template_is_no_upgrade() {
    let e = common::env().await;
    assert_eq!(template::upgrade_on_boot(&e.hub).await.unwrap(), Upgrade::UpToDate);
    assert!(pending(&e).await.is_empty());
}

#[tokio::test]
async fn template_change_opens_one_proposal_and_keeps_agent_edits() {
    let e = common::env().await;
    // The agent changed the top of the gate script; the new template changes its end.
    e.commit(&[(
        "gate/run",
        &std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/gate/run"))
            .unwrap()
            .replacen("set -eu", "set -eux", 1),
    )])
    .await;
    let tree = template_with(&e, "t1", "gate/run", |s| format!("{s}echo \"gate: really ok\"\n")).await;
    let Upgrade::Proposed { branch, proposal } = template::upgrade(&e.hub, &tree).await.unwrap() else { panic!() };
    // Idempotent: the same template again (another boot) proposes nothing.
    assert_eq!(template::upgrade(&e.hub, &tree).await.unwrap(), Upgrade::UpToDate);
    let open = pending(&e).await;
    assert_eq!(open.len(), 1);
    let p = &open[0];
    assert_eq!(
        (p.id.as_str(), p.instance.as_str(), p.branch.as_deref()),
        (proposal.as_str(), FRONT_DOOR, Some(branch.as_str()))
    );
    assert!(p.title.starts_with("Upgrade botler template files (hub v"), "{}", p.title);
    let head = e.hub.repo.rev(&format!("refs/heads/{branch}")).await.unwrap().unwrap();
    let gate = e.hub.repo.read_file(&head, "gate/run").await.unwrap().unwrap();
    assert!(gate.contains("set -eux") && gate.contains("really ok"), "{gate}");

    // A newer template (at the next boot, when no pipeline is in flight) supersedes the open upgrade.
    for _ in 0..200 {
        if e.hub.store.proposal(&proposal).await.unwrap().status != ProposalStatus::Gating {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let tree2 = template_with(&e, "t2", "README.md", |s| format!("{s}\nMore.\n")).await;
    let Upgrade::Proposed { proposal: second, .. } = template::upgrade(&e.hub, &tree2).await.unwrap() else { panic!() };
    let open = pending(&e).await;
    assert_eq!(open.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), [second.as_str()]);
    assert_eq!(e.hub.store.proposal(&proposal).await.unwrap().status, ProposalStatus::Failed);
}

#[tokio::test]
async fn conflicting_upgrade_goes_to_the_front_door() {
    let e = common::env().await;
    e.commit(&[("README.md", "ours\n")]).await;
    let tree = template_with(&e, "t1", "README.md", |_| "theirs\n".into()).await;
    let Upgrade::Conflicted { branch, paths } = template::upgrade(&e.hub, &tree).await.unwrap() else { panic!() };
    assert_eq!(paths, ["README.md"]);
    assert!(pending(&e).await.is_empty(), "nothing is proposed until someone resolves it");
    let head = e.hub.repo.rev(&format!("refs/heads/{branch}")).await.unwrap().unwrap();
    assert!(e.hub.repo.read_file(&head, "README.md").await.unwrap().unwrap().contains("<<<<<<<"));
    let events = e.hub.store.unrouted_events(100).await.unwrap();
    let ev = events.iter().find(|ev| ev.to.as_deref() == Some(FRONT_DOOR)).expect("front door told");
    let text = ev.field("text").and_then(|v| v.as_str()).unwrap();
    assert!(text.contains(&branch) && text.contains("README.md") && text.contains("propose_change"), "{text}");
    assert_eq!(template::upgrade(&e.hub, &tree).await.unwrap(), Upgrade::UpToDate, "told once");
}

#[tokio::test]
async fn repos_without_the_ref_bootstrap_from_mains_root() {
    let e = common::env().await;
    let root = e.hub.repo.rev(TEMPLATE_REF).await.unwrap().unwrap();
    e.commit(&[("README.md", "edited\n")]).await;
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(e.hub.repo.git_dir())
        .args(["update-ref", "-d", TEMPLATE_REF])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(template::snapshot(&e.hub.repo).await.unwrap().as_deref(), Some(root.as_str()));
    assert_eq!(e.hub.repo.rev(TEMPLATE_REF).await.unwrap().as_deref(), Some(root.as_str()));
    assert_eq!(template::upgrade_on_boot(&e.hub).await.unwrap(), Upgrade::UpToDate);
}
