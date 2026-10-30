//! Proposals: changes to the monorepo, their previews and reviews (see `transactions.rs`).

use std::sync::Arc;

use botler_core::change::Verdict;
use botler_core::defs::Capability;
use botler_store::ProposalStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;
use crate::transactions::{self, ChangePreview};

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ProposeChange {
    /// Your pushed branch, e.g. `agents/botler/work`.
    pub branch: String,
    /// One line saying what the change does.
    pub title: String,
}

#[derive(Debug, Serialize, TS)]
pub struct Proposed {
    pub proposal_id: String,
    pub status: ProposalStatus,
    pub note: String,
}

impl Op for ProposeChange {
    const NAME: &'static str = "propose_change";
    const DESCRIPTION: &'static str = "Propose your pushed branch (agents/<you>/...) for merging into main. Runs asynchronously: rebase, validation, a plain-language deployment summary (what will run when, what needs the user's approval), the gate (gate/run), a reviewer, maybe the user. Progress arrives as proposal.updated events; nothing takes effect until it is merged.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = Proposed;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Proposed, OpError> {
        let instance = caller.agent("only agents propose")?;
        if self.title.trim().is_empty() {
            return Err(OpError::BadRequest("give the change a title".into()));
        }
        let p = transactions::propose(hub, instance, &self.branch, self.title.trim()).await?;
        Ok(Proposed {
            proposal_id: p.id,
            status: ProposalStatus::Gating,
            note: "Progress arrives as proposal.updated events. Wait for them before relying on the change.".into(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct PreviewChange {
    pub branch: String,
}

impl Op for PreviewChange {
    const NAME: &'static str = "preview_change";
    const DESCRIPTION: &'static str = "The deployment summary of your pushed branch against main without proposing it: validation problems, triggers in plain words with next fire times, new capabilities, and whether the user must approve.";
    const WHO: Who = Who::UserAndAgents;
    type Output = ChangePreview;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<ChangePreview, OpError> {
        let instance = caller.agent("only agents preview")?;
        Ok(transactions::preview_branch(hub, instance, &self.branch).await?)
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GetProposal {
    pub id: String,
}

/// A proposal as agents and the user read it, with its deployment summary as markdown.
#[derive(Debug, Serialize, TS)]
pub struct ProposalDetail {
    pub id: String,
    pub title: String,
    pub status: ProposalStatus,
    pub branch: Option<String>,
    pub summary: Option<String>,
    pub gate_report: Option<String>,
    pub verdict: Option<String>,
    pub notes: Option<String>,
    pub merged_commit: Option<String>,
}

impl Op for GetProposal {
    const NAME: &'static str = "get_proposal";
    const DESCRIPTION: &'static str = "A proposal's status, deployment summary, gate report and review notes.";
    const WHO: Who = Who::UserAndAgents;
    type Output = ProposalDetail;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<ProposalDetail, OpError> {
        let p = hub.store.proposal(&self.id).await?;
        if let Some(me) = caller.instance()
            && p.instance != me
            && !caller.may(Capability::Review)
        {
            return Err(OpError::Forbidden("that proposal is not yours".into()));
        }
        let tz = hub.applied().await.defs.timezone;
        Ok(ProposalDetail {
            summary: p.summary.as_ref().map(|s| s.to_markdown(tz)),
            id: p.id,
            title: p.title,
            status: p.status,
            branch: p.branch,
            gate_report: p.gate_report,
            verdict: p.verdict,
            notes: p.review_notes,
            merged_commit: p.merged_commit,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct SubmitReview {
    pub proposal_id: String,
    pub verdict: Verdict,
    pub notes: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ReviewRecorded {
    pub recorded: Verdict,
    pub note: String,
}

impl Op for SubmitReview {
    const NAME: &'static str = "submit_review";
    const DESCRIPTION: &'static str = "Your verdict on the proposal you were asked to review: approve, escalate (fine, but the user should look) or reject, with notes for a busy reader.";
    const WHO: Who = Who::AgentsWith(Capability::Review);
    const ENDS_TURN: bool = true;
    type Output = ReviewRecorded;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<ReviewRecorded, OpError> {
        let instance = caller.agent("only reviewers review")?;
        transactions::submit_review(hub, instance, &self.proposal_id, self.verdict, &self.notes).await?;
        crate::agents::stop_after_reply(hub, instance).await;
        Ok(ReviewRecorded { recorded: self.verdict, note: "Thank you. Your work is done; stop now.".into() })
    }
}
