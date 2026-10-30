//! Agent instances and their lifecycle state.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, parse_ts, text_enum, ts};

text_enum!(InstanceState {
    Dormant = "dormant",
    Starting = "starting",
    Running = "running",
    Ended = "ended",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Instance {
    pub name: String,
    pub definition: String,
    pub persistent: bool,
    pub state: InstanceState,
    /// Session file path relative to the data directory's `sessions/`.
    pub session_file: String,
    pub branch: Option<String>,
    pub container_id: Option<String>,
    pub parent: Option<String>,
    pub task: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct NewInstance {
    pub name: String,
    pub definition: String,
    pub persistent: bool,
    pub session_file: String,
    pub branch: Option<String>,
    pub parent: Option<String>,
    pub task: Option<String>,
}

fn row(r: &SqliteRow) -> Result<Instance> {
    const T: &str = "instances";
    Ok(Instance {
        name: r.get("name"),
        definition: r.get("definition"),
        persistent: r.get::<i64, _>("persistent") != 0,
        state: InstanceState::parse(T, r.get("state"))?,
        session_file: r.get("session_file"),
        branch: r.get("branch"),
        container_id: r.get("container_id"),
        parent: r.get("parent"),
        task: r.get("task"),
        created_at: parse_ts(T, r.get("created_at"))?,
        last_active: parse_ts(T, r.get("last_active"))?,
    })
}

impl Store {
    pub async fn create_instance(&self, n: &NewInstance) -> Result<Instance> {
        let now = ts(Utc::now());
        let res = sqlx::query(
            "INSERT INTO instances (name, definition, persistent, state, session_file, branch, parent, task, created_at, last_active)
             VALUES (?, ?, ?, 'dormant', ?, ?, ?, ?, ?, ?) ON CONFLICT (name) DO NOTHING",
        )
        .bind(&n.name)
        .bind(&n.definition)
        .bind(i64::from(n.persistent))
        .bind(&n.session_file)
        .bind(&n.branch)
        .bind(&n.parent)
        .bind(&n.task)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        if res.rows_affected() == 0 {
            return Err(Error::InstanceExists(n.name.clone()));
        }
        self.instance(&n.name).await
    }

    pub async fn instance(&self, name: &str) -> Result<Instance> {
        let r = sqlx::query("SELECT * FROM instances WHERE name = ?").bind(name).fetch_optional(&self.pool).await?;
        row(&r.ok_or_else(|| crate::not_found("instance", name))?)
    }

    pub async fn find_instance(&self, name: &str) -> Result<Option<Instance>> {
        let r = sqlx::query("SELECT * FROM instances WHERE name = ?").bind(name).fetch_optional(&self.pool).await?;
        r.as_ref().map(row).transpose()
    }

    /// All instances that have not ended, newest first.
    pub async fn live_instances(&self) -> Result<Vec<Instance>> {
        let rows = sqlx::query("SELECT * FROM instances WHERE state != 'ended' ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn all_instances(&self, limit: u32) -> Result<Vec<Instance>> {
        let rows = sqlx::query("SELECT * FROM instances ORDER BY created_at DESC LIMIT ?")
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(row).collect()
    }

    pub async fn set_instance_state(&self, name: &str, state: InstanceState, container_id: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE instances SET state = ?, container_id = ?, last_active = ? WHERE name = ?")
            .bind(state.as_str())
            .bind(container_id)
            .bind(ts(Utc::now()))
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn touch_instance(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE instances SET last_active = ? WHERE name = ?")
            .bind(ts(Utc::now()))
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Crash recovery: no container survives the hub's view of it, so everything not ended is
    /// dormant until the reconciler proves otherwise.
    pub async fn reset_running_instances(&self) -> Result<u64> {
        let r = sqlx::query(
            "UPDATE instances SET state = 'dormant', container_id = NULL WHERE state IN ('starting', 'running')",
        )
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }
}
