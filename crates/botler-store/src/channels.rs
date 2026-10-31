//! Channels: who outside counts as the user (bindings, made with one-time codes), senders that
//! do not, and what channel services send out.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_opt_ts, parse_ts, ts};

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct ChannelBinding {
    pub id: String,
    /// The channel service.
    pub channel: String,
    /// The outside account, as the channel names it (a Telegram chat id, an email address, ...).
    pub sender: String,
    /// The chat their messages go to.
    pub target: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct ChannelStranger {
    pub channel: String,
    pub sender: String,
    /// Messages dropped from this sender.
    pub count: i64,
    pub last_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct OutboxItem {
    pub id: String,
    pub channel: String,
    pub sender: String,
    pub text: String,
    /// Uploads (`<id>/<name>`).
    pub files: Vec<String>,
    /// A question this item asks; a reply naming this item's id answers it.
    pub dialog_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub sent_at: Option<DateTime<Utc>>,
}

fn binding(r: &SqliteRow) -> Result<ChannelBinding> {
    const T: &str = "channel_bindings";
    Ok(ChannelBinding {
        id: r.get("id"),
        channel: r.get("channel"),
        sender: r.get("sender"),
        target: r.get("target"),
        created_at: parse_ts(T, r.get("created_at"))?,
        revoked_at: parse_opt_ts(T, r.get("revoked_at"))?,
    })
}

fn outbox(r: &SqliteRow) -> Result<OutboxItem> {
    const T: &str = "channel_outbox";
    Ok(OutboxItem {
        id: r.get("id"),
        channel: r.get("channel"),
        sender: r.get("sender"),
        text: r.get("text"),
        files: serde_json::from_str(r.get("files")).map_err(|e| Error::Corrupt { table: T, detail: e.to_string() })?,
        dialog_id: r.get("dialog_id"),
        created_at: parse_ts(T, r.get("created_at"))?,
        sent_at: parse_opt_ts(T, r.get("sent_at"))?,
    })
}

impl Store {
    /// The live binding of `sender` on `channel`.
    pub async fn channel_binding(&self, channel: &str, sender: &str) -> Result<Option<ChannelBinding>> {
        let r = sqlx::query("SELECT * FROM channel_bindings WHERE channel = ? AND sender = ? AND revoked_at IS NULL")
            .bind(channel)
            .bind(sender)
            .fetch_optional(&self.pool)
            .await?;
        r.as_ref().map(binding).transpose()
    }

    pub async fn channel_bindings(&self) -> Result<Vec<ChannelBinding>> {
        let rows = sqlx::query("SELECT * FROM channel_bindings WHERE revoked_at IS NULL ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(binding).collect()
    }

    /// Bind `sender` on `channel` to the user, sending to `target` (replacing a live binding).
    pub async fn bind_channel(&self, channel: &str, sender: &str, target: &str) -> Result<ChannelBinding> {
        let now = ts(Utc::now());
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE channel_bindings SET revoked_at = ? WHERE channel = ? AND sender = ? AND revoked_at IS NULL",
        )
        .bind(&now)
        .bind(channel)
        .bind(sender)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO channel_bindings (id, channel, sender, target, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(new_id())
            .bind(channel)
            .bind(sender)
            .bind(target)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM channel_strangers WHERE channel = ? AND sender = ?")
            .bind(channel)
            .bind(sender)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.channel_binding(channel, sender).await?.ok_or_else(|| crate::not_found("channel binding", sender))
    }

    /// Send a binding's messages to another chat.
    pub async fn retarget_channel(&self, id: &str, target: &str) -> Result<()> {
        sqlx::query("UPDATE channel_bindings SET target = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(target)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Revoke a binding; false if it was not live.
    pub async fn revoke_channel_binding(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE channel_bindings SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn create_channel_code(&self, code: &str, target: &str, expires_at: DateTime<Utc>) -> Result<()> {
        sqlx::query("INSERT INTO channel_codes (code, target, expires_at) VALUES (?, ?, ?)")
            .bind(code)
            .bind(target)
            .bind(ts(expires_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Use a pairing code: its target if it was unused and unexpired (never usable again).
    pub async fn use_channel_code(&self, code: &str, now: DateTime<Utc>) -> Result<Option<String>> {
        let r = sqlx::query(
            "UPDATE channel_codes SET used_at = ? WHERE code = ? AND used_at IS NULL AND expires_at > ? RETURNING target",
        )
        .bind(ts(now))
        .bind(code)
        .bind(ts(now))
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|r| r.get("target")))
    }

    /// Count a dropped message from a sender that is not bound.
    pub async fn record_channel_stranger(&self, channel: &str, sender: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO channel_strangers (channel, sender, count, last_at) VALUES (?, ?, 1, ?)
             ON CONFLICT (channel, sender) DO UPDATE SET count = count + 1, last_at = excluded.last_at",
        )
        .bind(channel)
        .bind(sender)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn channel_strangers(&self) -> Result<Vec<ChannelStranger>> {
        let rows =
            sqlx::query("SELECT * FROM channel_strangers ORDER BY last_at DESC LIMIT 50").fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| {
                Ok(ChannelStranger {
                    channel: r.get("channel"),
                    sender: r.get("sender"),
                    count: r.get("count"),
                    last_at: parse_ts("channel_strangers", r.get("last_at"))?,
                })
            })
            .collect()
    }

    pub async fn queue_outbox(
        &self,
        channel: &str,
        sender: &str,
        text: &str,
        files: &[String],
        dialog_id: Option<&str>,
    ) -> Result<OutboxItem> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO channel_outbox (id, channel, sender, text, files, dialog_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(channel)
        .bind(sender)
        .bind(text)
        .bind(crate::encode("outbox files", files)?)
        .bind(dialog_id)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.outbox_item(&id).await
    }

    pub async fn outbox_item(&self, id: &str) -> Result<OutboxItem> {
        let r = sqlx::query("SELECT * FROM channel_outbox WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        outbox(&r.ok_or_else(|| crate::not_found("outbox item", id))?)
    }

    /// What `channel` has yet to send, oldest first.
    pub async fn pending_outbox(&self, channel: &str, limit: i64) -> Result<Vec<OutboxItem>> {
        let rows = sqlx::query(
            "SELECT * FROM channel_outbox WHERE channel = ? AND sent_at IS NULL ORDER BY created_at, rowid LIMIT ?",
        )
        .bind(channel)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(outbox).collect()
    }

    /// Mark an item sent; false if it is not `channel`'s or was already sent.
    pub async fn ack_outbox(&self, channel: &str, id: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE channel_outbox SET sent_at = ? WHERE id = ? AND channel = ? AND sent_at IS NULL")
            .bind(ts(Utc::now()))
            .bind(id)
            .bind(channel)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }
}
