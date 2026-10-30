//! Monorepo transactions: the only way agent-authored changes reach `main`.
//!
//! propose → rebase → validate → deployment summary → gate → reviewer → decision → (user) →
//! merge → apply. Every step is recorded on the proposal and reported to the proposing agent as
//! a `proposal.updated` event. Approval binds to the pinned, gated commit: if main moves before
//! the merge, the pipeline starts over instead of merging something nobody reviewed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use botler_core::Source;
use botler_core::change::{ChangeSummary, Decision, Verdict, decide, preview};
use botler_core::payloads::{BuiltinPayload, ProposalUpdatedPayload};
use botler_runtime::{ContainerSpec, Mount, NetworkMode};
use botler_store::{DialogKind, NewDialog, NewProposal, Priority, Proposal, ProposalKind, ProposalStatus};
use chrono::Utc;
use serde_json::json;
use tokio::io::AsyncReadExt;

use crate::hub::{Hub, HubError, Result, load_at};
use crate::{images, plumbing, router};

/// How often a proposal is re-run because main moved underneath it before giving up.
const MAX_ATTEMPTS: i64 = 3;
const GATE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const REPORT_LINES: usize = 200;
const DIFF_LIMIT: usize = 60_000;

/// Start a proposal for `branch` (pushed by `instance`). The pipeline runs in the background.
pub async fn propose(hub: &Arc<Hub>, instance: &str, branch: &str, title: &str) -> Result<Proposal> {
    let prefix = format!("agents/{instance}/");
    if !branch.starts_with(&prefix) {
        return Err(HubError::Forbidden(format!("you can only propose your own branches ({prefix}...)")));
    }
    let head = hub.repo.rev(&format!("refs/heads/{branch}")).await?.ok_or_else(|| {
        HubError::Forbidden(format!("branch {branch} is not on the hub; `git push origin HEAD:{branch}` first"))
    })?;
    let main = hub.repo.main().await?;
    if hub.repo.is_ancestor(&head, &main).await? {
        return Err(HubError::Forbidden("nothing to propose: the branch has no commits that are not on main".into()));
    }
    let p = hub
        .store
        .create_proposal(
            ProposalKind::Change,
            ProposalStatus::Gating,
            &NewProposal {
                instance: instance.into(),
                title: title.into(),
                branch: Some(branch.into()),
                base_commit: Some(main),
                head_commit: Some(head.clone()),
                ..Default::default()
            },
        )
        .await?;
    hub.repo.pin(&format!("refs/proposals/{}", p.id), &head).await?;
    spawn_pipeline(hub.clone(), p.id.clone());
    Ok(p)
}

fn spawn_pipeline(hub: Arc<Hub>, id: String) {
    tokio::spawn(async move {
        if let Err(e) = pipeline(&hub, &id).await {
            tracing::error!(proposal = %id, error = %e, "proposal pipeline failed");
            if let Err(e2) = fail(&hub, &id, &format!("internal error: {e}")).await {
                tracing::error!(proposal = %id, error = %e2, "could not record proposal failure");
            }
        }
    });
}

/// Rebase, validate, summarise, gate, then hand to the reviewer.
pub async fn pipeline(hub: &Arc<Hub>, id: &str) -> Result<()> {
    let attempt = hub.store.bump_proposal_attempts(id).await?;
    if attempt > MAX_ATTEMPTS {
        return fail(hub, id, "main kept changing while this proposal was in flight; propose again").await;
    }
    let p = hub.store.proposal(id).await?;
    hub.store.set_proposal_status(id, ProposalStatus::Gating).await?;
    let original = p.head_commit.clone().unwrap_or_default();
    let head = match hub.repo.rebase_onto_main(&original, &p.title).await {
        Ok(h) => h,
        Err(botler_repo::Error::Conflict { detail, .. }) => {
            return fail(hub, id, &format!("does not rebase onto main; pull, resolve and push again:\n{detail}")).await;
        }
        Err(e) => return Err(e.into()),
    };
    hub.repo.pin(&format!("refs/proposals/{id}"), &head).await?;
    let base = hub.repo.main().await?;

    let new = match load_at(&hub.repo, &head).await {
        Ok(a) => a,
        Err(HubError::InvalidDefinitions { diagnostics, .. }) => {
            let list: Vec<String> = diagnostics.iter().map(|d| format!("- {d}")).collect();
            return fail(hub, id, &format!("definitions do not load:\n{}", list.join("\n"))).await;
        }
        Err(e) => return Err(e),
    };
    let old = hub.applied().await;
    let changed = hub.repo.changed_paths(&base, &head).await?;
    let summary = preview(&old.defs, &new.defs, &changed, Utc::now());
    hub.store.set_proposal_gate(id, &head, &summary, "").await?;
    // The proposal records the main it was checked against; merge requires main to be unchanged.
    hub.store.set_proposal_base(id, &base).await?;
    let md = summary.to_markdown(new.defs.timezone);
    update(hub, id, "gating", &format!("Validated. Deployment summary:\n\n{md}\nRunning the gate…")).await?;

    let (ok, report) = run_gate(hub, id, &head).await?;
    hub.store.set_proposal_gate(id, &head, &summary, &report).await?;
    if !ok {
        return fail(hub, id, &format!("the gate failed; fix and propose again:\n```\n{report}\n```")).await;
    }

    // The reviewer is spawned while the proposal is still `gating`; it cannot submit before the
    // update below, since it first has to start and read the task.
    let reviewer = start_review(hub, id, &base, &head, &md, &report).await?;
    match reviewer {
        Some(name) => {
            hub.store.set_proposal_reviewing(id, &name).await?;
            update(hub, id, "reviewing", &format!("Gate passed. Reviewer `{name}` is looking at it.")).await
        }
        // No reviewer agent defined: the user decides.
        None => decide_and_act(hub, id, Verdict::Escalate, "no reviewer agent is defined").await,
    }
}

/// Run `gate/run` from the commit in the monorepo's dev image. Returns (passed, report tail).
async fn run_gate(hub: &Hub, id: &str, commit: &str) -> Result<(bool, String)> {
    let applied = hub.applied().await;
    let tree = hub.repo.read_file(commit, "gate/run").await?;
    if tree.is_none() {
        return Ok((true, "no gate/run in this commit; nothing to check".into()));
    }
    let image_name = if applied.defs.images.contains("dev") { "dev" } else { "agent" };
    let image = images::ensure_agent_image(hub, image_name, commit).await?;
    let dir = hub.paths.root.join("gates").join(id);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    hub.repo.export(commit, &dir).await?;
    let mut mounts = vec![
        Mount::Bind { src: dir.clone(), dst: "/workspace".into(), read_only: false },
        Mount::Bind { src: hub.paths.assets().join("sdk"), dst: plumbing::SDK_DIR.into(), read_only: true },
        // Dependency downloads are cached across gate runs.
        Mount::Volume { name: "botler-gate-uv-cache".into(), dst: "/home/agent/.cache/uv".into() },
    ];
    let mut env = BTreeMap::from([
        ("PYTHONPATH".to_string(), plumbing::SDK_DIR.to_string()),
        ("CI".into(), "1".into()),
        // Explicit, so images built before the base image moved HOME still use the cache.
        ("HOME".into(), crate::agents::HOME.into()),
    ]);
    crate::localtime::apply(applied.defs.timezone, Path::new(crate::localtime::ZONEINFO), &mut env, &mut mounts);
    let spec = ContainerSpec {
        name: format!("botler-{}-gate-{id}", hub.id),
        image,
        command: vec!["sh".into(), "-c".into(), "cd /workspace && sh gate/run 2>&1".into()],
        env,
        mounts,
        labels: hub.labels("gate"),
        network: NetworkMode::Default,
        workdir: Some("/workspace".into()),
        remove_on_exit: true,
        ..Default::default()
    };
    let mut att = hub.containers.run_attached(&spec).await?;
    let mut out = String::new();
    let read = tokio::time::timeout(GATE_TIMEOUT, att.stdout.read_to_string(&mut out)).await;
    let code = match read {
        Ok(Ok(_)) => match tokio::time::timeout(Duration::from_secs(30), att.exited).await {
            Ok(Ok(c)) => c,
            _ => None,
        },
        Ok(Err(e)) => return Err(HubError::Io(e)),
        Err(_) => {
            if att.kill.send(()).is_err() {
                tracing::debug!("gate already gone");
            }
            out.push_str(&format!("\n[gate timed out after {} minutes]", GATE_TIMEOUT.as_secs() / 60));
            None
        }
    };
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await {
        tracing::warn!(dir = %dir.display(), error = %e, "could not remove gate workspace");
    }
    let stderr = att.stderr.text();
    if !stderr.is_empty() {
        out.push_str(&format!("\n{stderr}"));
    }
    let report = crate::failures::tail(&out, REPORT_LINES);
    Ok((code == Some(0), report))
}

/// Spawn a fresh reviewer for the proposal. The policy comes from main, never from the change.
async fn start_review(
    hub: &Arc<Hub>,
    id: &str,
    base: &str,
    head: &str,
    md: &str,
    gate_report: &str,
) -> Result<Option<String>> {
    let applied = hub.applied().await;
    if !applied.defs.agents.contains_key("reviewer") {
        return Ok(None);
    }
    let p = hub.store.proposal(id).await?;
    let policy =
        hub.repo.read_file(base, "policies/reviewer.md").await?.unwrap_or_else(|| "(no policy file on main)".into());
    let mut diff = hub.repo.diff(base, head).await?;
    if diff.len() > DIFF_LIMIT {
        let mut end = DIFF_LIMIT;
        while !diff.is_char_boundary(end) {
            end -= 1;
        }
        diff.truncate(end);
        diff.push_str("\n… (diff truncated)");
    }
    let task = format!(
        "Review proposal `{id}`: \"{title}\" by `{by}`.\n\n\
         ## Policy (from main)\n{policy}\n\n\
         ## Deployment summary\n{md}\n\n\
         ## Gate\npassed\n```\n{gate}\n```\n\n\
         ## Diff\n```diff\n{diff}\n```\n\n\
         Decide, then call `submit_review(proposal_id=\"{id}\", verdict, notes)` exactly once. \
         verdict is `approve`, `escalate` (fine, but the user should look) or `reject`.",
        title = p.title,
        by = p.instance,
        gate = crate::failures::tail(gate_report, 30),
    );
    let name = router::spawn(hub, "reviewer", None, Some(&task), None, None).await?;
    Ok(Some(name))
}

/// The reviewer's verdict arrives (from the `submit_review` op).
pub async fn submit_review(hub: &Arc<Hub>, reviewer: &str, id: &str, verdict: Verdict, notes: &str) -> Result<()> {
    let p = hub.store.proposal(id).await?;
    if p.status != ProposalStatus::Reviewing || p.reviewer.as_deref() != Some(reviewer) {
        return Err(HubError::Forbidden(format!("proposal {id} is not waiting for your review")));
    }
    let v = match verdict {
        Verdict::Approve => "approve",
        Verdict::Escalate => "escalate",
        Verdict::Reject => "reject",
    };
    hub.store.set_proposal_review(id, v, notes).await?;
    decide_and_act(hub, id, verdict, notes).await
}

async fn decide_and_act(hub: &Arc<Hub>, id: &str, verdict: Verdict, notes: &str) -> Result<()> {
    let p = hub.store.proposal(id).await?;
    let summary: ChangeSummary = summary_of(&p)?;
    match decide(&summary, verdict) {
        Decision::Reject => {
            hub.store.set_proposal_outcome(id, ProposalStatus::Rejected, "rejected by the reviewer").await?;
            update(hub, id, "rejected", &format!("The reviewer rejected it:\n{notes}")).await
        }
        Decision::Merge => merge(hub, id).await,
        Decision::AskUser => ask_user(hub, &p, &summary, notes).await,
    }
}

fn summary_of(p: &Proposal) -> Result<ChangeSummary> {
    p.summary.clone().ok_or_else(|| HubError::Forbidden(format!("proposal {} has no summary yet", p.id)))
}

async fn ask_user(hub: &Hub, p: &Proposal, summary: &ChangeSummary, notes: &str) -> Result<()> {
    hub.store.set_proposal_status(&p.id, ProposalStatus::AwaitingUser).await?;
    let tz = hub.applied().await.defs.timezone;
    update(hub, &p.id, "awaiting_user", "Waiting for the user's approval.").await?;
    let detail = format!(
        "{}\n\n**Reviewer:** {}\n\n**Gate:** passed",
        summary.to_markdown(tz),
        if notes.is_empty() { "(none)" } else { notes }
    );
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: p.instance.clone(),
            kind: DialogKind::Approval,
            question: format!("Apply \"{}\" (from {})?", p.title, p.instance),
            detail: Some(detail),
            ctas: vec!["approve".into(), "reject".into()],
            about: vec![],
            priority: Priority::Blocking,
            proposal_id: Some(p.id.clone()),
            spec: None,
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(())
}

/// The user answered an approval dialog.
pub async fn on_approval(hub: &Arc<Hub>, proposal_id: &str, approved: bool) -> Result<()> {
    let p = hub.store.proposal(proposal_id).await?;
    if p.status != ProposalStatus::AwaitingUser {
        return Err(HubError::Forbidden(format!("proposal {proposal_id} is not waiting for approval")));
    }
    if approved {
        merge(hub, proposal_id).await
    } else {
        hub.store.set_proposal_outcome(proposal_id, ProposalStatus::Rejected, "rejected by the user").await?;
        update(hub, proposal_id, "rejected", "The user rejected it.").await
    }
}

/// Fast-forward main to the reviewed commit, apply it, tell everyone.
async fn merge(hub: &Arc<Hub>, id: &str) -> Result<()> {
    let p = hub.store.proposal(id).await?;
    let head = p.head_commit.clone().unwrap_or_default();
    let base = p.base_commit.clone().unwrap_or_default();
    match hub.repo.advance_main(&base, &head).await {
        Ok(()) => {}
        Err(botler_repo::Error::MainMoved { .. } | botler_repo::Error::NotFastForward { .. }) => {
            update(hub, id, "gating", "main changed meanwhile; re-checking against the new main.").await?;
            spawn_pipeline(hub.clone(), id.to_string());
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    }
    let applied = load_at(&hub.repo, &head).await?;
    hub.apply(applied).await?;
    hub.store.set_proposal_merged(id, &head).await?;
    tracing::info!(proposal = %id, commit = %head, "proposal merged");
    mirror(hub).await;
    update(hub, id, "merged", &format!("Merged as {} and applied.", &head[..head.len().min(12)])).await
}

/// Push main to the configured mirror; failures are logged, never fatal.
async fn mirror(hub: &Hub) {
    let Some(m) = hub.applied().await.defs.instance.mirror.clone() else { return };
    if let Err(e) = hub.repo.push_mirror(&m.url).await {
        tracing::warn!(url = %m.url, error = %e, "mirror push failed");
    }
}

async fn fail(hub: &Hub, id: &str, why: &str) -> Result<()> {
    hub.store.set_proposal_outcome(id, ProposalStatus::Failed, why).await?;
    update(hub, id, "failed", why).await
}

/// Report progress to the proposing agent.
async fn update(hub: &Hub, id: &str, status: &str, detail: &str) -> Result<()> {
    let p = hub.store.proposal(id).await?;
    let payload = BuiltinPayload::ProposalUpdated(ProposalUpdatedPayload {
        proposal_id: id.into(),
        title: p.title,
        status: status.into(),
        detail: detail.into(),
        summary: None,
    });
    let ev = crate::hub::builtin(payload, Source::Hub)?.to(p.instance.clone());
    hub.emit(ev).await?;
    Ok(())
}

/// The deployment summary of a branch against main, without proposing it.
pub async fn preview_branch(hub: &Hub, instance: &str, branch: &str) -> Result<serde_json::Value> {
    let prefix = format!("agents/{instance}/");
    if !branch.starts_with(&prefix) {
        return Err(HubError::Forbidden(format!("you can only preview your own branches ({prefix}...)")));
    }
    let head = hub
        .repo
        .rev(&format!("refs/heads/{branch}"))
        .await?
        .ok_or_else(|| HubError::Forbidden(format!("branch {branch} is not on the hub; push it first")))?;
    let base = hub.repo.main().await?;
    let tree = hub.repo.definition_tree(&head).await?;
    let report = botler_core::load_with(&tree, crate::localtime::host());
    let diagnostics: Vec<String> = report.diagnostics.iter().map(ToString::to_string).collect();
    let Some(new) = report.definitions else {
        return Ok(json!({ "valid": false, "diagnostics": diagnostics }));
    };
    let old = hub.applied().await;
    let changed = hub.repo.changed_paths(&base, &head).await?;
    let summary = preview(&old.defs, &new, &changed, Utc::now());
    Ok(json!({
        "valid": true,
        "diagnostics": diagnostics,
        "summary": summary.to_markdown(new.timezone),
        "needs_user_approval": !summary.escalations.is_empty(),
        "note": "Relative to the current main. Proposing rebases first, so it may differ if main moves.",
    }))
}

/// Revert a merged commit (user action): validated, committed and applied directly.
pub async fn revert(hub: &Hub, commit: &str) -> Result<String> {
    let base = hub.repo.main().await?;
    let head = hub.repo.revert_commit(commit).await?;
    let applied = load_at(&hub.repo, &head).await?;
    hub.repo.advance_main(&base, &head).await?;
    hub.apply(applied).await?;
    mirror(hub).await;
    tracing::info!(reverted = commit, commit = %head, "reverted by the user");
    Ok(head)
}

/// After a restart: proposals that were mid-pipeline start over; reviews in progress continue.
pub async fn resume_on_boot(hub: &Arc<Hub>) -> Result<()> {
    for p in hub.store.unfinished_proposals().await? {
        if p.status == ProposalStatus::Gating {
            tracing::info!(proposal = %p.id, "restarting interrupted proposal");
            spawn_pipeline(hub.clone(), p.id);
        }
    }
    Ok(())
}
