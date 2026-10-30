//! Starting jobs and restarting services by hand (the web app's System view).

use std::sync::Arc;

use botler_core::defs::{Lifetime, Overlap, Target};
use botler_core::payloads::{BuiltinPayload, ManualPayload};
use botler_store::RunStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::{Hub, HubError};

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct RunJob {
    pub job: String,
}

#[derive(Debug, Serialize, TS)]
pub struct JobStarted {
    pub run_id: String,
    /// Waiting for the active run to finish (overlap = queue).
    pub queued: bool,
}

impl Op for RunJob {
    const NAME: &'static str = "run_job";
    const DESCRIPTION: &'static str = "Start a oneshot job now, with a `manual` event. Honours pause and the overlap policy of the job's trigger (skip by default).";
    const WHO: Who = Who::User;
    type Output = JobStarted;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<JobStarted, OpError> {
        if hub.paused().await? {
            return Err(HubError::Paused.into());
        }
        if hub.applied().await.defs.jobs.get(&self.job).is_some_and(|j| !j.spec.enabled) {
            return Err(OpError::Conflict(format!("{} is disabled (enabled = false in its job.toml)", self.job)));
        }
        // A job has no overlap policy of its own; its triggers do. Use the first one's.
        let overlap = hub
            .applied()
            .await
            .defs
            .triggers
            .values()
            .find(|t| t.target == Target::Run(self.job.clone()))
            .map_or_else(Overlap::default, |t| t.spec.overlap);
        let payload = BuiltinPayload::Manual(ManualPayload { runnable: self.job.clone() });
        let ev = crate::hub::builtin(payload, caller.source())?;
        // Journaled for the run's record; a manual start reaches no one else.
        hub.store.append_event(&ev).await?;
        hub.store.mark_routed(&ev.id.to_string()).await?;
        let Some(admitted) = crate::jobs::admit(hub, &self.job, None, Some(&ev), overlap).await? else {
            return Err(OpError::Conflict(format!("{} is still running (overlap = skip)", self.job)));
        };
        let run_id = admitted.run.id.clone();
        if admitted.launch {
            // Building the image can take minutes: answer now, start in the background. Start
            // failures finish the run and are reported like any other.
            let hub = hub.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::jobs::launch(&hub, &admitted.run, Some(&ev)).await {
                    tracing::error!(run = %admitted.run.id, error = %e, "could not record the manual run's start");
                }
            });
        }
        Ok(JobStarted { run_id, queued: !admitted.launch })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct RestartService {
    pub service: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ServiceRestarted {
    /// The run that was stopped, if one was running.
    pub stopped: Option<String>,
}

impl Op for RestartService {
    const NAME: &'static str = "restart_service";
    const DESCRIPTION: &'static str = "Stop a service; the reconciler starts it again right away.";
    const WHO: Who = Who::User;
    type Output = ServiceRestarted;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ServiceRestarted, OpError> {
        if hub.paused().await? {
            return Err(HubError::Paused.into());
        }
        let applied = hub.applied().await;
        if !applied.defs.jobs.get(&self.service).is_some_and(|j| j.spec.lifetime == Lifetime::Service) {
            return Err(HubError::Undefined { kind: "service", name: self.service }.into());
        }
        let run = hub.store.runs_for(&self.service, 1).await?.into_iter().next();
        let stopped = match run.filter(|r| r.status == RunStatus::Running) {
            Some(r) => {
                crate::services::stop(hub, &r, "restarted by the user").await?;
                Some(r.id)
            }
            None => None,
        };
        hub.reconcile_notify.notify_one();
        Ok(ServiceRestarted { stopped })
    }
}
