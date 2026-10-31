//! The delivery queue: events waiting to be put into an instance's session.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, new_id, parse_opt_ts, parse_ts, text_enum, ts};

text_enum!(DeliveryStatus {
    Pending = "pending",
    Delivered = "delivered",
    Dropped = "dropped",
    /// Waiting for the user to clear the breaker named in `note`.
    Held = "held",
    /// Not delivered because the user stopped the chat (`note` says why).
    Cancelled = "cancelled",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Delivery {
    pub id: String,
    pub event_id: String,
    pub instance: String,
    pub trigger_id: Option<String>,
    pub prompt: Option<String>,
    pub status: DeliveryStatus,
    pub attempts: i64,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub note: Option<String>,
    /// Added to the session without starting a turn; never wakes a sleeping agent.
    pub quiet: bool,
}

fn row(r: &SqliteRow) -> Result<Delivery> {
    const T: &str = "deliveries";
    Ok(Delivery {
        id: r.get("id"),
        event_id: r.get("event_id"),
        instance: r.get("instance"),
        trigger_id: r.get("trigger_id"),
        prompt: r.get("prompt"),
        status: DeliveryStatus::parse(T, r.get("status"))?,
        attempts: r.get("attempts"),
        created_at: parse_ts(T, r.get("created_at"))?,
        delivered_at: parse_opt_ts(T, r.get("delivered_at"))?,
        note: r.get("note"),
        quiet: r.get::<i64, _>("quiet") != 0,
    })
}

impl Store {
    pub async fn enqueue_delivery(
        &self,
        event_id: &str,
        instance: &str,
        trigger_id: Option<&str>,
        prompt: Option<&str>,
    ) -> Result<Delivery> {
        self.enqueue_delivery_with(event_id, instance, trigger_id, prompt, false, None).await
    }

    /// Queue a delivery that is `quiet` (a note that starts no turn) and/or held until the user
    /// clears the breaker `held_by`.
    pub async fn enqueue_delivery_with(
        &self,
        event_id: &str,
        instance: &str,
        trigger_id: Option<&str>,
        prompt: Option<&str>,
        quiet: bool,
        held_by: Option<&str>,
    ) -> Result<Delivery> {
        let id = new_id();
        let status = if held_by.is_some() { DeliveryStatus::Held } else { DeliveryStatus::Pending };
        sqlx::query(
            "INSERT INTO deliveries (id, event_id, instance, trigger_id, prompt, status, created_at, note, quiet)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(event_id)
        .bind(instance)
        .bind(trigger_id)
        .bind(prompt)
        .bind(status.as_str())
        .bind(ts(Utc::now()))
        .bind(held_by)
        .bind(i64::from(quiet))
        .execute(&self.pool)
        .await?;
        self.delivery(&id).await
    }

    /// Put a pending delivery in the queue as if it was queued at `at` (deliveries go out oldest first).
    pub async fn backdate_delivery(&self, id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE deliveries SET created_at = ? WHERE id = ?")
            .bind(ts(at))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delivery(&self, id: &str) -> Result<Delivery> {
        let r = sqlx::query("SELECT * FROM deliveries WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("delivery", id))?)
    }

    /// Pending deliveries for one instance, oldest first.
    pub async fn pending_deliveries(&self, instance: &str) -> Result<Vec<Delivery>> {
        let rows =
            sqlx::query("SELECT * FROM deliveries WHERE status = 'pending' AND instance = ? ORDER BY created_at, id")
                .bind(instance)
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(row).collect()
    }

    /// Instances that have at least one pending delivery, and whether any of those should wake
    /// the instance (false: only quiet notes, delivered when it is awake anyway).
    pub async fn instances_with_pending(&self) -> Result<Vec<(String, bool)>> {
        let rows = sqlx::query(
            "SELECT instance, MAX(quiet = 0) AS wakes FROM deliveries WHERE status = 'pending' GROUP BY instance",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| (r.get("instance"), r.get::<i64, _>("wakes") != 0)).collect())
    }

    /// Release the deliveries a breaker held back. Returns how many.
    pub async fn release_held(&self, subject: &str) -> Result<u64> {
        let r = sqlx::query("UPDATE deliveries SET status = 'pending', note = NULL WHERE status = 'held' AND note = ?")
            .bind(subject)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Every delivery an event caused (for tracing an event to the chats it reached).
    pub async fn deliveries_for_event(&self, event_id: &str) -> Result<Vec<Delivery>> {
        let rows = sqlx::query("SELECT * FROM deliveries WHERE event_id = ? ORDER BY created_at")
            .bind(event_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    /// The most recently delivered delivery of an instance (what started its current turn).
    pub async fn last_delivered(&self, instance: &str) -> Result<Option<Delivery>> {
        let r = sqlx::query(
            "SELECT * FROM deliveries WHERE instance = ? AND status = 'delivered' ORDER BY delivered_at DESC, id DESC LIMIT 1",
        )
        .bind(instance)
        .fetch_optional(&self.pool)
        .await?;
        r.as_ref().map(row).transpose()
    }

    pub async fn mark_delivery_attempt(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE deliveries SET attempts = attempts + 1 WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn mark_delivered(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE deliveries SET status = 'delivered', delivered_at = ? WHERE id = ?")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Drop a delivery that is still pending; false if it is not (delivered, or dropped already).
    pub async fn cancel_pending(&self, id: &str, note: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE deliveries SET status = 'dropped', note = ? WHERE id = ? AND status = 'pending'")
            .bind(note)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() == 1)
    }

    /// Cancel everything queued for `instance` (pending and held). Returns how many.
    pub async fn cancel_deliveries(&self, instance: &str, note: &str) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE deliveries SET status = 'cancelled', note = ? WHERE instance = ? AND status IN ('pending', 'held')",
        )
        .bind(note)
        .bind(instance)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    pub async fn drop_delivery(&self, id: &str, note: &str) -> Result<()> {
        sqlx::query("UPDATE deliveries SET status = 'dropped', note = ? WHERE id = ?")
            .bind(note)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
