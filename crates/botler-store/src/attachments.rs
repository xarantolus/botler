//! Files a chat attached to its reply, by when: the session view shows them with the reply.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Result, Store, parse_ts, ts};

impl Store {
    pub async fn add_attachments(&self, instance: &str, files: &[String]) -> Result<()> {
        let at = ts(Utc::now());
        for file in files {
            sqlx::query("INSERT INTO attachments (instance, at, file) VALUES (?, ?, ?)")
                .bind(instance)
                .bind(&at)
                .bind(file)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    /// `instance`'s attachments, oldest first.
    pub async fn attachments(&self, instance: &str) -> Result<Vec<(DateTime<Utc>, String)>> {
        let rows = sqlx::query("SELECT at, file FROM attachments WHERE instance = ? ORDER BY at, rowid")
            .bind(instance)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| Ok((parse_ts("attachments", r.get("at"))?, r.get("file")))).collect()
    }
}
