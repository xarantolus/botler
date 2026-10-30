//! The `health` op: services report their own health.
//!
//! A service with `[health] heartbeat = "1m"` must call `health(ok=True)` at least that often
//! (after `grace` from its start); the service reconcile treats a missing heartbeat as unhealthy.
//! `health(ok=False, reason=...)` stops it at once. Either way the hub reports an `unhealthy`
//! failure and restarts it with backoff.

use botler_store::RunKind;
use chrono::Utc;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::auth::Principal;
use crate::hub::Hub;
use crate::ops::{Caller, OpError, OpResult, OpSpec};

pub const OP: OpSpec = OpSpec {
    name: "health",
    description: "Report this service's health: `ok=true` is the heartbeat its job.toml `[health]` asks for; `ok=false` with a reason stops and restarts it and reports an unhealthy incident.",
    schema: crate::ops::schema::<Health>,
    visible: |c| matches!(c.principal, Principal::Run { .. }),
};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Health {
    pub ok: bool,
    /// Why it is unhealthy (with `ok = false`).
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn call(hub: &Hub, caller: &Caller, a: Health) -> OpResult {
    let Principal::Run { run_id } = &caller.principal else {
        return Err(OpError::Forbidden("only services report health".into()));
    };
    let run = hub.store.run(run_id).await?;
    if run.kind != RunKind::Service {
        return Err(OpError::BadRequest(format!("{} is a oneshot job; only services report health", run.runnable)));
    }
    if a.ok {
        let recorded = hub.store.heartbeat(run_id, Utc::now()).await?;
        return Ok(json!({ "ok": recorded }));
    }
    let reason = a.reason.unwrap_or_else(|| "reported unhealthy without a reason".into());
    crate::services::unhealthy(hub, &run, &format!("the service reported: {reason}")).await?;
    Ok(json!({ "stopping": true }))
}
