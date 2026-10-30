//! One-off timers set at runtime. Cron/interval schedules are definitions and live in the repo.

use botler_core::defs::Target;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_ts, text_enum, ts};

text_enum!(TimerStatus {
    Pending = "pending",
    Fired = "fired",
    Cancelled = "cancelled",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Timer {
    pub id: String,
    pub fire_at: DateTime<Utc>,
    pub target: Target,
    pub prompt: Option<String>,
    pub payload: serde_json::Value,
    pub created_by: String,
    pub status: TimerStatus,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct NewTimer {
    pub fire_at: DateTime<Utc>,
    pub target: Target,
    pub prompt: Option<String>,
    pub payload: serde_json::Value,
    pub created_by: String,
}

fn row(r: &SqliteRow) -> Result<Timer> {
    const T: &str = "timers";
    let corrupt = |e: serde_json::Error| Error::Corrupt { table: T, detail: e.to_string() };
    Ok(Timer {
        id: r.get("id"),
        fire_at: parse_ts(T, r.get("fire_at"))?,
        target: serde_json::from_str(r.get("target")).map_err(corrupt)?,
        prompt: r.get("prompt"),
        payload: serde_json::from_str(r.get("payload")).map_err(corrupt)?,
        created_by: r.get("created_by"),
        status: TimerStatus::parse(T, r.get("status"))?,
        created_at: parse_ts(T, r.get("created_at"))?,
    })
}

impl Store {
    pub async fn create_timer(&self, t: &NewTimer) -> Result<Timer> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO timers (id, fire_at, target, prompt, payload, created_by, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(&id)
        .bind(ts(t.fire_at))
        .bind(crate::encode("timer target", &t.target)?)
        .bind(&t.prompt)
        .bind(t.payload.to_string())
        .bind(&t.created_by)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.timer(&id).await
    }

    pub async fn timer(&self, id: &str) -> Result<Timer> {
        let r = sqlx::query("SELECT * FROM timers WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("timer", id))?)
    }

    /// Pending timers due at or before `now`.
    pub async fn due_timers(&self, now: DateTime<Utc>) -> Result<Vec<Timer>> {
        let rows = sqlx::query("SELECT * FROM timers WHERE status = 'pending' AND fire_at <= ? ORDER BY fire_at")
            .bind(ts(now))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// The earliest pending timer, for sleeping until it.
    pub async fn next_timer_at(&self) -> Result<Option<DateTime<Utc>>> {
        let r =
            sqlx::query("SELECT MIN(fire_at) AS t FROM timers WHERE status = 'pending'").fetch_one(&self.pool).await?;
        crate::parse_opt_ts("timers", r.get("t"))
    }

    pub async fn pending_timers(&self, created_by: Option<&str>) -> Result<Vec<Timer>> {
        let rows = match created_by {
            Some(c) => {
                sqlx::query("SELECT * FROM timers WHERE status = 'pending' AND created_by = ? ORDER BY fire_at").bind(c)
            }
            None => sqlx::query("SELECT * FROM timers WHERE status = 'pending' ORDER BY fire_at"),
        }
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    pub async fn set_timer_status(&self, id: &str, status: TimerStatus) -> Result<bool> {
        let r = sqlx::query("UPDATE timers SET status = ? WHERE id = ? AND status = 'pending'")
            .bind(status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }
}
