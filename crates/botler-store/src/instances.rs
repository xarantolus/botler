//! Agent instances and their lifecycle state.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Error, Result, Store, parse_ts, text_enum, ts};

text_enum!(InstanceState {
    Dormant = "dormant",
    Starting = "starting",
    Running = "running",
    /// Hidden, with a handover report; restored by messaging it. Nothing is deleted.
    Archived = "archived",
});

#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
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
    /// Spawn depth: 0 for chats not spawned by an agent, the parent's depth + 1 otherwise.
    pub depth: u32,
    pub task: Option<String>,
    pub title: Option<String>,
    /// Unique human-friendly alias, e.g. `groundstation-antenna-fix`.
    pub slug: Option<String>,
    /// Set while the hub waits for the agent's handover before archiving.
    pub archive_requested_at: Option<DateTime<Utc>>,
    /// How it was archived, with the agent's handover report when it gave one.
    pub archive_report: Option<ArchiveRecord>,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
    /// Pinned by the user: listed right after the front door.
    pub pinned: bool,
    /// The user named it: agents keep its title.
    pub titled_by_user: bool,
    /// When the user, another chat or the chat itself last wrote in it (the list's order); its
    /// creation before that.
    pub last_message_at: DateTime<Utc>,
}

/// The record kept when a chat is archived.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct ArchiveRecord {
    /// Whether the agent handed over (`handover_ready`) rather than being archived by force.
    pub confirmed_by_agent: bool,
    #[serde(default)]
    pub forced: bool,
    /// Why it was archived, if the user said.
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub report: Option<HandoverReport>,
    /// The chat the outstanding work went to.
    #[serde(default)]
    pub handed_to: Option<String>,
    pub at: DateTime<Utc>,
    /// Disk space freed by cleaning and packing its home after archiving.
    #[serde(default)]
    pub freed_bytes: Option<u64>,
}

/// An agent's handover when its chat is archived.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct HandoverReport {
    pub summary: String,
    #[serde(default)]
    pub outstanding: Vec<String>,
    /// Where its state lives: branches, files, memory notes, external systems.
    #[serde(default)]
    pub state: Vec<String>,
    #[serde(default)]
    pub handoff: Option<Handoff>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
pub struct Handoff {
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub spawn: Option<String>,
    pub message: String,
}

/// An instance whose turn was in flight when the hub stopped (see [`Store::take_interrupted_turns`]).
#[derive(Clone, Debug, PartialEq)]
pub struct InterruptedTurn {
    pub instance: String,
    pub started_at: DateTime<Utc>,
    /// Boots in a row that found it mid-turn, this one included.
    pub interruptions: u32,
}

#[derive(Clone, Debug)]
pub struct NewInstance {
    pub name: String,
    pub definition: String,
    pub persistent: bool,
    pub session_file: String,
    pub branch: Option<String>,
    pub parent: Option<String>,
    pub depth: u32,
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
        depth: u32::try_from(r.get::<i64, _>("depth"))
            .map_err(|e| Error::Corrupt { table: T, detail: format!("bad depth: {e}") })?,
        task: r.get("task"),
        title: r.get("title"),
        slug: r.get("slug"),
        archive_requested_at: crate::parse_opt_ts(T, r.get("archive_requested_at"))?,
        archive_report: r
            .get::<Option<String>, _>("archive_report")
            .map(|s| serde_json::from_str(&s).map_err(|e| Error::Corrupt { table: T, detail: e.to_string() }))
            .transpose()?,
        created_at: parse_ts(T, r.get("created_at"))?,
        last_active: parse_ts(T, r.get("last_active"))?,
        pinned: r.get::<i64, _>("pinned") != 0,
        titled_by_user: r.get::<i64, _>("titled_by_user") != 0,
        last_message_at: match r.get::<Option<String>, _>("last_message_at") {
            Some(t) => parse_ts(T, &t)?,
            None => parse_ts(T, r.get("created_at"))?,
        },
    })
}

impl Store {
    pub async fn create_instance(&self, n: &NewInstance) -> Result<Instance> {
        let now = ts(Utc::now());
        let res = sqlx::query(
            "INSERT INTO instances (name, definition, persistent, state, session_file, branch, parent, depth, task, created_at, last_active)
             VALUES (?, ?, ?, 'dormant', ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (name) DO NOTHING",
        )
        .bind(&n.name)
        .bind(&n.definition)
        .bind(i64::from(n.persistent))
        .bind(&n.session_file)
        .bind(&n.branch)
        .bind(&n.parent)
        .bind(n.depth)
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

    /// Find an instance by its id or its slug.
    pub async fn resolve_instance(&self, name_or_slug: &str) -> Result<Option<Instance>> {
        let r = sqlx::query("SELECT * FROM instances WHERE name = ? OR slug = ? ORDER BY (name = ?) DESC LIMIT 1")
            .bind(name_or_slug)
            .bind(name_or_slug)
            .bind(name_or_slug)
            .fetch_optional(&self.pool)
            .await?;
        r.as_ref().map(row).transpose()
    }

    /// Set a chat's title and slug. Fails with [`Error::SlugTaken`] if another instance has it.
    pub async fn set_instance_title(&self, name: &str, title: &str, slug: &str) -> Result<()> {
        if let Some(other) = self.resolve_instance(slug).await?
            && other.name != name
        {
            return Err(Error::SlugTaken(slug.into()));
        }
        let r = sqlx::query("UPDATE instances SET title = ?, slug = ? WHERE name = ?")
            .bind(title)
            .bind(slug)
            .bind(name)
            .execute(&self.pool)
            .await;
        match r {
            Ok(r) if r.rows_affected() == 0 => Err(crate::not_found("instance", name)),
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(Error::SlugTaken(slug.into())),
            Err(e) => Err(e.into()),
        }
    }

    /// All instances that have not ended, newest first.
    pub async fn live_instances(&self) -> Result<Vec<Instance>> {
        let rows = sqlx::query("SELECT * FROM instances WHERE state != 'archived' ORDER BY created_at DESC")
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

    /// Someone wrote in chat `name` (the user or another chat, or the chat answering them).
    pub async fn touch_message(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE instances SET last_message_at = ? WHERE name = ?")
            .bind(ts(Utc::now()))
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The user's title for chat `name`; agents keep it from then on.
    pub async fn set_user_title(&self, name: &str, title: &str) -> Result<()> {
        sqlx::query("UPDATE instances SET title = ?, titled_by_user = 1 WHERE name = ?")
            .bind(title)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_pinned(&self, name: &str, pinned: bool) -> Result<()> {
        sqlx::query("UPDATE instances SET pinned = ? WHERE name = ?")
            .bind(i64::from(pinned))
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

    /// Mark that the hub asked this chat to prepare for archiving.
    pub async fn request_archive(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE instances SET archive_requested_at = ? WHERE name = ?")
            .bind(ts(Utc::now()))
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Archive with the agent's (or a forced) handover report.
    pub async fn archive_instance(&self, name: &str, report: &ArchiveRecord) -> Result<()> {
        let r = sqlx::query(
            "UPDATE instances SET state = 'archived', container_id = NULL, archive_report = ?, archive_requested_at = NULL WHERE name = ?",
        )
        .bind(crate::encode("archive report", report)?)
        .bind(name)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Err(crate::not_found("instance", name));
        }
        Ok(())
    }

    /// Continue `name` in session file `file` (a rollover; the old file stays).
    pub async fn set_session_file(&self, name: &str, file: &str) -> Result<()> {
        let r = sqlx::query("UPDATE instances SET session_file = ? WHERE name = ?")
            .bind(file)
            .bind(name)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(crate::not_found("instance", name));
        }
        Ok(())
    }

    /// Record the space freed by packing an archived chat's home in its archive report.
    pub async fn set_archive_freed(&self, name: &str, bytes: u64) -> Result<()> {
        sqlx::query("UPDATE instances SET archive_report = json_set(archive_report, '$.freed_bytes', ?) WHERE name = ? AND archive_report IS NOT NULL")
            .bind(bytes as i64)
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Chats that may be archived automatically: dormant, not persistent, no archive pending,
    /// inactive since `before`. Oldest first.
    pub async fn archivable_instances(&self, before: DateTime<Utc>) -> Result<Vec<Instance>> {
        let rows = sqlx::query(
            "SELECT * FROM instances WHERE state = 'dormant' AND persistent = 0 AND archive_requested_at IS NULL AND last_active < ? ORDER BY last_active",
        )
        .bind(ts(before))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Chats whose handover was asked for before `before` and never came.
    pub async fn stale_archive_requests(&self, before: DateTime<Utc>) -> Result<Vec<Instance>> {
        let rows = sqlx::query(
            "SELECT * FROM instances WHERE state != 'archived' AND archive_requested_at IS NOT NULL AND archive_requested_at < ?",
        )
        .bind(ts(before))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row).collect()
    }

    /// Delete an archived chat for good: its row and what only it uses (queued deliveries,
    /// timers it set or that resume it, its state, its breaker). The event journal, runs, usage,
    /// dialogs and proposals are history and stay. Returns false if it is not archived.
    pub async fn delete_archived_instance(&self, name: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let r = sqlx::query("DELETE FROM instances WHERE name = ? AND state = 'archived'")
            .bind(name)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() == 0 {
            return Ok(false);
        }
        let (agent, subject) = (format!("agent:{name}"), format!("instance:{name}"));
        let target = crate::encode("timer target", &serde_json::json!({ "resume": name }))?;
        for (sql, v) in [
            ("DELETE FROM deliveries WHERE instance = ? AND status = 'pending'", name),
            ("DELETE FROM timers WHERE status = 'pending' AND created_by = ?", &agent),
            ("DELETE FROM timers WHERE status = 'pending' AND target = ?", &target),
            ("DELETE FROM kv WHERE owner = ?", &agent),
            ("DELETE FROM breakers WHERE subject = ?", &subject),
        ] {
            sqlx::query(sql).bind(v).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Point an instance at another agent definition (restoring a chat whose own is gone).
    pub async fn set_instance_definition(&self, name: &str, definition: &str) -> Result<()> {
        let r = sqlx::query("UPDATE instances SET definition = ? WHERE name = ?")
            .bind(definition)
            .bind(name)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(crate::not_found("instance", name));
        }
        Ok(())
    }

    /// Restore an archived chat to sleeping; its report stays for reference.
    pub async fn restore_instance(&self, name: &str) -> Result<()> {
        sqlx::query(
            "UPDATE instances SET state = 'dormant', archive_requested_at = NULL WHERE name = ? AND state = 'archived'",
        )
        .bind(name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Rename instance `old` (and a definition of the same name) to `new` everywhere the hub
    /// refers to it as a live target: the instance row, children's parent, the delivery queue,
    /// dialogs, proposals, usage, timers (creator and `resume` target), state, breakers,
    /// incidents, schedule state of the definition's own triggers, and events not yet routed.
    /// The routed event journal is history and keeps the old name. One transaction; returns
    /// false (and changes nothing) if `old` does not exist or `new` already does.
    pub async fn rename_instance(&self, old: &str, new: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let names: Vec<String> = sqlx::query_scalar("SELECT name FROM instances WHERE name IN (?, ?)")
            .bind(old)
            .bind(new)
            .fetch_all(&mut *tx)
            .await?;
        if names != [old] {
            return Ok(false);
        }
        let (old_agent, new_agent) = (format!("agent:{old}"), format!("agent:{new}"));
        let (old_inst, new_inst) = (format!("instance:{old}"), format!("instance:{new}"));
        let (old_target, new_target) = (
            crate::encode("timer target", &serde_json::json!({ "resume": old }))?,
            crate::encode("timer target", &serde_json::json!({ "resume": new }))?,
        );
        let (old_prefix, new_prefix) = (format!("agents/{old}/"), format!("agents/{new}/"));
        let renames: [(&str, &str, &str); 16] = [
            ("UPDATE instances SET name = ? WHERE name = ?", new, old),
            ("UPDATE instances SET definition = ? WHERE definition = ?", new, old),
            ("UPDATE instances SET parent = ? WHERE parent = ?", new, old),
            ("UPDATE deliveries SET instance = ? WHERE instance = ?", new, old),
            ("UPDATE dialogs SET instance = ? WHERE instance = ?", new, old),
            ("UPDATE proposals SET instance = ? WHERE instance = ?", new, old),
            ("UPDATE usage SET instance = ? WHERE instance = ?", new, old),
            ("UPDATE grants SET holder = ? WHERE holder = ?", new, old),
            ("UPDATE subscriptions SET instance = ? WHERE instance = ?", new, old),
            ("UPDATE timers SET created_by = ? WHERE created_by = ?", &new_agent, &old_agent),
            ("UPDATE timers SET target = ? WHERE target = ?", &new_target, &old_target),
            ("UPDATE kv SET owner = ? WHERE owner = ?", &new_agent, &old_agent),
            ("UPDATE breakers SET subject = ? WHERE subject = ?", &new_inst, &old_inst),
            ("UPDATE incidents SET runnable = ? WHERE runnable = ?", &new_agent, &old_agent),
            (
                "UPDATE events SET to_instance = ?1, body = json_set(body, '$.to', ?1) WHERE routed = 0 AND to_instance = ?2",
                new,
                old,
            ),
            (
                "UPDATE schedule_state SET trigger_id = ?1 || substr(trigger_id, length(?2) + 1) WHERE substr(trigger_id, 1, length(?2)) = ?2",
                &new_prefix,
                &old_prefix,
            ),
        ];
        for (sql, to, from) in renames {
            sqlx::query(sql).bind(to).bind(from).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// `name` has a turn in flight (a turn-starting delivery handed to pi, or pi started one).
    /// Written when it starts, so a hard crash of the hub leaves it set.
    pub async fn begin_turn(&self, name: &str) -> Result<()> {
        sqlx::query("UPDATE instances SET turn_started_at = COALESCE(turn_started_at, ?) WHERE name = ?")
            .bind(ts(Utc::now()))
            .bind(name)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// `name`'s turn is over: it `settled` (which also resets the interruption count), or no turn
    /// is running any more (its container exited, the prompt started none).
    pub async fn end_turn(&self, name: &str, settled: bool) -> Result<()> {
        let sql = if settled {
            "UPDATE instances SET turn_started_at = NULL, turn_interruptions = 0 WHERE name = ?"
        } else {
            "UPDATE instances SET turn_started_at = NULL WHERE name = ?"
        };
        sqlx::query(sql).bind(name).execute(&self.pool).await?;
        Ok(())
    }

    /// At boot: the instances that were mid-turn when the hub stopped. Their marker is cleared and
    /// their interruption count bumped.
    pub async fn take_interrupted_turns(&self) -> Result<Vec<InterruptedTurn>> {
        const T: &str = "instances";
        // Writing first takes the write lock up front (a read upgraded later fails when busy).
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE instances SET turn_interruptions = turn_interruptions + 1 WHERE turn_started_at IS NOT NULL",
        )
        .execute(&mut *tx)
        .await?;
        let rows = sqlx::query(
            "SELECT name, turn_started_at, turn_interruptions FROM instances WHERE turn_started_at IS NOT NULL",
        )
        .fetch_all(&mut *tx)
        .await?;
        sqlx::query("UPDATE instances SET turn_started_at = NULL WHERE turn_started_at IS NOT NULL")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        rows.iter()
            .map(|r| {
                Ok(InterruptedTurn {
                    instance: r.get("name"),
                    started_at: parse_ts(T, r.get("turn_started_at"))?,
                    interruptions: u32::try_from(r.get::<i64, _>("turn_interruptions"))
                        .map_err(|e| Error::Corrupt { table: T, detail: format!("bad turn_interruptions: {e}") })?,
                })
            })
            .collect()
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
