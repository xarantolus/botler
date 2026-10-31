//! Monorepo transactions: the only way agent-authored changes reach `main`.
//!
//! propose → rebase → validate → deployment summary → gate → reviewer → decision → (user) →
//! merge → apply. Every step is recorded on the proposal and reported to the proposing agent as
//! a `proposal.updated` event. Approval binds to the change: only a gated commit merges, so if
//! main moves before the merge the pipeline rebases and gates again, and the approval carries
//! over only if the rebased change is the same patch and needs no approval the earlier one did
//! not cover; otherwise it is reviewed (and maybe asked) again. When main moves, every proposal
//! in flight is rebased onto it right away ([`main_moved`]): one that conflicts fails at once,
//! so its agent can resolve it instead of anyone approving a dead change.

use std::sync::Arc;
use std::time::Duration;

use botler_core::Source;
use botler_core::change::{ChangeSummary, Decision, Verdict, decide, preview};
use botler_core::defs::Environment;
use botler_core::payloads::{BuiltinPayload, ProposalUpdatedPayload};
use botler_runtime::Mount;
use botler_store::{
    DialogKind, DialogStatus, NewDialog, NewProposal, Priority, Proposal, ProposalDecision, ProposalKind,
    ProposalStage, ProposalStatus,
};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use crate::environment::{self, Runnable};
use crate::hub::{Hub, HubError, Live, Result, load_at};
use crate::{images, router};

/// How often a proposal is re-run because main moved underneath it before giving up.
const MAX_ATTEMPTS: i64 = 3;
const GATE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const REPORT_LINES: usize = 200;
const DIFF_LIMIT: usize = 60_000;
/// Live progress (detail, output tail) is saved and shown at most this often.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);
const PROGRESS_LINES: usize = 40;

/// The pipeline, for agents reading a proposal; see [`explain`] for where one stands.
pub const PROCESS: &str = "A proposal goes: rebase onto main → validate (deployment summary) → gate \
(first builds the image it runs in, which can take minutes) → reviewer agent → the user, only if the \
reviewer escalates or the change needs their approval → merge and apply. If main moves before the \
merge, the proposal is rebased and checked again; an approved change that is unchanged merges \
without asking again. The user may approve early, before the checks finish: it then merges as \
soon as the gate passes, without a review (a failing gate still fails it). Never promise the user \
more than this.";

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
    let before = hub.store.proposal(id).await?;
    let main = hub.repo.main().await?;
    if before.stage == Some(ProposalStage::Merge) && !before.status.is_terminal() {
        // The hub stopped after main moved to it, before recording the merge.
        if let Some(head) = &before.head_commit
            && hub.repo.is_ancestor(head, &main).await?
        {
            hub.store.set_proposal_merged(id, head).await?;
            return update(hub, id, "merged", &format!("Merged as {}.", short(head))).await;
        }
        // Approved, then main moved under the merge (or a merge from before approvals were
        // recorded was interrupted): that approval may carry over.
        if before.approved_patch.is_none() {
            record_approval(hub, &before).await?;
        }
    }
    // Atomically back to the start, also from the merge stage (main moved under the merge, or
    // the hub restarted mid-merge): a re-run must really run.
    if !hub.store.restart_proposal(id, &format!("rebasing onto main {}", short(&main))).await? {
        tracing::info!(proposal = %id, "proposal finished before its pipeline (re-)ran");
        return Ok(());
    }
    broadcast(hub, id).await?;
    let p = hub.store.proposal(id).await?;
    // An approval from before this run carries over; one given during it is an early approval.
    let approved_at_start = p.approved_patch.is_some();
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
    let mut md = summary.to_markdown(new.defs.timezone);
    for w in crate::model_catalog::warnings(hub, &new.defs).await {
        md.push_str(&format!("- {w}\n"));
    }
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
    hub.store.set_proposal_gated(id, &head).await?;

    let again = match carry(hub, &hub.store.proposal(id).await?, &base, &head, &summary).await? {
        Carry::NoApproval => String::new(),
        Carry::Carries if !approved_at_start => {
            update(hub, id, "gating", "The gate passed and the user approved it already: merging, without a review.")
                .await?;
            return merge(hub, id).await;
        }
        Carry::Carries => {
            hub.store.carry_proposal_approval(id).await?;
            let by = hub.store.proposal(id).await?.decided_by.unwrap_or_else(|| "the reviewer".into());
            let by = if by == USER { "the user".to_string() } else { format!("`{by}`") };
            update(
                hub,
                id,
                "gating",
                &format!(
                    "Rebased onto the new main unchanged and passed the gate again: the approval (by {by}) carries over. Merging without asking again."
                ),
            )
            .await?;
            return merge(hub, id).await;
        }
        Carry::Lost(why) => format!("The earlier approval does not carry over: {why}. "),
    };

    if !stage(hub, id, ProposalStage::Review, "starting a reviewer").await? {
        return Ok(());
    }
    // The reviewer is spawned while the proposal is still `gating`; it cannot submit before the
    // update below, since it first has to start and read the task.
    let reviewer = start_review(hub, id, &base, &head, &md, &report).await?;
    match reviewer {
        Some(name) => {
            if !hub.store.set_proposal_reviewing(id, &name).await? {
                return Ok(()); // the user approved (it is merging) or rejected it meanwhile
            }
            hub.store.set_proposal_progress(id, &format!("reviewer `{name}` is reading the diff"), None).await?;
            // A reviewer that fails or goes quiet hands the decision to the user.
            crate::turns::expect_reply(hub, crate::turns::HUB, &name, None, None).await?;
            update(hub, id, "reviewing", &format!("{again}Gate passed. Reviewer `{name}` is looking at it.")).await
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
    let image_name = if applied.defs.images.contains("dev") { "dev" } else { "agent" };
    if !stage(hub, id, ProposalStage::Image, &format!("checking image {image_name}")).await? {
        return stopped();
    }
    let image = {
        let mut progress = Progress::new(hub, id, format!("building image {image_name}"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        if !applied.defs.images.contains(image_name) {
            return Err(HubError::Undefined { kind: "image", name: image_name.into() });
        }
        let recipe = images::Recipe::agent(image_name);
        let build = images::ensure_with(hub, &recipe, commit, Some(&tx));
        tokio::pin!(build);
        loop {
            tokio::select! {
                built = &mut build => break built?,
                Some(line) = rx.recv() => progress.line(&line).await?,
            }
        }
    };
    if !stage(hub, id, ProposalStage::Gate, "starting the gate").await? {
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
    let mut progress = Progress::new(hub, id, "gate running".into());
    let read = tokio::time::timeout(GATE_TIMEOUT, async {
        let mut lines = BufReader::new(&mut att.stdout).lines();
        while let Some(line) = lines.next_line().await? {
            out.push_str(&line);
            out.push('\n');
            progress.line(&line).await?;
        }
        Ok::<_, HubError>(())
    })
    .await;
    let code = match read {
        Ok(Ok(())) => match tokio::time::timeout(Duration::from_secs(30), att.exited).await {
            Ok(Ok(c)) => c,
            _ => None,
        },
        Ok(Err(e)) => return Err(e),
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
    // The user decided while the review ran (rejected it, or approved it, which outranks the
    // reviewer): the verdict is kept for the record, nothing more.
    let by_user = p.decided_by.as_deref() == Some(USER);
    if by_user && (p.status == ProposalStatus::Rejected || p.approved_patch.is_some()) {
        hub.store.clear_expectations(Some(crate::turns::HUB), reviewer).await?;
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
    if !stage(hub, &p.id, ProposalStage::Approval, "waiting for your approval").await? {
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
/// `decision` of an approval given before the proposal asked for it.
pub const EARLY: &str = "approved by you before the checks finished";

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
        // Early: before it asks. The approval covers this patch and its escalations; it still
        // has to pass the gate, and it carries over clean rebases like any approval.
        let early = p.status != ProposalStatus::AwaitingUser;
        if early {
            if p.status.is_terminal() {
                return Err(HubError::Forbidden(format!("proposal {id} is already decided")));
            }
            if p.stage == Some(ProposalStage::Merge) {
                return Err(HubError::Forbidden(format!("proposal {id} is already merging")));
            }
            if p.summary.is_none() || matches!(p.stage, None | Some(ProposalStage::Rebase)) {
                return Err(HubError::Forbidden(format!(
                    "proposal {id} is not validated yet; approve it in a few seconds, once its summary shows"
                )));
            }
        }
        let why = if early { EARLY } else { "approved by you" };
        if !hub.store.decide_proposal(id, None, &ProposalDecision { by: USER, why, reason }).await? {
            return Err(HubError::Forbidden(format!("proposal {id} is already decided")));
        }
        if early {
            record_approval(hub, &p).await?;
        }
        close_approval(hub, id, "approve").await?;
        let said = reason.map(|r| format!(" (\"{r}\")")).unwrap_or_default();
        // Not gated yet: the pipeline merges it once the gate passes.
        if early && (p.gated_commit.is_none() || p.gated_commit != p.head_commit) {
            let detail = format!(
                "The user approved it{said} before the checks finished; it merges as soon as the gate passes, without a review."
            );
            return update(hub, id, "approved", &detail).await;
        }
        update(hub, id, "approved", &format!("The user approved it{said}; merging.")).await?;
        if let Some(reviewer) = p.reviewer.as_deref().filter(|_| p.status == ProposalStatus::Reviewing) {
            hub.store.clear_expectations(Some(crate::turns::HUB), reviewer).await?;
        }
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
/// rejected it): the pipeline then stops. Any other refusal (it is merging) is a bug that would
/// leave the proposal stuck, so it is an error, which fails the proposal visibly.
async fn stage(hub: &Hub, id: &str, stage: ProposalStage, detail: &str) -> Result<bool> {
    if hub.store.set_proposal_stage(id, stage, detail).await? {
        broadcast(hub, id).await?;
        return Ok(true);
    }
    let p = hub.store.proposal(id).await?;
    if p.status.is_terminal() {
        return Ok(false);
    }
    let from = p.stage.map_or("none", ProposalStage::as_str);
    Err(HubError::StageRefused {
        id: id.into(),
        to: stage.as_str(),
        detail: format!("it is {} at stage {from}", p.status.as_str()),
    })
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
    if !stage(hub, id, ProposalStage::Merge, "fast-forwarding main").await? {
        return Ok(());
    }
    let p = hub.store.proposal(id).await?;
    // What merges now is what was approved; it carries over a clean rebase.
    record_approval(hub, &p).await?;
    let head = p.head_commit.clone().unwrap_or_default();
    let base = p.base_commit.clone().unwrap_or_default();
    // Only the gated commit merges: one rebased since (main moved) is gated first.
    let moved = p.gated_commit.as_deref() != Some(head.as_str())
        || match hub.repo.advance_main(&base, &head).await {
            Ok(()) => false,
            Err(botler_repo::Error::MainMoved { .. } | botler_repo::Error::NotFastForward { .. }) => true,
            Err(e) => return Err(e.into()),
        };
    if moved {
        let detail = "main changed meanwhile; rebasing and re-checking against the new main. \
                      If the change is unchanged, the approval carries over and it merges without asking again.";
        update(hub, id, "gating", detail).await?;
        spawn_pipeline(hub.clone(), id.to_string());
        return Ok(());
    }
    hub.store.set_proposal_progress(id, "applying", None).await?;
    let applied = load_at(&hub.repo, &head).await?;
    hub.apply(applied).await?;
    hub.store.set_proposal_merged(id, &head).await?;
    tracing::info!(proposal = %id, commit = %head, "proposal merged");
    mirror(hub).await;
    update(hub, id, "merged", &format!("Merged as {} and applied.", short(&head))).await?;
    after_main_moved(hub).await;
    Ok(())
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

/// Record what is approved: the change's patch id and the escalations its summary lists.
async fn record_approval(hub: &Hub, p: &Proposal) -> Result<()> {
    let (Some(base), Some(head)) = (&p.base_commit, &p.head_commit) else { return Ok(()) };
    let Some(patch) = hub.repo.patch_id(base, head).await? else { return Ok(()) };
    let escalations = p.summary.as_ref().map(|s| s.escalations.as_slice()).unwrap_or_default();
    Ok(hub.store.set_proposal_approval(&p.id, Some((&patch, escalations))).await?)
}

enum Carry {
    NoApproval,
    Carries,
    /// Why not; the approval is dropped.
    Lost(String),
}

/// Whether the earlier approval of `p` covers the change rebased to `base..head` with `summary`:
/// the same patch, and nothing needing approval that the earlier one did not cover.
async fn carry(hub: &Hub, p: &Proposal, base: &str, head: &str, summary: &ChangeSummary) -> Result<Carry> {
    let Some(approved) = p.approved_patch.as_deref() else { return Ok(Carry::NoApproval) };
    let why = if hub.repo.patch_id(base, head).await?.as_deref() != Some(approved) {
        "rebased onto the new main, the change is not the same patch any more".to_string()
    } else {
        let covered = p.approved_escalations.as_deref().unwrap_or_default();
        let new: Vec<String> =
            summary.escalations.iter().filter(|e| !covered.contains(e)).map(|e| e.explain()).collect();
        if new.is_empty() {
            return Ok(Carry::Carries);
        }
        format!("on the new main it also needs approval because it {}", new.join("; "))
    };
    hub.store.set_proposal_approval(&p.id, None).await?;
    Ok(Carry::Lost(why))
}

/// [`main_moved`], logged instead of failing whatever moved main.
pub async fn after_main_moved(hub: &Hub) {
    if let Err(e) = main_moved(hub).await {
        tracing::error!(error = %e, "could not rebase the proposals in flight onto the new main");
    }
}

/// Main moved: rebase every proposal in flight onto it now. One that conflicts fails, so its
/// agent hears it (and can resolve it) and nobody approves a dead change. One waiting for its
/// reviewer or the user that rebases to the same patch gets the new head pinned; it is gated
/// again at merge time. One whose pipeline runs (`gating`) is only checked: the pipeline rebases
/// it again before merging.
pub async fn main_moved(hub: &Hub) -> Result<()> {
    static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE_AT_A_TIME.lock().await;
    let main = hub.repo.main().await?;
    for p in hub.store.in_flight_proposals().await? {
        // Not checked yet, or merging: its own pipeline deals with the new main.
        if matches!(p.stage, None | Some(ProposalStage::Rebase | ProposalStage::Merge)) {
            continue;
        }
        let (Some(base), Some(head)) = (p.base_commit.as_deref(), p.head_commit.as_deref()) else { continue };
        if base == main {
            continue;
        }
        let rebased = match hub.repo.rebase_onto_main(head, &p.title).await {
            Ok(h) => h,
            Err(botler_repo::Error::Conflict { detail, .. }) => {
                let why = format!(
                    "main moved and this change no longer rebases onto it. Rebase your branch onto main, \
                     resolve the conflict, push and propose again:\n{detail}"
                );
                fail(hub, &p.id, &why).await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        if p.status == ProposalStatus::Gating
            || hub.repo.patch_id(&main, &rebased).await? != hub.repo.patch_id(base, head).await?
        {
            continue;
        }
        if hub.store.set_proposal_rebased(&p.id, &main, &rebased).await? {
            hub.repo.pin(&format!("refs/proposals/{}", p.id), &rebased).await?;
            broadcast(hub, &p.id).await?;
        }
    }
    Ok(())
}

/// The current stage's live detail and output tail, saved and shown at most every
/// [`PROGRESS_EVERY`] (the agent hears only about stage changes).
struct Progress<'a> {
    hub: &'a Hub,
    id: &'a str,
    what: String,
    /// The image build's current `STEP n/m: …`.
    step: Option<String>,
    tail: std::collections::VecDeque<String>,
    shown: Option<std::time::Instant>,
}

impl<'a> Progress<'a> {
    fn new(hub: &'a Hub, id: &'a str, what: String) -> Self {
        Self { hub, id, what, step: None, tail: Default::default(), shown: None }
    }

    async fn line(&mut self, line: &str) -> Result<()> {
        let line = line.trim_end();
        if line.is_empty() {
            return Ok(());
        }
        if line.starts_with("STEP ") {
            self.step = Some(clip(line, 100));
        }
        if self.tail.len() == PROGRESS_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(clip(line, 300));
        if self.shown.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY) {
            self.shown = Some(std::time::Instant::now());
            let detail = match (&self.step, self.tail.back()) {
                (Some(step), _) => format!("{} ({step})", self.what),
                (None, Some(last)) => format!("{}, last line: {}", self.what, clip(last, 100)),
                (None, None) => self.what.clone(),
            };
            let log = self.tail.iter().map(String::as_str).collect::<Vec<_>>().join("\n");
            self.hub.store.set_proposal_progress(self.id, &detail, Some(&log)).await?;
            broadcast(self.hub, self.id).await?;
        }
        Ok(())
    }
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Where `p` stands, in a sentence or two: what happens now. `for_agent` adds whether its agent
/// needs to do anything. See [`PROCESS`].
pub fn explain(p: &Proposal, for_agent: bool) -> String {
    let by = |who: Option<&str>| match who {
        Some(USER) => "the user".to_string(),
        Some(who) => format!("`{who}`"),
        None => "the reviewer".to_string(),
    };
    let wait = "Nothing for you to do; you hear about every step as proposal.updated.";
    let (now, todo) = match (p.status, p.stage) {
        (ProposalStatus::Merged, _) => {
            (format!("Merged as {} and applied: it is live.", short(p.merged_commit.as_deref().unwrap_or("?"))), "")
        }
        (ProposalStatus::Rejected, _) => (
            format!(
                "Rejected by {}: {}.",
                by(p.decided_by.as_deref()),
                p.decision_reason.as_deref().or(p.verdict.as_deref()).unwrap_or("see the notes")
            ),
            "Address that before proposing again; do not propose it unchanged.",
        ),
        (ProposalStatus::Failed, _) => (
            "Failed; the notes say why.".to_string(),
            "Fix the problem on your branch, push, and call propose_change again.",
        ),
        (ProposalStatus::Approved, _) => ("Approved.".to_string(), ""),
        (_, Some(ProposalStage::Merge)) => (format!("Approved by {}; merging now.", by(p.decided_by.as_deref())), wait),
        (ProposalStatus::AwaitingUser, _) => (
            format!(
                "Checked and reviewed; waiting for the user to approve or reject it ({}).",
                p.decision.as_deref().map_or("it needs their approval", |d| d.trim_start_matches("asked you: "))
            ),
            wait,
        ),
        (ProposalStatus::Reviewing, _) => {
            (format!("The gate passed; reviewer {} is reviewing it.", by(p.reviewer.as_deref())), wait)
        }
        (ProposalStatus::Gating, stage) => {
            let doing = match stage {
                None | Some(ProposalStage::Rebase) => "being rebased onto main and validated",
                Some(ProposalStage::Image) => "building the image the gate runs in (this can take minutes)",
                Some(ProposalStage::Gate) => "running the gate",
                _ => "being checked",
            };
            let now = if p.approved_patch.is_some() && p.decision.as_deref() == Some(EARLY) {
                format!(
                    "Approved by the user before the checks finished; it is {doing} and merges as soon as the gate passes."
                )
            } else if p.approved_patch.is_some() {
                format!(
                    "Approved by {}, but main moved before the merge, so it is {doing} again. If the change is unchanged it merges without asking again; otherwise it is reviewed again.",
                    by(p.decided_by.as_deref())
                )
            } else {
                format!("It is {doing}; then a reviewer looks at it.")
            };
            (now, wait)
        }
    };
    if for_agent && !todo.is_empty() { format!("{now} {todo}") } else { now }
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
    // Nobody is asked about a dead proposal.
    let reason = format!("the proposal failed: {}", why.lines().next().unwrap_or(why));
    for d in hub.store.open_dialogs().await? {
        if d.kind == DialogKind::Approval && d.proposal_id.as_deref() == Some(id) {
            hub.store.close_dialog(&d.id, DialogStatus::Aborted, &reason).await?;
        }
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
    let mut diagnostics: Vec<String> = report.diagnostics.iter().map(ToString::to_string).collect();
    if let Some(defs) = &report.definitions {
        diagnostics.extend(crate::model_catalog::warnings(hub, defs).await.iter().map(ToString::to_string));
    }
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
    after_main_moved(hub).await;
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
