//! Incidents: one open row per failing runnable, so a crash storm is one incident with a count.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, new_id, parse_ts, text_enum, ts};

text_enum!(IncidentStatus {
    Open = "open",
    Resolved = "resolved",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Incident {
    pub id: String,
    pub runnable: String,
    pub kind: String,
    pub status: IncidentStatus,
    pub count: i64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
    pub last_run: Option<String>,
    pub resolution: Option<String>,
}

fn row(r: &SqliteRow) -> Result<Incident> {
    const T: &str = "incidents";
    Ok(Incident {
        id: r.get("id"),
        runnable: r.get("runnable"),
        kind: r.get("kind"),
        status: IncidentStatus::parse(T, r.get("status"))?,
        count: r.get("count"),
        first_at: parse_ts(T, r.get("first_at"))?,
        last_at: parse_ts(T, r.get("last_at"))?,
        last_run: r.get("last_run"),
        resolution: r.get("resolution"),
    })
}

impl Store {
    /// Open a new incident or bump the open one. Returns the incident and whether it is new
    /// (only new incidents notify; repeats just count).
    pub async fn record_failure(&self, runnable: &str, kind: &str, run_id: Option<&str>) -> Result<(Incident, bool)> {
        let now = ts(Utc::now());
        let mut tx = self.pool.begin().await?;
        let existing = sqlx::query("SELECT id FROM incidents WHERE runnable = ? AND status = 'open'")
            .bind(runnable)
            .fetch_optional(&mut *tx)
            .await?;
        let (id, is_new) = match existing {
            Some(r) => {
                let id: String = r.get("id");
                sqlx::query("UPDATE incidents SET count = count + 1, last_at = ?, last_run = ?, kind = ? WHERE id = ?")
                    .bind(&now)
                    .bind(run_id)
                    .bind(kind)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                (id, false)
            }
            None => {
                let id = new_id();
                sqlx::query(
                    "INSERT INTO incidents (id, runnable, kind, status, count, first_at, last_at, last_run)
                     VALUES (?, ?, ?, 'open', 1, ?, ?, ?)",
                )
                .bind(&id)
                .bind(runnable)
                .bind(kind)
                .bind(&now)
                .bind(&now)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
                (id, true)
            }
        };
        tx.commit().await?;
        Ok((self.incident(&id).await?, is_new))
    }

    pub async fn incident(&self, id: &str) -> Result<Incident> {
        let r = sqlx::query("SELECT * FROM incidents WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("incident", id))?)
    }

    pub async fn open_incidents(&self) -> Result<Vec<Incident>> {
        let rows = sqlx::query("SELECT * FROM incidents WHERE status = 'open' ORDER BY last_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// Past incidents of a runnable, newest first (the triage agent's history).
    pub async fn incidents_for(&self, runnable: &str, limit: u32) -> Result<Vec<Incident>> {
        let rows = sqlx::query("SELECT * FROM incidents WHERE runnable = ? ORDER BY first_at DESC LIMIT ?")
            .bind(runnable)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn resolve_incident(&self, id: &str, resolution: &str) -> Result<()> {
        sqlx::query("UPDATE incidents SET status = 'resolved', resolution = ? WHERE id = ? AND status = 'open'")
            .bind(resolution)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
