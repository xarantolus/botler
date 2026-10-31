//! Signed-in devices and one-time sign-in codes. Only hashes of their secrets are stored.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, new_id, parse_opt_ts, parse_ts, ts};

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Device {
    pub id: String,
    /// What the user called it (or the browser it signed in from).
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

fn row(r: &SqliteRow) -> Result<(Device, String)> {
    const T: &str = "devices";
    let d = Device {
        id: r.get("id"),
        name: r.get("name"),
        created_at: parse_ts(T, r.get("created_at"))?,
        last_seen_at: parse_ts(T, r.get("last_seen_at"))?,
        revoked_at: parse_opt_ts(T, r.get("revoked_at"))?,
    };
    Ok((d, r.get("secret_hash")))
}

impl Store {
    pub async fn create_device(&self, name: &str, secret_hash: &str) -> Result<Device> {
        let id = new_id();
        let now = ts(Utc::now());
        sqlx::query("INSERT INTO devices (id, name, secret_hash, created_at, last_seen_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(name)
            .bind(secret_hash)
            .bind(&now)
            .bind(&now)
            .execute(&self.pool)
            .await?;
        Ok(self.device(&id).await?.ok_or_else(|| crate::not_found("device", &id))?.0)
    }

    /// A device and its secret's hash.
    pub async fn device(&self, id: &str) -> Result<Option<(Device, String)>> {
        let r = sqlx::query("SELECT * FROM devices WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        r.as_ref().map(row).transpose()
    }

    /// Devices that are still signed in, most recently seen first.
    pub async fn devices(&self) -> Result<Vec<Device>> {
        let rows = sqlx::query("SELECT * FROM devices WHERE revoked_at IS NULL ORDER BY last_seen_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| row(r).map(|(d, _)| d)).collect()
    }

    pub async fn touch_device(&self, id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
            .bind(ts(at))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Sign a device out; false if it was not signed in.
    pub async fn revoke_device(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE devices SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn create_login_code(&self, code_hash: &str, expires_at: DateTime<Utc>) -> Result<()> {
        sqlx::query("INSERT INTO login_codes (code_hash, expires_at) VALUES (?, ?)")
            .bind(code_hash)
            .bind(ts(expires_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Use a sign-in code: true if it was unused and unexpired (it never works again).
    pub async fn use_login_code(&self, code_hash: &str, now: DateTime<Utc>) -> Result<bool> {
        let r = sqlx::query(
            "UPDATE login_codes SET used_at = ? WHERE code_hash = ? AND used_at IS NULL AND expires_at > ?",
        )
        .bind(ts(now))
        .bind(code_hash)
        .bind(ts(now))
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() > 0)
    }
}
