//! The `health` op: services report their own health.
//!
//! A service with `[health] heartbeat = "1m"` must call `health(ok=True)` at least that often
//! (after `grace` from its start); the service reconcile treats a missing heartbeat as unhealthy.
//! `health(ok=False, reason=...)` stops it at once. Either way the hub reports an `unhealthy`
//! failure and restarts it with backoff.

use std::sync::Arc;

use botler_core::payloads::TrialOutcome;
use botler_store::RunKind;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::Principal;
use crate::hub::Hub;
use crate::ops::{Caller, Op, OpError, Who};

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct Health {
    pub ok: bool,
    /// Why it is unhealthy (with `ok = false`).
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, TS)]
#[serde(untagged)]
pub enum HealthRecorded {
    /// A heartbeat; `ok` is false if the run had already ended.
    Heartbeat { ok: bool },
    /// Unhealthy: the service is being stopped.
    Stopping { stopping: bool },
}

impl Op for Health {
    const NAME: &'static str = "health";
    const DESCRIPTION: &'static str = "Report this service's health: `ok=true` is the heartbeat its job.toml `[health]` asks for; `ok=false` with a reason stops and restarts it and reports an unhealthy incident.";
    const WHO: Who = Who::Runs;
    type Output = HealthRecorded;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<HealthRecorded, OpError> {
        let Principal::Run { run_id } = &caller.principal else {
            return Err(OpError::Forbidden("only services report health".into()));
        };
        let run = hub.store.run(run_id).await?;
        if run.kind == RunKind::Trial && !self.ok {
            let reason = format!("reported unhealthy: {}", self.reason.as_deref().unwrap_or("no reason given"));
            crate::trials::finish(hub, &run, TrialOutcome::Failed, None, Some(reason)).await?;
            return Ok(HealthRecorded::Stopping { stopping: true });
        }
        if !matches!(run.kind, RunKind::Service | RunKind::Trial) {
            return Err(OpError::BadRequest(format!("{} is a oneshot job; only services report health", run.runnable)));
        }
        if self.ok {
            let recorded = hub.store.heartbeat(run_id, Utc::now()).await?;
            return Ok(HealthRecorded::Heartbeat { ok: recorded });
        }
        let reason = self.reason.unwrap_or_else(|| "reported unhealthy without a reason".into());
        crate::services::unhealthy(hub, &run, &format!("the service reported: {reason}")).await?;
        Ok(HealthRecorded::Stopping { stopping: true })
    }
}
