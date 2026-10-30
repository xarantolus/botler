//! The append-only event journal.

use botler_core::Event;
use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Error, Result, Store, ts};

fn decode(body: &str) -> Result<Event> {
    serde_json::from_str(body).map_err(|e| Error::Corrupt { table: "events", detail: e.to_string() })
}

/// Filter for reading the journal back.
#[derive(Clone, Debug, Default)]
pub struct EventQuery {
    /// Exact type or `prefix.*`.
    pub kind: Option<String>,
    pub correlation: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: u32,
}

impl Store {
    /// Journal an event. Idempotent on `id`.
    pub async fn append_event(&self, e: &Event) -> Result<()> {
        let body =
            serde_json::to_string(e).map_err(|err| Error::Corrupt { table: "events", detail: err.to_string() })?;
        sqlx::query(
            "INSERT INTO events (id, kind, source, body, to_instance, correlation, at) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(e.id.to_string())
        .bind(&e.kind)
        .bind(e.source.key())
        .bind(body)
        .bind(&e.to)
        .bind(e.correlation.map(|c| c.to_string()))
        .bind(ts(e.at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn event(&self, id: &str) -> Result<Event> {
        let row = sqlx::query("SELECT body FROM events WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        let row = row.ok_or_else(|| crate::not_found("event", id))?;
        decode(row.get::<&str, _>("body"))
    }

    /// Events journaled but not yet routed, oldest first (recovery after a crash).
    pub async fn unrouted_events(&self, limit: u32) -> Result<Vec<Event>> {
        let rows = sqlx::query("SELECT body FROM events WHERE routed = 0 ORDER BY id LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| decode(r.get::<&str, _>("body"))).collect()
    }

    pub async fn mark_routed(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE events SET routed = 1 WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// Newest-first query over the journal.
    pub async fn events(&self, q: &EventQuery) -> Result<Vec<Event>> {
        let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new("SELECT body FROM events WHERE 1 = 1");
        if let Some(k) = &q.kind {
            if let Some(prefix) = k.strip_suffix('*') {
                let escaped = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
                qb.push(" AND kind LIKE ").push_bind(format!("{escaped}%")).push(" ESCAPE '\\'");
            } else {
                qb.push(" AND kind = ").push_bind(k.clone());
            }
        }
        if let Some(c) = &q.correlation {
            qb.push(" AND (correlation = ").push_bind(c.clone()).push(" OR id = ").push_bind(c.clone()).push(")");
        }
        if let Some(s) = q.since {
            qb.push(" AND at >= ").push_bind(ts(s));
        }
        qb.push(" ORDER BY id DESC LIMIT ").push_bind(q.limit.clamp(1, 1000));
        let rows = qb.build().fetch_all(&self.pool).await?;
        rows.iter().map(|r| decode(r.get::<&str, _>("body"))).collect()
    }
}
