//! Tripped circuit breakers. A tripped subject stays paused until the user clears it.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Result, Store, parse_ts, ts};

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Breaker {
    pub subject: String,
    pub kind: String,
    pub reason: String,
    pub tripped_at: DateTime<Utc>,
}

impl Store {
    /// Trip a breaker. Returns true if it was not already tripped.
    pub async fn trip_breaker(&self, subject: &str, kind: &str, reason: &str) -> Result<bool> {
        let r = sqlx::query("INSERT INTO breakers (subject, kind, reason, tripped_at) VALUES (?, ?, ?, ?) ON CONFLICT (subject) DO NOTHING")
            .bind(subject)
            .bind(kind)
            .bind(reason)
            .bind(ts(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    pub async fn breaker(&self, subject: &str) -> Result<Option<Breaker>> {
        let r =
            sqlx::query("SELECT * FROM breakers WHERE subject = ?").bind(subject).fetch_optional(&self.pool).await?;
        r.map(|r| {
            Ok(Breaker {
                subject: r.get("subject"),
                kind: r.get("kind"),
                reason: r.get("reason"),
                tripped_at: parse_ts("breakers", r.get("tripped_at"))?,
            })
        })
        .transpose()
    }

    pub async fn breakers(&self) -> Result<Vec<Breaker>> {
        let rows = sqlx::query("SELECT * FROM breakers ORDER BY tripped_at DESC").fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|r| {
                Ok(Breaker {
                    subject: r.get("subject"),
                    kind: r.get("kind"),
                    reason: r.get("reason"),
                    tripped_at: parse_ts("breakers", r.get("tripped_at"))?,
                })
            })
            .collect()
    }

    pub async fn clear_breaker(&self, subject: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM breakers WHERE subject = ?").bind(subject).execute(&self.pool).await?;
        Ok(r.rows_affected() == 1)
    }
}
