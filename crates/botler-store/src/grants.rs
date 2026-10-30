//! Grants (resources a chat holds beyond its definition) and runtime subscriptions: the edges of
//! the per-chat graph the hub stores. Both are suspended while their chat is archived.

use std::collections::BTreeMap;

use botler_core::defs::{MountMode, Resource, ResourceKind};
use botler_core::routing::Subscription;
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, new_id, parse_opt_ts, parse_ts, ts};

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
pub struct Grant {
    pub id: String,
    /// The chat (instance) holding it.
    pub holder: String,
    pub kind: ResourceKind,
    /// The resource's name within its kind (a secret group, a mount, a service, an event type).
    pub resource: String,
    pub mode: Option<MountMode>,
    /// `user`, `policy` (an `auto` rule) or `spawn:<parent>`.
    pub granted_by: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    /// The holder is archived: the grant applies again when it is restored.
    pub suspended: bool,
}

impl Grant {
    pub fn resource(&self) -> Resource {
        Resource::new(self.kind, self.resource.clone())
    }
}

#[derive(Clone, Debug)]
pub struct NewSubscription {
    pub instance: String,
    pub pattern: String,
    pub filter: BTreeMap<String, serde_json::Value>,
    pub wake: bool,
    pub note: Option<String>,
}

/// A grants query with `suspended` derived from the holder's state.
macro_rules! grants {
    ($tail:literal) => {
        concat!(
            "SELECT g.*, COALESCE(i.state = 'archived', 0) AS suspended FROM grants g ",
            "LEFT JOIN instances i ON i.name = g.holder ",
            $tail
        )
    };
}

fn grant_row(r: &SqliteRow) -> Result<Grant> {
    const T: &str = "grants";
    let kind: String = r.get("kind");
    let kind = ResourceKind::ALL
        .into_iter()
        .find(|k| k.as_str() == kind)
        .ok_or_else(|| Error::Corrupt { table: T, detail: format!("unknown resource kind {kind:?}") })?;
    Ok(Grant {
        id: r.get("id"),
        holder: r.get("holder"),
        kind,
        resource: r.get("resource"),
        mode: match r.get::<Option<&str>, _>("mode") {
            None => None,
            Some("ro") => Some(MountMode::ReadOnly),
            Some("rw") => Some(MountMode::ReadWrite),
            Some(m) => return Err(Error::Corrupt { table: T, detail: format!("unknown mount mode {m:?}") }),
        },
        granted_by: r.get("granted_by"),
        created_at: parse_ts(T, r.get("created_at"))?,
        revoked_at: parse_opt_ts(T, r.get("revoked_at"))?,
        suspended: r.get("suspended"),
    })
}

fn sub_row(r: &SqliteRow) -> Result<Subscription> {
    const T: &str = "subscriptions";
    Ok(Subscription {
        id: r.get("id"),
        instance: r.get("instance"),
        pattern: r.get("pattern"),
        filter: serde_json::from_str(r.get("filter"))
            .map_err(|e| Error::Corrupt { table: T, detail: e.to_string() })?,
        wake: r.get("wake"),
        note: r.get("note"),
        created_at: parse_ts(T, r.get("created_at"))?,
    })
}

impl Store {
    /// Grant `resource` to `holder`. Holding it already (unrevoked) returns that grant.
    pub async fn add_grant(
        &self,
        holder: &str,
        resource: &Resource,
        mode: Option<MountMode>,
        granted_by: &str,
    ) -> Result<Grant> {
        if let Some(g) = self.live_grants(holder).await?.into_iter().find(|g| g.resource() == *resource) {
            return Ok(g);
        }
        let id = new_id();
        sqlx::query(
            "INSERT INTO grants (id, holder, kind, resource, mode, granted_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(holder)
        .bind(resource.kind.as_str())
        .bind(&resource.name)
        .bind(mode.map(MountMode::as_str))
        .bind(granted_by)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.grant(&id).await
    }

    pub async fn grant(&self, id: &str) -> Result<Grant> {
        let r = sqlx::query(grants!("WHERE g.id = ?")).bind(id).fetch_optional(&self.pool).await?;
        grant_row(&r.ok_or_else(|| crate::not_found("grant", id))?)
    }

    /// `holder`'s unrevoked grants, oldest first (also while it is archived).
    pub async fn live_grants(&self, holder: &str) -> Result<Vec<Grant>> {
        let rows = sqlx::query(grants!("WHERE g.holder = ? AND g.revoked_at IS NULL ORDER BY g.id"))
            .bind(holder)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(grant_row).collect()
    }

    /// Every unrevoked grant, by holder.
    pub async fn all_live_grants(&self) -> Result<Vec<Grant>> {
        let rows =
            sqlx::query(grants!("WHERE g.revoked_at IS NULL ORDER BY g.holder, g.id")).fetch_all(&self.pool).await?;
        rows.iter().map(grant_row).collect()
    }

    /// Revoke a grant. Returns false if it was revoked already.
    pub async fn revoke_grant(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("UPDATE grants SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
            .bind(ts(Utc::now()))
            .bind(id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            self.grant(id).await?; // an unknown id is an error; revoked already is not
        }
        Ok(r.rows_affected() == 1)
    }

    pub async fn add_subscription(&self, s: &NewSubscription) -> Result<Subscription> {
        let id = new_id();
        sqlx::query(
            "INSERT INTO subscriptions (id, instance, pattern, filter, wake, note, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&s.instance)
        .bind(&s.pattern)
        .bind(crate::encode("subscription filter", &s.filter)?)
        .bind(s.wake)
        .bind(&s.note)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        self.subscription(&id).await
    }

    pub async fn subscription(&self, id: &str) -> Result<Subscription> {
        let r = sqlx::query("SELECT * FROM subscriptions WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        sub_row(&r.ok_or_else(|| crate::not_found("subscription", id))?)
    }

    /// A chat's subscriptions, oldest first.
    pub async fn subscriptions_of(&self, instance: &str) -> Result<Vec<Subscription>> {
        let rows = sqlx::query("SELECT * FROM subscriptions WHERE instance = ? ORDER BY id")
            .bind(instance)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(sub_row).collect()
    }

    /// Subscriptions the router follows: those of chats that are not archived.
    pub async fn active_subscriptions(&self) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(
            "SELECT s.* FROM subscriptions s JOIN instances i ON i.name = s.instance
             WHERE i.state != 'archived' ORDER BY s.id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(sub_row).collect()
    }

    pub async fn delete_subscription(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM subscriptions WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(r.rows_affected() == 1)
    }

    /// The event types `source` (a [`botler_core::Source::key`]) has emitted.
    pub async fn emitted_kinds(&self, source: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT DISTINCT kind FROM events WHERE source = ?")
            .bind(source)
            .fetch_all(&self.pool)
            .await?)
    }

    /// The chats `name` spawned, oldest first.
    pub async fn children(&self, name: &str) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT name FROM instances WHERE parent = ? ORDER BY created_at")
            .bind(name)
            .fetch_all(&self.pool)
            .await?)
    }
}
