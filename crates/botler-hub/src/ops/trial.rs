//! Trying a job or service from your branch before proposing it (see `trials.rs`).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::{Hub, HubError, load_at};
use crate::trials::{self, MAX_MINUTES};

fn minutes(asked: Option<u32>) -> Result<u32, OpError> {
    let minutes = asked.unwrap_or(3);
    if !(1..=MAX_MINUTES).contains(&minutes) {
        return Err(OpError::BadRequest(format!("minutes must be 1 to {MAX_MINUTES}")));
    }
    Ok(minutes)
}

/// The head and definitions of `instance`'s own pushed `branch`.
async fn branch_defs(hub: &Hub, instance: &str, branch: &str) -> Result<(String, botler_core::Definitions), OpError> {
    let prefix = format!("agents/{instance}/");
    if !branch.starts_with(&prefix) {
        return Err(OpError::Forbidden(format!("you can only try your own branches ({prefix}...)")));
    }
    let head = hub.repo.rev(&format!("refs/heads/{branch}")).await.map_err(HubError::from)?.ok_or_else(|| {
        OpError::BadRequest(format!("branch {branch} is not on the hub; `git push origin HEAD:{branch}` first"))
    })?;
    match load_at(&hub.repo, &head).await {
        Ok(a) => Ok((head, a.defs)),
        Err(HubError::InvalidDefinitions { diagnostics, .. }) => {
            let list: Vec<String> = diagnostics.iter().map(|d| format!("- {d}")).collect();
            Err(OpError::BadRequest(format!("definitions do not load:\n{}", list.join("\n"))))
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ReplayEvent {
    /// The past event to feed the job (`/botler/event.json`, as when it first ran).
    pub event_id: String,
    pub job: String,
    /// Your pushed branch to take the job from (to test a fix); main when unset.
    #[serde(default)]
    pub branch: Option<String>,
    /// How long it may run, 1 to 10 (default 3).
    #[serde(default)]
    pub minutes: Option<u32>,
}

impl Op for ReplayEvent {
    const NAME: &'static str = "replay_event";
    const DESCRIPTION: &'static str = "Re-run a job against a past event, as a dry run: main's definition and image, or your pushed branch's to test a fix. Like trial_run: only secret groups you hold, its own empty `state`, and emit_event, send_message and set_timer are recorded, not carried out. The outcome arrives as trial.finished.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = TrialStarted;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<TrialStarted, OpError> {
        let instance = caller.agent("only agents replay events")?;
        let minutes = minutes(self.minutes)?;
        let event = hub.store.event(&self.event_id).await?;
        let (branch, head, defs) = match &self.branch {
            Some(b) => {
                let (head, defs) = branch_defs(hub, instance, b).await?;
                (b.clone(), head, defs)
            }
            None => {
                let applied = hub.applied().await;
                ("main".to_string(), applied.commit.clone(), applied.defs.clone())
            }
        };
        let def = defs
            .jobs
            .get(&self.job)
            .ok_or_else(|| OpError::BadRequest(format!("no job `{}` on {branch}", self.job)))?;
        let (run, withheld) = trials::start(hub, instance, &branch, &head, def, minutes, Some(event)).await?;
        Ok(TrialStarted {
            run_id: run.id,
            minutes,
            withheld_secrets: withheld,
            note: "Replaying as a dry run; the outcome arrives as a trial.finished event.".into(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct TrialRun {
    /// The job or service (`jobs/<job>/` on the branch).
    pub job: String,
    /// Your pushed branch, e.g. `agents/botler/mail-watch`.
    pub branch: String,
    /// How long it may run, 1 to 10 (default 3). Then it is stopped: the normal end of a service.
    #[serde(default)]
    pub minutes: Option<u32>,
}

#[derive(Debug, Serialize, TS)]
pub struct TrialStarted {
    pub run_id: String,
    pub minutes: u32,
    /// Secret groups (and `mount:<name>` shares) the job declares that you do not hold: it runs
    /// without them. Shares you hold are mounted read-only.
    pub withheld_secrets: Vec<String>,
    pub note: String,
}

impl Op for TrialRun {
    const NAME: &'static str = "trial_run";
    const DESCRIPTION: &'static str = "Try a job or service from your pushed branch (agents/<you>/...) for a few minutes before proposing it: its definition and image from the branch, only secret groups you hold, its own empty `state`. emit_event, send_message and set_timer are recorded, not carried out (they return dry_run: true). When it exits or its minutes are up, a trial.finished event brings the outcome, logs tail and the events it would have emitted.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = TrialStarted;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<TrialStarted, OpError> {
        let instance = caller.agent("only agents start trials")?;
        let minutes = minutes(self.minutes)?;
        let (head, defs) = branch_defs(hub, instance, &self.branch).await?;
        let def = defs
            .jobs
            .get(&self.job)
            .ok_or_else(|| OpError::BadRequest(format!("no job `{}` on {}", self.job, self.branch)))?;
        let (run, withheld) = trials::start(hub, instance, &self.branch, &head, def, minutes, None).await?;
        Ok(TrialStarted {
            run_id: run.id,
            minutes,
            withheld_secrets: withheld,
            note: "Building and running; the outcome arrives as a trial.finished event.".into(),
        })
    }
}
