//! Proposals: monorepo changes moving through gate → review → decision, and approval-gated action
//! invocations bound to the hash of their arguments.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_ts, text_enum, ts};

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

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
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
    pub summary: Option<serde_json::Value>,
    pub gate_report: Option<String>,
    pub verdict: Option<String>,
    pub review_notes: Option<String>,
    pub status: ProposalStatus,
    pub merged_commit: Option<String>,
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
}

fn row(r: &SqliteRow) -> Result<Proposal> {
    const T: &str = "proposals";
    let json = |col: &str| -> Result<Option<serde_json::Value>> {
        r.get::<Option<String>, _>(col)
            .map(|s| serde_json::from_str(&s).map_err(|e| Error::Corrupt { table: T, detail: e.to_string() }))
            .transpose()
    };
    Ok(Proposal {
        id: r.get("id"),
        kind: ProposalKind::parse(T, r.get("kind"))?,
        instance: r.get("instance"),
        title: r.get("title"),
        branch: r.get("branch"),
        base_commit: r.get("base_commit"),
        head_commit: r.get("head_commit"),
        action: r.get("action"),
        args: json("args")?,
        args_hash: r.get("args_hash"),
        summary: json("summary")?,
        gate_report: r.get("gate_report"),
        verdict: r.get("verdict"),
        review_notes: r.get("review_notes"),
        status: ProposalStatus::parse(T, r.get("status"))?,
        merged_commit: r.get("merged_commit"),
        created_at: parse_ts(T, r.get("created_at"))?,
        updated_at: parse_ts(T, r.get("updated_at"))?,
    })
}

impl Store {
    pub async fn create_proposal(
        &self,
        kind: ProposalKind,
        status: ProposalStatus,
        p: &NewProposal,
    ) -> Result<Proposal> {
        let id = new_id();
        let now = ts(Utc::now());
        sqlx::query(
            "INSERT INTO proposals (id, kind, instance, title, branch, base_commit, head_commit, action, args, args_hash, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

    pub async fn set_proposal_status(&self, id: &str, status: ProposalStatus) -> Result<()> {
        sqlx::query("UPDATE proposals SET status = ?, updated_at = ? WHERE id = ?")
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
        summary: &serde_json::Value,
        report: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE proposals SET head_commit = ?, summary = ?, gate_report = ?, updated_at = ? WHERE id = ?")
            .bind(head_commit)
            .bind(summary.to_string())
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

    pub async fn set_proposal_merged(&self, id: &str, commit: &str) -> Result<()> {
        sqlx::query("UPDATE proposals SET status = 'merged', merged_commit = ?, updated_at = ? WHERE id = ?")
            .bind(commit)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
