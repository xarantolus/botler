//! Incidents and run logs: what triage reads and resolves.

use std::sync::Arc;

use botler_core::defs::Capability;
use botler_store::{Incident, Run, RunStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::{Hub, HubError};

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct GetRunLogs {
    pub run_id: String,
    /// Last N lines (default 200).
    #[serde(default)]
    pub tail: Option<usize>,
}

/// A run with the tail of its output.
#[derive(Debug, Serialize, TS)]
pub struct RunLogs {
    pub run: Run,
    pub logs: String,
}

impl Op for GetRunLogs {
    const NAME: &'static str = "get_run_logs";
    const DESCRIPTION: &'static str = "Output of a job run.";
    const WHO: Who = Who::UserAndAgents;
    type Output = RunLogs;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<RunLogs, OpError> {
        let run = hub.store.run(&self.run_id).await?;
        let saved = hub.paths.run_log(&run.id);
        let logs = match tokio::fs::read_to_string(&saved).await {
            Ok(l) => l,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Still running: read live.
                let name = crate::jobs::run_container(hub, &run);
                match hub.containers.logs(&name, 2000).await {
                    Ok(l) => l,
                    Err(botler_runtime::ContainerError::NotFound(_)) => match run.status {
                        RunStatus::Queued => "(not started yet: its image is building or it waits its turn)\n".into(),
                        _ => "(no output: its container is gone and nothing was kept)\n".into(),
                    },
                    Err(e) => return Err(HubError::from(e).into()),
                }
            }
            Err(e) => return Err(HubError::Io(e).into()),
        };
        Ok(RunLogs { run, logs: crate::failures::tail(&logs, self.tail.unwrap_or(200)) })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GetIncident {
    pub id: String,
}

impl Op for GetIncident {
    const NAME: &'static str = "get_incident";
    const DESCRIPTION: &'static str = "One incident: what failed, how often, how it was resolved.";
    const WHO: Who = Who::Capable(Capability::Incidents);
    type Output = Incident;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Incident, OpError> {
        Ok(hub.store.incident(&self.id).await?)
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GetIncidentHistory {
    pub runnable: String,
}

impl Op for GetIncidentHistory {
    const NAME: &'static str = "get_incident_history";
    const DESCRIPTION: &'static str =
        "Past incidents of a job or service with their resolutions, newest first. Check this before fixing anything.";
    const WHO: Who = Who::Capable(Capability::Incidents);
    type Output = Vec<Incident>;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Vec<Incident>, OpError> {
        Ok(hub.store.incidents_for(&self.runnable, 20).await?)
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ResolveIncident {
    pub id: String,
    /// Cause and what you did. Future triage reads this.
    pub resolution: String,
}

#[derive(Debug, Serialize, TS)]
pub struct IncidentResolved {
    pub resolved: String,
}

impl Op for ResolveIncident {
    const NAME: &'static str = "resolve_incident";
    const DESCRIPTION: &'static str = "Close an incident with what caused it and what you did (or why nothing needs doing). Required at the end of every triage.";
    const WHO: Who = Who::Capable(Capability::Incidents);
    type Output = IncidentResolved;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<IncidentResolved, OpError> {
        if self.resolution.trim().len() < 10 {
            return Err(OpError::BadRequest("describe the cause and what you did".into()));
        }
        hub.store.resolve_incident(&self.id, &self.resolution).await?;
        Ok(IncidentResolved { resolved: self.id })
    }
}
