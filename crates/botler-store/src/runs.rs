//! Container runs of jobs, services, actions, gates and replays.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, new_id, parse_opt_ts, parse_ts, text_enum, ts};

text_enum!(RunKind {
    Oneshot = "oneshot",
    Service = "service",
    Action = "action",
    Gate = "gate",
    Replay = "replay",
    /// `trial_run`: a job or service from an agent's branch, its effects recorded, not routed.
    Trial = "trial",
});

text_enum!(RunStatus {
    Queued = "queued",
    Running = "running",
    Succeeded = "succeeded",
    Failed = "failed",
    Timeout = "timeout",
    Cancelled = "cancelled",
});

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Run {
    pub id: String,
    pub runnable: String,
    pub kind: RunKind,
    pub trigger_id: Option<String>,
    pub event_id: Option<String>,
    pub commit_id: Option<String>,
    pub status: RunStatus,
    pub container_id: Option<String>,
    pub exit_code: Option<i64>,
    pub reason: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    /// Services: the last `health` heartbeat.
    pub heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
pub struct NewRun {
    pub runnable: String,
    pub kind: RunKind,
    pub trigger_id: Option<String>,
    pub event_id: Option<String>,
    pub commit_id: Option<String>,
}

fn row(r: &SqliteRow) -> Result<Run> {
    const T: &str = "runs";
    Ok(Run {
        id: r.get("id"),
        runnable: r.get("runnable"),
        kind: RunKind::parse(T, r.get("kind"))?,
        trigger_id: r.get("trigger_id"),
        event_id: r.get("event_id"),
        commit_id: r.get("commit_id"),
        status: RunStatus::parse(T, r.get("status"))?,
        container_id: r.get("container_id"),
        exit_code: r.get("exit_code"),
        reason: r.get("reason"),
        started_at: parse_opt_ts(T, r.get("started_at"))?,
        finished_at: parse_opt_ts(T, r.get("finished_at"))?,
        created_at: parse_ts(T, r.get("created_at"))?,
        heartbeat_at: parse_opt_ts(T, r.get("heartbeat_at"))?,
    })
}

impl Store {
    pub async fn create_run(&self, n: &NewRun) -> Result<Run> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO runs (id, runnable, kind, trigger_id, event_id, commit_id, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'queued', ?)",
        )
        .bind(&id)
        .bind(&n.runnable)
        .bind(n.kind.as_str())
        .bind(&n.trigger_id)
        .bind(&n.event_id)
        .bind(&n.commit_id)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.run(&id).await
    }

    pub async fn run(&self, id: &str) -> Result<Run> {
        let r = sqlx::query("SELECT * FROM runs WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("run", id))?)
    }

    pub async fn start_run(&self, id: &str, container_id: &str) -> Result<()> {
        sqlx::query("UPDATE runs SET status = 'running', container_id = ?, started_at = ? WHERE id = ?")
            .bind(container_id)
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record a service's health heartbeat. Returns false if the run is no longer running.
    pub async fn heartbeat(&self, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let r = sqlx::query("UPDATE runs SET heartbeat_at = ? WHERE id = ? AND status = 'running'")
            .bind(ts(at))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Move a queued/running run to a terminal status. Returns false if it was already terminal
    /// (another path finished it first), so exactly one caller reports the outcome.
    pub async fn finish_run(
        &self,
        id: &str,
        status: RunStatus,
        exit_code: Option<i64>,
        reason: Option<&str>,
    ) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE runs SET status = ?, exit_code = ?, reason = ?, finished_at = ? WHERE id = ? AND status IN ('queued', 'running')",
        )
        .bind(status.as_str())
        .bind(exit_code)
        .bind(reason)
        .bind(ts(Utc::now()))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Queued runs of one runnable, oldest first (overlap = queue). Trials never count as the
    /// runnable's own runs, here and in [`Self::runs_for`] and [`Self::failures_since`].
    pub async fn queued_runs(&self, runnable: &str) -> Result<Vec<Run>> {
        let rows = sqlx::query(
            "SELECT * FROM runs WHERE runnable = ? AND status = 'queued' AND kind != 'trial' ORDER BY created_at",
        )
        .bind(runnable)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Runs still queued or running, e.g. to reconcile after a hub restart.
    pub async fn active_runs(&self) -> Result<Vec<Run>> {
        let rows = sqlx::query("SELECT * FROM runs WHERE status IN ('queued', 'running') ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn runs_for(&self, runnable: &str, limit: u32) -> Result<Vec<Run>> {
        let rows =
            sqlx::query("SELECT * FROM runs WHERE runnable = ? AND kind != 'trial' ORDER BY created_at DESC LIMIT ?")
                .bind(runnable)
                .bind(limit)
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(row).collect()
    }

    pub async fn runs_for_event(&self, event_id: &str) -> Result<Vec<Run>> {
        let rows = sqlx::query("SELECT * FROM runs WHERE event_id = ? ORDER BY created_at")
            .bind(event_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn recent_runs(&self, limit: u32) -> Result<Vec<Run>> {
        let rows = sqlx::query("SELECT * FROM runs ORDER BY created_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// How many runs of `runnable` ended in failure since `since` (crash-loop detection).
    pub async fn failures_since(&self, runnable: &str, since: DateTime<Utc>) -> Result<i64> {
        let r = sqlx::query(
            "SELECT COUNT(*) AS n FROM runs WHERE runnable = ? AND kind != 'trial' AND status IN ('failed', 'timeout') AND finished_at >= ?",
        )
        .bind(runnable)
        .bind(ts(since))
        .fetch_one(&self.pool)
        .await?;
        Ok(r.get("n"))
    }
}
