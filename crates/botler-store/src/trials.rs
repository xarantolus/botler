//! Trial runs: which agent started them, and the effects recorded instead of carried out.

use botler_core::payloads::TrialEffect;
use sqlx::Row;

use crate::{Error, Result, Store};

/// At most this many effects are recorded per trial; later ones are dropped.
pub const MAX_EFFECTS: i64 = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Trial {
    pub run_id: String,
    /// The agent instance that started it and hears how it ended.
    pub instance: String,
    pub branch: String,
    pub minutes: u32,
    pub withheld: Vec<String>,
    pub effects: Vec<TrialEffect>,
}

impl Store {
    pub async fn create_trial(
        &self,
        run_id: &str,
        instance: &str,
        branch: &str,
        minutes: u32,
        withheld: &[String],
    ) -> Result<()> {
        let withheld = serde_json::to_string(withheld).map_err(|e| corrupt(&e))?;
        sqlx::query("INSERT INTO trials (run_id, instance, branch, minutes, withheld) VALUES (?, ?, ?, ?, ?)")
            .bind(run_id)
            .bind(instance)
            .bind(branch)
            .bind(minutes)
            .bind(withheld)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The trial of `run_id`, or `None` if the run is no trial.
    pub async fn trial(&self, run_id: &str) -> Result<Option<Trial>> {
        let row = sqlx::query("SELECT * FROM trials WHERE run_id = ?").bind(run_id).fetch_optional(&self.pool).await?;
        let Some(r) = row else { return Ok(None) };
        let withheld: String = r.get("withheld");
        let effects: String = r.get("effects");
        Ok(Some(Trial {
            run_id: r.get("run_id"),
            instance: r.get("instance"),
            branch: r.get("branch"),
            minutes: r.get("minutes"),
            withheld: serde_json::from_str(&withheld).map_err(|e| corrupt(&e))?,
            effects: serde_json::from_str(&effects).map_err(|e| corrupt(&e))?,
        }))
    }

    /// Append an effect. Returns false once the trial has [`MAX_EFFECTS`].
    pub async fn record_trial_effect(&self, run_id: &str, effect: &TrialEffect) -> Result<bool> {
        let json = serde_json::to_string(effect).map_err(|e| corrupt(&e))?;
        let r = sqlx::query(
            "UPDATE trials SET effects = json_insert(effects, '$[#]', json(?))
             WHERE run_id = ? AND json_array_length(effects) < ?",
        )
        .bind(json)
        .bind(run_id)
        .bind(MAX_EFFECTS)
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected() == 1)
    }
}

fn corrupt(e: &serde_json::Error) -> Error {
    Error::Corrupt { table: "trials", detail: e.to_string() }
}
