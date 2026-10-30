//! Facts per schedule trigger. Next fire times are always recomputed from these plus the spec.

use chrono::{DateTime, Utc};
use sqlx::Row;

use crate::{Result, Store, parse_opt_ts, parse_ts, ts};

#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleState {
    pub trigger_id: String,
    pub anchor: DateTime<Utc>,
    pub last_fired: Option<DateTime<Utc>>,
    pub skipped: i64,
}

impl Store {
    /// The state for a trigger, creating it anchored at `now` on first sight.
    pub async fn schedule_state(&self, trigger_id: &str, now: DateTime<Utc>) -> Result<ScheduleState> {
        sqlx::query(
            "INSERT INTO schedule_state (trigger_id, anchor) VALUES (?, ?) ON CONFLICT (trigger_id) DO NOTHING",
        )
        .bind(trigger_id)
        .bind(ts(now))
        .execute(&self.pool)
        .await?;
        let r = sqlx::query("SELECT * FROM schedule_state WHERE trigger_id = ?")
            .bind(trigger_id)
            .fetch_one(&self.pool)
            .await?;
        const T: &str = "schedule_state";
        Ok(ScheduleState {
            trigger_id: r.get("trigger_id"),
            anchor: parse_ts(T, r.get("anchor"))?,
            last_fired: parse_opt_ts(T, r.get("last_fired"))?,
            skipped: r.get("skipped"),
        })
    }

    pub async fn record_schedule(&self, trigger_id: &str, last_fired: DateTime<Utc>, skipped: u32) -> Result<()> {
        sqlx::query("UPDATE schedule_state SET last_fired = ?, skipped = skipped + ? WHERE trigger_id = ?")
            .bind(ts(last_fired))
            .bind(i64::from(skipped))
            .bind(trigger_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Forget state for triggers that no longer exist, so a re-added trigger starts fresh.
    pub async fn prune_schedules(&self, keep: &[String]) -> Result<u64> {
        let all = sqlx::query("SELECT trigger_id FROM schedule_state").fetch_all(&self.pool).await?;
        let mut removed = 0;
        for r in all {
            let id: String = r.get("trigger_id");
            if !keep.contains(&id) {
                sqlx::query("DELETE FROM schedule_state WHERE trigger_id = ?").bind(&id).execute(&self.pool).await?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}
