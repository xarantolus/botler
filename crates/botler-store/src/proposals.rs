//! Proposals: monorepo changes moving through gate → review → decision, and approval-gated action
//! invocations bound to the hash of their arguments.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_opt_ts, parse_ts, text_enum, ts};

text_enum!(ProposalKind {
    Change = "change",
    Action = "action",
});

text_enum!(ProposalStatus {
    Gating = "gating",
    Reviewing = "reviewing",
    AwaitingUser = "awaiting_user",
    Merged = "merged",
    Approved = "approved",
    Rejected = "rejected",
    Failed = "failed",
});

impl ProposalStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Approved | Self::Rejected | Self::Failed)
    }
}

text_enum!(ProposalStage {
    Rebase = "rebase",
    /// Building the image the gate runs in.
    Image = "image",
    /// Running `gate/run`.
    Gate = "gate",
    Review = "review",
    Approval = "approval",
    Merge = "merge",
});

/// When the pipeline entered a stage.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct StageMark {
    pub stage: ProposalStage,
    pub at: DateTime<Utc>,
}

/// Who decided a proposal and why.
#[derive(Clone, Debug)]
pub struct ProposalDecision<'a> {
    /// `user`, or the reviewer's instance name.
    pub by: &'a str,
    /// Why it went this way (the decision table's outcome), for the user.
    pub why: &'a str,
    /// The user's reason (rejections) or comment (approvals).
    pub reason: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Proposal {
    pub id: String,
    pub kind: ProposalKind,
    pub instance: String,
    pub title: String,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub head_commit: Option<String>,
    pub action: Option<String>,
    pub args: Option<serde_json::Value>,
    pub args_hash: Option<String>,
    /// Actions: the action's code version (image tag) when it was invoked; only that code runs.
    pub code_version: Option<String>,
    /// The deployment summary, once the change validated.
    pub summary: Option<botler_core::change::ChangeSummary>,
    pub gate_report: Option<String>,
    pub verdict: Option<String>,
    pub review_notes: Option<String>,
    pub status: ProposalStatus,
    pub merged_commit: Option<String>,
    pub reviewer: Option<String>,
    pub attempts: i64,
    /// Where the pipeline is, or stopped.
    pub stage: Option<ProposalStage>,
    pub stage_at: Option<DateTime<Utc>>,
    /// Every stage entered, in order, across re-runs.
    pub stages: Vec<StageMark>,
    /// What the current stage is doing right now, e.g. `building image dev (STEP 7/14: RUN …)`.
    pub stage_detail: Option<String>,
    /// The last lines of the current stage's output (image build, gate).
    pub stage_log: Option<String>,
    /// The head the gate last passed on; a merge of any other head re-checks first.
    pub gated_commit: Option<String>,
    /// `git patch-id` of the approved change: an identical change rebased onto a newer main
    /// keeps its approval.
    pub approved_patch: Option<String>,
    /// The escalations the approval covered.
    pub approved_escalations: Option<Vec<botler_core::change::Escalation>>,
    /// How often the approval carried over a clean rebase.
    pub carried_over: i64,
    /// Why it went the way it did, e.g. `merged without asking you: the reviewer approved and nothing needs your approval`.
    pub decision: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    /// The user's reason for rejecting, or comment on approving.
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Default)]
pub struct NewProposal {
    pub instance: String,
    pub title: String,
    pub branch: Option<String>,
    pub base_commit: Option<String>,
    pub head_commit: Option<String>,
    pub action: Option<String>,
    pub args: Option<serde_json::Value>,
    pub args_hash: Option<String>,
    pub code_version: Option<String>,
}

/// Enter a stage (binds: stage, now, now, stage, now): its history grows, the last stage's live
/// detail and output go.
macro_rules! enter_stage {
    () => {
        "stage = ?, stage_at = ?, updated_at = ?, stage_log = NULL, \
         stages = json_insert(COALESCE(stages, '[]'), '$[#]', json_object('stage', ?, 'at', ?))"
    };
}

fn row(r: &SqliteRow) -> Result<Proposal> {
    const T: &str = "proposals";
    fn json<V: serde::de::DeserializeOwned>(r: &SqliteRow, col: &str) -> Result<Option<V>> {
        r.get::<Option<String>, _>(col)
            .map(|s| serde_json::from_str(&s).map_err(|e| Error::Corrupt { table: T, detail: format!("{col}: {e}") }))
            .transpose()
    }
    Ok(Proposal {
        id: r.get("id"),
        kind: ProposalKind::parse(T, r.get("kind"))?,
        instance: r.get("instance"),
        title: r.get("title"),
        branch: r.get("branch"),
        base_commit: r.get("base_commit"),
        head_commit: r.get("head_commit"),
        action: r.get("action"),
        args: json(r, "args")?,
        args_hash: r.get("args_hash"),
        code_version: r.get("code_version"),
        summary: json(r, "summary")?,
        gate_report: r.get("gate_report"),
        verdict: r.get("verdict"),
        review_notes: r.get("review_notes"),
        status: ProposalStatus::parse(T, r.get("status"))?,
        merged_commit: r.get("merged_commit"),
        reviewer: r.get("reviewer"),
        attempts: r.get("attempts"),
        stage: r.get::<Option<String>, _>("stage").map(|s| ProposalStage::parse(T, &s)).transpose()?,
        stage_at: parse_opt_ts(T, r.get("stage_at"))?,
        stages: json(r, "stages")?.unwrap_or_default(),
        stage_detail: r.get("stage_detail"),
        stage_log: r.get("stage_log"),
        gated_commit: r.get("gated_commit"),
        approved_patch: r.get("approved_patch"),
        approved_escalations: json(r, "approved_escalations")?,
        carried_over: r.get("carried_over"),
        decision: r.get("decision"),
        decided_by: r.get("decided_by"),
        decided_at: parse_opt_ts(T, r.get("decided_at"))?,
        decision_reason: r.get("decision_reason"),
        created_at: parse_ts(T, r.get("created_at"))?,
        updated_at: parse_ts(T, r.get("updated_at"))?,
    })
}

impl Store {
    // The pipeline's writes below only touch proposals still in flight, so a decision (the
    // user's rejection) is never overwritten by a pipeline step that was already running.
    pub async fn create_proposal(
        &self,
        kind: ProposalKind,
        status: ProposalStatus,
        p: &NewProposal,
    ) -> Result<Proposal> {
        let id = new_id();
        let now = ts(Utc::now());
        sqlx::query(
            "INSERT INTO proposals (id, kind, instance, title, branch, base_commit, head_commit, action, args, args_hash, code_version, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(kind.as_str())
        .bind(&p.instance)
        .bind(&p.title)
        .bind(&p.branch)
        .bind(&p.base_commit)
        .bind(&p.head_commit)
        .bind(&p.action)
        .bind(p.args.as_ref().map(ToString::to_string))
        .bind(&p.args_hash)
        .bind(&p.code_version)
        .bind(status.as_str())
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        self.proposal(&id).await
    }

    pub async fn proposal(&self, id: &str) -> Result<Proposal> {
        let r = sqlx::query("SELECT * FROM proposals WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("proposal", id))?)
    }

    pub async fn recent_proposals(&self, limit: u32) -> Result<Vec<Proposal>> {
        let rows = sqlx::query("SELECT * FROM proposals ORDER BY created_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// Proposals that were mid-flight when the hub stopped.
    pub async fn unfinished_proposals(&self) -> Result<Vec<Proposal>> {
        let rows = sqlx::query("SELECT * FROM proposals WHERE status IN ('gating', 'reviewing') ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// Proposals not finished yet, including those waiting for the user.
    pub async fn in_flight_proposals(&self) -> Result<Vec<Proposal>> {
        let rows = sqlx::query("SELECT * FROM proposals WHERE kind = 'change' AND status IN ('gating', 'reviewing', 'awaiting_user') ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn set_proposal_status(&self, id: &str, status: ProposalStatus) -> Result<()> {
        sqlx::query("UPDATE proposals SET status = ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')")
            .bind(status.as_str())
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_proposal_gate(
        &self,
        id: &str,
        head_commit: &str,
        summary: &botler_core::change::ChangeSummary,
        report: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE proposals SET head_commit = ?, summary = ?, gate_report = ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')")
            .bind(head_commit)
            .bind(crate::encode("proposal summary", summary)?)
            .bind(report)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_proposal_review(&self, id: &str, verdict: &str, notes: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET verdict = ?, review_notes = ?, updated_at = ? WHERE id = ?")
            .bind(verdict)
            .bind(notes)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_proposal_base(&self, id: &str, base: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET base_commit = ?, updated_at = ? WHERE id = ?")
            .bind(base)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Hand the proposal to `reviewer`: status and reviewer change together, so nobody sees a
    /// proposal under review without its reviewer.
    /// False if it finished or is merging meanwhile.
    pub async fn set_proposal_reviewing(&self, id: &str, reviewer: &str) -> Result<bool> {
        let n = sqlx::query("UPDATE proposals SET status = ?, reviewer = ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user') AND COALESCE(stage, '') != 'merge'")
            .bind(ProposalStatus::Reviewing.as_str())
            .bind(reviewer)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n == 1)
    }

    /// Count a pipeline (re-)run; returns the new count.
    pub async fn bump_proposal_attempts(&self, id: &str) -> Result<i64> {
        sqlx::query("UPDATE proposals SET attempts = attempts + 1, updated_at = ? WHERE id = ?")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(self.proposal(id).await?.attempts)
    }

    /// Record why a proposal failed or was rejected. False if it had already finished.
    pub async fn set_proposal_outcome(&self, id: &str, status: ProposalStatus, detail: &str) -> Result<bool> {
        let n = sqlx::query("UPDATE proposals SET status = ?, review_notes = COALESCE(review_notes || char(10), '') || ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')")
            .bind(status.as_str())
            .bind(detail)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n == 1)
    }

    pub async fn set_proposal_merged(&self, id: &str, commit: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET status = 'merged', merged_commit = ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')")
            .bind(commit)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Move an in-flight proposal to `stage`, doing `detail`. False if it finished, or is merging
    /// (which a rejection can no longer stop).
    pub async fn set_proposal_stage(&self, id: &str, stage: ProposalStage, detail: &str) -> Result<bool> {
        let now = ts(Utc::now());
        let n = sqlx::query(concat!(
            "UPDATE proposals SET ",
            enter_stage!(),
            ", stage_detail = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user') AND COALESCE(stage, '') != 'merge'",
        ))
        .bind(stage.as_str())
        .bind(&now)
        .bind(&now)
        .bind(stage.as_str())
        .bind(&now)
        .bind(detail)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Start an in-flight proposal's pipeline over: back to `gating` at the rebase stage, from any
    /// stage, merge included (main moved under the merge, or the hub restarted mid-merge). False
    /// if it had already finished.
    pub async fn restart_proposal(&self, id: &str, detail: &str) -> Result<bool> {
        let now = ts(Utc::now());
        let stage = ProposalStage::Rebase.as_str();
        let n = sqlx::query(concat!(
            "UPDATE proposals SET status = 'gating', ",
            enter_stage!(),
            ", stage_detail = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')",
        ))
        .bind(stage)
        .bind(&now)
        .bind(&now)
        .bind(stage)
        .bind(&now)
        .bind(detail)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Record who decided an in-flight proposal and why; with `status`, also finish it there.
    /// False if it had already finished or is merging.
    pub async fn decide_proposal(
        &self,
        id: &str,
        status: Option<ProposalStatus>,
        d: &ProposalDecision<'_>,
    ) -> Result<bool> {
        let now = ts(Utc::now());
        let n = sqlx::query(
            "UPDATE proposals SET status = COALESCE(?, status), decision = ?, decided_by = ?, decided_at = ?, decision_reason = ?, updated_at = ?
             WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user') AND COALESCE(stage, '') != 'merge'",
        )
        .bind(status.map(ProposalStatus::as_str))
        .bind(d.why)
        .bind(d.by)
        .bind(&now)
        .bind(d.reason)
        .bind(&now)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// What the current stage is doing now, and its output tail.
    pub async fn set_proposal_progress(&self, id: &str, detail: &str, log: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE proposals SET stage_detail = ?, stage_log = COALESCE(?, stage_log) WHERE id = ?")
            .bind(detail)
            .bind(log)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The gate passed on `commit`.
    pub async fn set_proposal_gated(&self, id: &str, commit: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET gated_commit = ?, updated_at = ? WHERE id = ?")
            .bind(commit)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Main moved and the change rebased onto it unchanged: the new base and head.
    pub async fn set_proposal_rebased(&self, id: &str, base: &str, head: &str) -> Result<bool> {
        let n = sqlx::query("UPDATE proposals SET base_commit = ?, head_commit = ?, updated_at = ? WHERE id = ? AND status IN ('gating', 'reviewing', 'awaiting_user')")
            .bind(base)
            .bind(head)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        Ok(n == 1)
    }

    /// Record what was approved (the change's patch id and the escalations it covered), or with
    /// `None` drop the approval.
    pub async fn set_proposal_approval(
        &self,
        id: &str,
        approval: Option<(&str, &[botler_core::change::Escalation])>,
    ) -> Result<()> {
        let escalations = approval.map(|(_, e)| crate::encode("approved escalations", e)).transpose()?;
        sqlx::query("UPDATE proposals SET approved_patch = ?, approved_escalations = ?, updated_at = ? WHERE id = ?")
            .bind(approval.map(|(p, _)| p))
            .bind(escalations)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The approval carried over a clean rebase.
    pub async fn carry_proposal_approval(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET carried_over = carried_over + 1, updated_at = ? WHERE id = ?")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
