//! Per-runnable key/value state (poller cursors, dedup keys) behind the SDK's `state` API.

use chrono::Utc;
use sqlx::Row;

use crate::{Result, Store, ts};

/// Values larger than this are rejected: kv is for cursors, not data.
pub const MAX_VALUE_BYTES: usize = 64 * 1024;

impl Store {
    pub async fn kv_get(&self, owner: &str, key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM kv WHERE owner = ? AND key = ?")
            .bind(owner)
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.get("value")))
    }

    pub async fn kv_set(&self, owner: &str, key: &str, value: &str) -> Result<()> {
        if value.len() > MAX_VALUE_BYTES {
            return Err(crate::Error::ValueTooLarge { key: key.into(), limit: MAX_VALUE_BYTES });
        }
        sqlx::query(
            "INSERT INTO kv (owner, key, value, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (owner, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        )
        .bind(owner)
        .bind(key)
        .bind(value)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn kv_delete(&self, owner: &str, key: &str) -> Result<()> {
        sqlx::query("DELETE FROM kv WHERE owner = ? AND key = ?").bind(owner).bind(key).execute(&self.pool).await?;
        Ok(())
    }
}
