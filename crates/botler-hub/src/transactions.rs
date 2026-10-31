//! Monorepo transactions: the only way agent-authored changes reach `main`.
//!
//! propose → rebase → validate → deployment summary → gate → reviewer → decision → (user) →
//! merge → apply. Every step is recorded on the proposal and reported to the proposing agent as
//! a `proposal.updated` event. Approval binds to the pinned, gated commit: if main moves before
//! the merge, the pipeline starts over instead of merging something nobody reviewed.

use std::sync::Arc;
use std::time::Duration;

use botler_core::Source;
use botler_core::change::{ChangeSummary, Decision, Verdict, decide, preview};
use botler_core::defs::Environment;
use botler_core::payloads::{BuiltinPayload, ProposalUpdatedPayload};
use botler_runtime::Mount;
use botler_store::{
    DialogKind, NewDialog, NewProposal, Priority, Proposal, ProposalDecision, ProposalKind, ProposalStage,
    ProposalStatus,
};
use chrono::Utc;
use tokio::io::AsyncReadExt;

use crate::environment::{self, Runnable};
use crate::hub::{Hub, HubError, Live, Result, load_at};
use crate::{images, router};

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
    open(hub, instance, branch, &head, title).await
}

/// Start a proposal for `head` (on `branch`) on behalf of `instance`, which hears about its
/// progress. Also used by the hub for its own changes (template upgrades).
pub async fn open(hub: &Arc<Hub>, instance: &str, branch: &str, head: &str, title: &str) -> Result<Proposal> {
    let main = hub.repo.main().await?;
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
                head_commit: Some(head.into()),
                ..Default::default()
            },
        )
        .await?;
    hub.repo.pin(&format!("refs/proposals/{}", p.id), head).await?;
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
    if !stage(hub, id, ProposalStage::Rebase).await? {
        return Ok(());
    }
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
    // The user may have rejected it meanwhile (which also stops the gate).
    if hub.store.proposal(id).await?.status.is_terminal() {
        return Ok(());
    }
    hub.store.set_proposal_gate(id, &head, &summary, &report).await?;
    if !ok {
        return fail(hub, id, &format!("the gate failed; fix and propose again:\n```\n{report}\n```")).await;
    }

    if !stage(hub, id, ProposalStage::Review).await? {
        return Ok(());
    }
    // The reviewer is spawned while the proposal is still `gating`; it cannot submit before the
    // update below, since it first has to start and read the task.
    let reviewer = start_review(hub, id, &base, &head, &md, &report).await?;
    match reviewer {
        Some(name) => {
            hub.store.set_proposal_reviewing(id, &name).await?;
            // A reviewer that fails or goes quiet hands the decision to the user.
            crate::turns::expect_reply(hub, crate::turns::HUB, &name, None, None).await?;
            update(hub, id, "reviewing", &format!("Gate passed. Reviewer `{name}` is looking at it.")).await
        }
        // No reviewer agent defined: the user decides.
        None => decide_and_act(hub, id, "hub", None, "no reviewer agent is defined").await,
    }
}

/// Run `gate/run` from the commit in the monorepo's dev image. Returns (passed, report tail).
async fn run_gate(hub: &Hub, id: &str, commit: &str) -> Result<(bool, String)> {
    let applied = hub.applied().await;
    let tree = hub.repo.read_file(commit, "gate/run").await?;
    if tree.is_none() {
        return Ok((true, "no gate/run in this commit; nothing to check".into()));
    }
    let stopped = || Ok((false, "stopped: the proposal was decided".into()));
    if !stage(hub, id, ProposalStage::Image).await? {
        return stopped();
    }
    let image_name = if applied.defs.images.contains("dev") { "dev" } else { "agent" };
    let image = images::ensure_agent_image(hub, image_name, commit).await?;
    if !stage(hub, id, ProposalStage::Gate).await? {
        return stopped();
    }
    let dir = hub.paths.root.join("gates").join(id);
    if dir.exists() {
        tokio::fs::remove_dir_all(&dir).await?;
    }
    hub.repo.export(commit, &dir).await?;
    let command = vec!["sh".into(), "-c".into(), "cd /workspace && sh gate/run 2>&1".into()];
    let gate = Runnable::Gate { id };
    let mut spec = environment::container_spec(hub, &Environment::default(), gate, image, command, &[]).await?;
    spec.mounts.insert(0, Mount::Bind { src: dir.clone(), dst: "/workspace".into(), read_only: false });
    // Dependency downloads are cached across gate runs.
    spec.mounts.push(Mount::Volume { name: "botler-gate-uv-cache".into(), dst: "/home/agent/.cache/uv".into() });
    spec.env.insert("CI".into(), "1".into());
    // Explicit, so images built before the base image moved HOME still use the cache.
    spec.env.insert("HOME".into(), crate::agents::HOME.into());
    spec.workdir = Some("/workspace".into());
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
    if p.reviewer.as_deref() != Some(reviewer) {
        return Err(HubError::Forbidden(format!("proposal {id} is not waiting for your review")));
    }
    let v = match verdict {
        Verdict::Approve => "approve",
        Verdict::Escalate => "escalate",
        Verdict::Reject => "reject",
    };
    // The user decided while the review ran: the verdict is kept for the record, nothing more.
    if p.status == ProposalStatus::Rejected && p.decided_by.as_deref() == Some(USER) {
        return Ok(hub.store.set_proposal_review(id, v, notes).await?);
    }
    if p.status != ProposalStatus::Reviewing {
        return Err(HubError::Forbidden(format!("proposal {id} is not waiting for your review")));
    }
    hub.store.set_proposal_review(id, v, notes).await?;
    hub.store.clear_expectations(Some(crate::turns::HUB), reviewer).await?;
    decide_and_act(hub, id, reviewer, Some(verdict), notes).await
}

/// The reviewer `reviewer` failed or did not answer in time: proposals it is still reviewing go
/// to the user, with the reason. A no-op for any other instance, and for a proposal the user
/// already decided (it is no longer `reviewing`).
pub async fn reviewer_unavailable(hub: &Arc<Hub>, reviewer: &str, reason: &str) -> Result<()> {
    for p in hub.store.unfinished_proposals().await? {
        if p.status == ProposalStatus::Reviewing && p.reviewer.as_deref() == Some(reviewer) {
            tracing::warn!(proposal = %p.id, reviewer, reason, "reviewer unavailable; asking the user");
            let notes = format!("reviewer unavailable: {reason}");
            hub.store.set_proposal_review(&p.id, "unavailable", &notes).await?;
            decide_and_act(hub, &p.id, reviewer, None, &notes).await?;
        }
    }
    Ok(())
}

/// The decision table, applied. `by` is the reviewer (or `hub` without one); `verdict` is `None`
/// when there is no review (see [`decide`]).
async fn decide_and_act(hub: &Arc<Hub>, id: &str, by: &str, verdict: Option<Verdict>, notes: &str) -> Result<()> {
    let p = hub.store.proposal(id).await?;
    let summary: ChangeSummary = summary_of(&p)?;
    let decision = decide(&summary, verdict);
    let why = match (&decision, verdict) {
        (Decision::Reject, _) => "rejected by the reviewer".to_string(),
        (Decision::Merge, _) => {
            "merged without asking you: the reviewer approved and nothing in it needs your approval".to_string()
        }
        (Decision::AskUser, Some(Verdict::Approve)) => {
            format!("asked you: the reviewer approved, but {} change(s) need your approval", summary.escalations.len())
        }
        (Decision::AskUser, None) if by == "hub" => "asked you: no reviewer agent is defined".to_string(),
        (Decision::AskUser, None) => format!("asked you: {notes}"),
        (Decision::AskUser, _) => "asked you: the reviewer escalated it to you".to_string(),
    };
    let status = (decision == Decision::Reject).then_some(ProposalStatus::Rejected);
    if !hub.store.decide_proposal(id, status, &ProposalDecision { by, why: &why, reason: None }).await? {
        return Ok(()); // the user decided first
    }
    match decision {
        Decision::Reject => update(hub, id, "rejected", &format!("The reviewer rejected it:\n{notes}")).await,
        Decision::Merge => merge(hub, id).await,
        Decision::AskUser => ask_user(hub, &p, &summary, notes).await,
    }
}

fn summary_of(p: &Proposal) -> Result<ChangeSummary> {
    p.summary.clone().ok_or_else(|| HubError::Forbidden(format!("proposal {} has no summary yet", p.id)))
}

async fn ask_user(hub: &Hub, p: &Proposal, summary: &ChangeSummary, notes: &str) -> Result<()> {
    if !stage(hub, &p.id, ProposalStage::Approval).await? {
        return Ok(());
    }
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

/// `decided_by` of the user's decisions.
pub const USER: &str = "user";

/// The user decides: approve (only once it awaits them; `reason` is an optional comment) or
/// reject at any stage before the merge (`reason` required), which stops the pipeline where it
/// is. Either way the approval dialog closes and the agent hears the reason.
pub async fn on_approval(hub: &Arc<Hub>, id: &str, approved: bool, reason: Option<&str>) -> Result<()> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    let p = hub.store.proposal(id).await?;
    if p.kind == ProposalKind::Action {
        return crate::actions::on_approval(hub, &p, approved, reason).await;
    }
    if approved {
        if p.status != ProposalStatus::AwaitingUser {
            return Err(HubError::Forbidden(format!(
                "proposal {id} is not waiting for your approval; approve it once its checks finish"
            )));
        }
        let d = ProposalDecision { by: USER, why: "approved by you", reason };
        if !hub.store.decide_proposal(id, None, &d).await? {
            return Err(HubError::Forbidden(format!("proposal {id} is already decided")));
        }
        close_approval(hub, id, "approve").await?;
        return merge(hub, id).await;
    }
    let Some(reason) = reason else {
        return Err(HubError::Forbidden("say why you reject it: the agent reads the reason".into()));
    };
    let during = match p.stage {
        None | Some(ProposalStage::Rebase) => "while it was being rebased",
        Some(ProposalStage::Image) => "while the gate's image was building",
        Some(ProposalStage::Gate) => "while the gate ran",
        Some(ProposalStage::Review) => "while it was being reviewed",
        Some(ProposalStage::Approval) => "when asked",
        Some(ProposalStage::Merge) => "while merging",
    };
    let why = format!("rejected by you {during}");
    let d = ProposalDecision { by: USER, why: &why, reason: Some(reason) };
    if !hub.store.decide_proposal(id, Some(ProposalStatus::Rejected), &d).await? {
        return Err(HubError::Forbidden(format!("proposal {id} is already decided or merging")));
    }
    close_approval(hub, id, "reject").await?;
    update(hub, id, "rejected", &format!("The user rejected it: {reason}")).await?;
    // A running gate is stopped; its pipeline notices the rejection and ends.
    let gate = environment::gate_container(hub, id);
    if hub.containers.inspect(&gate).await?.is_some() {
        hub.containers.stop(&gate, 0).await?;
    }
    Ok(())
}

/// Answer the proposal's open approval dialog, if any.
pub(crate) async fn close_approval(hub: &Hub, id: &str, answer: &str) -> Result<()> {
    for d in hub.store.open_dialogs().await? {
        if d.kind == DialogKind::Approval && d.proposal_id.as_deref() == Some(id) {
            hub.store.answer_dialog(&d.id, answer).await?;
        }
    }
    Ok(())
}

/// Move an in-flight proposal to `stage` and show it live. False once it was decided (the user
/// rejected it): the pipeline then stops.
async fn stage(hub: &Hub, id: &str, stage: ProposalStage) -> Result<bool> {
    let moved = hub.store.set_proposal_stage(id, stage).await?;
    if moved {
        broadcast(hub, id).await?;
    }
    Ok(moved)
}

/// Show the proposal's current state to live viewers.
async fn broadcast(hub: &Hub, id: &str) -> Result<()> {
    let proposal = Box::new(hub.store.proposal(id).await?);
    if hub.live.send(Live::Proposal { proposal }).is_err() {
        tracing::trace!("no live viewers");
    }
    Ok(())
}

/// Fast-forward main to the reviewed commit, apply it, tell everyone.
async fn merge(hub: &Arc<Hub>, id: &str) -> Result<()> {
    // From here on a rejection cannot stop it.
    if !stage(hub, id, ProposalStage::Merge).await? {
        return Ok(());
    }
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
    if !hub.store.set_proposal_outcome(id, ProposalStatus::Failed, why).await? {
        return Ok(()); // decided meanwhile
    }
    update(hub, id, "failed", why).await
}

/// Report progress to the proposing agent (and live viewers).
async fn update(hub: &Hub, id: &str, status: &str, detail: &str) -> Result<()> {
    broadcast(hub, id).await?;
    let p = hub.store.proposal(id).await?;
    let payload = BuiltinPayload::ProposalUpdated(ProposalUpdatedPayload {
        proposal_id: id.into(),
        title: p.title,
        status: status.into(),
        detail: detail.into(),
        summary: None,
        reason: p.decision_reason,
    });
    let ev = crate::hub::builtin(payload, Source::Hub)?.to(p.instance.clone());
    hub.emit(ev).await?;
    Ok(())
}

/// A branch's deployment summary against main.
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct ChangePreview {
    pub valid: bool,
    pub diagnostics: Vec<String>,
    /// The rest is for valid changes only.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub needs_user_approval: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// The deployment summary of a branch against main, without proposing it.
pub async fn preview_branch(hub: &Hub, instance: &str, branch: &str) -> Result<ChangePreview> {
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
        return Ok(ChangePreview { valid: false, diagnostics, summary: None, needs_user_approval: None, note: None });
    };
    let old = hub.applied().await;
    let changed = hub.repo.changed_paths(&base, &head).await?;
    let summary = preview(&old.defs, &new, &changed, Utc::now());
    Ok(ChangePreview {
        valid: true,
        diagnostics,
        summary: Some(summary.to_markdown(new.timezone)),
        needs_user_approval: Some(!summary.escalations.is_empty()),
        note: Some("Relative to the current main. Proposing rebases first, so it may differ if main moves.".into()),
    })
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

/// Proposals a restart interrupted mid-pipeline. Collected before anything at boot opens new
/// ones (whose pipelines already run), and passed to [`resume_on_boot`].
pub async fn interrupted(hub: &Hub) -> Result<Vec<String>> {
    let all = hub.store.unfinished_proposals().await?;
    Ok(all.into_iter().filter(|p| p.status == ProposalStatus::Gating).map(|p| p.id).collect())
}

/// After a restart: interrupted proposals start over, unless finished meanwhile (superseded);
/// reviews in progress continue. A review without a deadline (started before deadlines existed)
/// gets one counted from when it started, so the scheduler hands it to the user once overdue.
pub async fn resume_on_boot(hub: &Arc<Hub>, interrupted: Vec<String>) -> Result<()> {
    for id in interrupted {
        if hub.store.proposal(&id).await?.status == ProposalStatus::Gating {
            tracing::info!(proposal = %id, "restarting interrupted proposal");
            spawn_pipeline(hub.clone(), id);
        }
    }
    let within = hub.applied().await.defs.instance.defaults.reply_within.as_chrono();
    for p in hub.store.unfinished_proposals().await? {
        let Some(reviewer) = p.reviewer.as_deref().filter(|_| p.status == ProposalStatus::Reviewing) else { continue };
        let waiting = hub.store.expectations_on(reviewer).await?.iter().any(|x| x.sender == crate::turns::HUB);
        if !waiting {
            tracing::info!(proposal = %p.id, reviewer, "review in progress has no deadline; adding one");
            hub.store.expect_reply(crate::turns::HUB, reviewer, None, p.updated_at + within).await?;
        }
    }
    Ok(())
}
