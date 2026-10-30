//! What the hub is waiting for: retries of failed turns, and answers agents expect from others.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, parse_opt_ts, parse_ts, ts};

/// A turn that failed with a model error and is being retried.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRetry {
    pub instance: String,
    /// The event whose delivery started the failed turn.
    pub event_id: Option<String>,
    /// Failed attempts so far.
    pub attempts: u32,
    pub error: String,
    /// When the next attempt is sent; `None` while one is in flight.
    pub next_at: Option<DateTime<Utc>>,
}

/// `sender` waits for `recipient` to answer until `due_at`.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplyExpectation {
    pub sender: String,
    pub recipient: String,
    pub event_id: Option<String>,
    pub due_at: DateTime<Utc>,
}

fn retry_row(r: &SqliteRow) -> Result<TurnRetry> {
    const T: &str = "turn_retries";
    Ok(TurnRetry {
        instance: r.get("instance"),
        event_id: r.get("event_id"),
        attempts: u32::try_from(r.get::<i64, _>("attempts"))
            .map_err(|e| crate::Error::Corrupt { table: T, detail: e.to_string() })?,
        error: r.get("error"),
        next_at: parse_opt_ts(T, r.get("next_at"))?,
    })
}

fn expectation_row(r: &SqliteRow) -> Result<ReplyExpectation> {
    Ok(ReplyExpectation {
        sender: r.get("sender"),
        recipient: r.get("recipient"),
        event_id: r.get("event_id"),
        due_at: parse_ts("reply_expectations", r.get("due_at"))?,
    })
}

impl Store {
    /// Count a failed attempt at `instance`'s turn. The first failure records `event_id` (what
    /// started the turn); later ones keep it.
    pub async fn record_turn_failure(&self, instance: &str, event_id: Option<&str>, error: &str) -> Result<TurnRetry> {
        let r = sqlx::query(
            "INSERT INTO turn_retries (instance, event_id, attempts, error) VALUES (?, ?, 1, ?)
             ON CONFLICT (instance) DO UPDATE SET attempts = attempts + 1, error = excluded.error, next_at = NULL
             RETURNING *",
        )
        .bind(instance)
        .bind(event_id)
        .bind(error)
        .fetch_one(&self.pool)
        .await?;
        retry_row(&r)
    }

    pub async fn turn_retry(&self, instance: &str) -> Result<Option<TurnRetry>> {
        let r = sqlx::query("SELECT * FROM turn_retries WHERE instance = ?")
            .bind(instance)
            .fetch_optional(&self.pool)
            .await?;
        r.as_ref().map(retry_row).transpose()
    }

    pub async fn schedule_turn_retry(&self, instance: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE turn_retries SET next_at = ? WHERE instance = ?")
            .bind(ts(at))
            .bind(instance)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Retries due at `now`, marked in flight (each is returned once).
    pub async fn take_due_retries(&self, now: DateTime<Utc>) -> Result<Vec<TurnRetry>> {
        let rows = sqlx::query("UPDATE turn_retries SET next_at = NULL WHERE next_at <= ? RETURNING *")
            .bind(ts(now))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(retry_row).collect()
    }

    /// The turn succeeded or was given up: forget its retries. Returns whether there were any.
    pub async fn clear_turn_retry(&self, instance: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM turn_retries WHERE instance = ?").bind(instance).execute(&self.pool).await?;
        Ok(r.rows_affected() > 0)
    }

    /// `sender` expects an answer from `recipient` by `due_at` (replacing an earlier expectation
    /// between the two).
    pub async fn expect_reply(
        &self,
        sender: &str,
        recipient: &str,
        event_id: Option<&str>,
        due_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO reply_expectations (sender, recipient, event_id, due_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (sender, recipient) DO UPDATE SET event_id = excluded.event_id, due_at = excluded.due_at",
        )
        .bind(sender)
        .bind(recipient)
        .bind(event_id)
        .bind(ts(due_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `recipient` answered `sender`, or (`sender = None`) everyone waiting on it was told.
    pub async fn clear_expectations(&self, sender: Option<&str>, recipient: &str) -> Result<()> {
        sqlx::query("DELETE FROM reply_expectations WHERE recipient = ? AND (? IS NULL OR sender = ?)")
            .bind(recipient)
            .bind(sender)
            .bind(sender)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn expectations_on(&self, recipient: &str) -> Result<Vec<ReplyExpectation>> {
        let rows = sqlx::query("SELECT * FROM reply_expectations WHERE recipient = ? ORDER BY due_at")
            .bind(recipient)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(expectation_row).collect()
    }

    /// Expectations overdue at `now`, removed (each is returned once).
    pub async fn take_overdue(&self, now: DateTime<Utc>) -> Result<Vec<ReplyExpectation>> {
        let rows = sqlx::query("DELETE FROM reply_expectations WHERE due_at <= ? RETURNING *")
            .bind(ts(now))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(expectation_row).collect()
    }

    /// The earliest pending retry or answer deadline, for sleeping until it.
    pub async fn next_wait_at(&self) -> Result<Option<DateTime<Utc>>> {
        let r = sqlx::query(
            "SELECT MIN(t) AS t FROM (SELECT MIN(next_at) AS t FROM turn_retries
             UNION ALL SELECT MIN(due_at) FROM reply_expectations)",
        )
        .fetch_one(&self.pool)
        .await?;
        parse_opt_ts("reply_expectations", r.get("t"))
    }
}
