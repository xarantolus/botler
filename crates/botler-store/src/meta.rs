//! Small key/value facts about the hub itself (last applied commit, pause switch).

use sqlx::Row;

use crate::{Result, Store};

pub const LAST_APPLIED_COMMIT: &str = "last_applied_commit";
pub const PAUSED: &str = "paused";

impl Store {
    pub async fn meta(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT value FROM meta WHERE key = ?").bind(key).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| r.get("value")))
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn last_applied_commit(&self) -> Result<Option<String>> {
        self.meta(LAST_APPLIED_COMMIT).await
    }

    pub async fn set_last_applied_commit(&self, commit: &str) -> Result<()> {
        self.set_meta(LAST_APPLIED_COMMIT, commit).await
    }

    /// The kill switch.
    pub async fn paused(&self) -> Result<bool> {
        Ok(self.meta(PAUSED).await?.as_deref() == Some("1"))
    }

    pub async fn set_paused(&self, paused: bool) -> Result<()> {
        self.set_meta(PAUSED, if paused { "1" } else { "0" }).await
    }
}
