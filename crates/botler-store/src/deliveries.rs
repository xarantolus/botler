//! The delivery queue: events waiting to be put into an instance's session.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, new_id, parse_opt_ts, parse_ts, text_enum, ts};

text_enum!(DeliveryStatus {
    Pending = "pending",
    Delivered = "delivered",
    Dropped = "dropped",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
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
        let id = new_id();
        sqlx::query(
            "INSERT INTO deliveries (id, event_id, instance, trigger_id, prompt, status, created_at)
             VALUES (?, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(&id)
        .bind(event_id)
        .bind(instance)
        .bind(trigger_id)
        .bind(prompt)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.delivery(&id).await
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

    /// Instances that have at least one pending delivery.
    pub async fn instances_with_pending(&self) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT DISTINCT instance FROM deliveries WHERE status = 'pending'")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| r.get("instance")).collect())
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

    pub async fn drop_delivery(&self, id: &str, note: &str) -> Result<()> {
        sqlx::query("UPDATE deliveries SET status = 'dropped', note = ? WHERE id = ?")
            .bind(note)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
