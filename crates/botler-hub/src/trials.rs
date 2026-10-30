//! Trial runs (`trial_run`): an agent tries a job or service from its own branch before proposing
//! it. The run uses the branch's definition and image, gets only the secret groups the agent
//! holds, never touches production state, and stops after its minutes. What it emits, sends or
//! schedules is recorded instead of carried out; when it ends, a `trial.finished` event carries
//! the outcome, the logs tail and those effects to the agent. Trials never open incidents and
//! never count as the job's own runs (see `botler_store::Store::runs_for`).

use std::sync::Arc;

use botler_core::Source;
use botler_core::defs::{Definitions, JobDef, SecretClass};
use botler_core::payloads::{BuiltinPayload, TrialEffect, TrialFinishedPayload, TrialOutcome};
use botler_store::{NewRun, Run, RunKind, RunStatus};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::auth::Principal;
use crate::hub::{Hub, HubError, Result, load_at};
use crate::ops::Caller;
use crate::{failures, jobs};

pub const MAX_MINUTES: u32 = 10;
/// A trial still queued (its image building) this long after it was created never started.
const START_DEADLINE: Duration = Duration::minutes(30);

/// The secret groups `instance` holds.
///
/// Seam for per-chat grants: until they land, an instance holds the groups its definition lists.
/// Grants replace this one function.
async fn held_secret_groups(hub: &Hub, instance: &str) -> Result<Vec<String>> {
    let applied = hub.applied().await;
    let definition = hub.store.find_instance(instance).await?.map(|i| i.definition);
    let def = definition.as_deref().and_then(|d| applied.defs.agents.get(d));
    Ok(def.map(|d| d.spec.env.secrets.clone()).unwrap_or_default())
}

/// Start a trial of `def` (from commit `head` of `branch`) for `instance`, stopped after `minutes`.
/// The image builds in the background; the outcome arrives as `trial.finished`. Returns the run
/// and the secret groups withheld.
pub async fn start(
    hub: &Arc<Hub>,
    instance: &str,
    branch: &str,
    head: &str,
    def: &JobDef,
    minutes: u32,
) -> Result<(Run, Vec<String>)> {
    if hub.paused().await? {
        return Err(HubError::Paused);
    }
    for run in hub.store.active_runs().await?.iter().filter(|r| r.kind == RunKind::Trial) {
        if hub.store.trial(&run.id).await?.is_some_and(|t| t.instance == instance) {
            return Err(HubError::Forbidden(format!(
                "your trial {} of {} is still running; wait for its trial.finished",
                run.id, run.runnable
            )));
        }
    }
    // Only container-class secrets of groups the job declares and the caller holds; secret
    // declarations come from main, never from the branch.
    let held = held_secret_groups(hub, instance).await?;
    let applied = hub.applied().await;
    let container_only = |g: &String| {
        applied.defs.secrets_for(std::slice::from_ref(g), &[]).iter().all(|s| s.class == SecretClass::Container)
    };
    let mut env = def.spec.env.clone();
    let (given, withheld): (Vec<String>, Vec<String>) =
        env.secrets.drain(..).partition(|g| held.contains(g) && container_only(g));
    env.secrets = given;
    // Not reachable as the service, nor from the network: those belong to the deployed one.
    env.port = None;
    env.publish.clear();

    let run = hub
        .store
        .create_run(&NewRun {
            runnable: def.name.clone(),
            kind: RunKind::Trial,
            trigger_id: None,
            event_id: None,
            commit_id: Some(head.to_string()),
        })
        .await?;
    hub.store.create_trial(&run.id, instance, branch, minutes, &withheld).await?;
    tracing::info!(run = %run.id, job = %def.name, %branch, minutes, "trial starting");
    let (hub2, run2, job) = (hub.clone(), run.clone(), def.spec.clone());
    tokio::spawn(async move {
        let timeout = Duration::minutes(i64::from(minutes));
        if let Err(e) = jobs::launch_job(&hub2, &run2, &job, &env, timeout, None).await {
            tracing::warn!(run = %run2.id, error = %e, "trial could not start");
            if let Err(e2) =
                finish(&hub2, &run2, TrialOutcome::Failed, None, Some(format!("could not start: {e}"))).await
            {
                tracing::error!(run = %run2.id, error = %e2, "could not finish the trial");
            }
        }
    });
    Ok((run, withheld))
}

/// The trial run `caller` is, if any: its effects are recorded instead of carried out.
pub async fn of_caller(hub: &Hub, caller: &Caller) -> Result<Option<String>> {
    let Principal::Run { run_id } = &caller.principal else { return Ok(None) };
    Ok((hub.store.run(run_id).await?.kind == RunKind::Trial).then(|| run_id.clone()))
}

/// The definitions trial `run_id` runs with: its branch's.
pub async fn definitions(hub: &Hub, run_id: &str) -> Result<Definitions> {
    let at = hub.store.run(run_id).await?.commit_id;
    let at = at.ok_or_else(|| HubError::Undefined { kind: "commit of trial", name: run_id.into() })?;
    Ok(load_at(&hub.repo, &at).await?.defs)
}

/// Record what trial `run_id` would have done with `op`.
pub async fn record(hub: &Hub, run_id: &str, op: &str, detail: &impl Serialize) -> Result<()> {
    let detail = serde_json::to_value(detail).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    let effect = TrialEffect { op: op.into(), detail, at: Utc::now() };
    if !hub.store.record_trial_effect(run_id, &effect).await? {
        return Err(HubError::Forbidden(format!(
            "this trial already recorded {} effects; that is plenty to check",
            botler_store::MAX_TRIAL_EFFECTS
        )));
    }
    Ok(())
}

/// The `state` namespace of `caller`: trials get their own, empty at start, so production cursors
/// are neither read nor moved.
pub async fn state_owner(hub: &Hub, caller: &Caller) -> Result<String> {
    Ok(match of_caller(hub, caller).await? {
        Some(run_id) => format!("trial:{run_id}"),
        None => caller.subject(),
    })
}

/// The trial's container exited by itself.
pub async fn on_exit(hub: &Hub, run: &Run, exit_code: Option<i64>, oom: bool) -> Result<()> {
    let ok = exit_code == Some(0) && !oom;
    let reason = (!ok).then(|| if oom { "out of memory".to_string() } else { format!("exit code {exit_code:?}") });
    let outcome = if ok { TrialOutcome::Succeeded } else { TrialOutcome::Failed };
    finish(hub, run, outcome, exit_code, reason).await
}

/// Stop a running trial whose minutes are up, and give up on one that never started.
pub async fn sweep(hub: &Hub, run: &Run, now: DateTime<Utc>) -> Result<()> {
    match (run.status, run.started_at) {
        (RunStatus::Running, Some(started)) => {
            let Some(t) = hub.store.trial(&run.id).await? else { return Ok(()) };
            if now - started >= Duration::minutes(i64::from(t.minutes)) {
                let why = format!("stopped after {} minutes, as asked", t.minutes);
                return finish(hub, run, TrialOutcome::TimeUp, None, Some(why)).await;
            }
            Ok(())
        }
        (RunStatus::Queued, _) if now - run.created_at > START_DEADLINE => {
            finish(hub, run, TrialOutcome::Failed, None, Some("never started (hub restarted?)".into())).await
        }
        _ => Ok(()),
    }
}

/// End trial `run` (once): stop and remove its container and tell the agent that started it.
pub async fn finish(
    hub: &Hub,
    run: &Run,
    outcome: TrialOutcome,
    exit_code: Option<i64>,
    reason: Option<String>,
) -> Result<()> {
    let status = match outcome {
        TrialOutcome::Succeeded => RunStatus::Succeeded,
        TrialOutcome::Failed => RunStatus::Failed,
        TrialOutcome::TimeUp => RunStatus::Cancelled,
    };
    if !hub.store.finish_run(&run.id, status, exit_code, reason.as_deref()).await? {
        return Ok(()); // something else ended it first
    }
    let started = hub.store.run(&run.id).await?.container_id.is_some();
    let logs = if started {
        let name = jobs::run_container(hub, run);
        match hub.containers.stop(&name, 10).await {
            Ok(()) | Err(botler_runtime::ContainerError::NotFound(_)) => {}
            Err(e) => return Err(e.into()),
        }
        let logs = jobs::collect_logs(hub, &name, &run.id).await;
        jobs::finalize_container(hub, run).await;
        logs
    } else {
        crate::environment::cleanup_secrets(&hub.paths.root.join("runs").join(&run.id).join("secrets")).await;
        String::new()
    };
    let trial =
        hub.store.trial(&run.id).await?.ok_or_else(|| HubError::Undefined { kind: "trial", name: run.id.clone() })?;
    tracing::info!(run = %run.id, job = %run.runnable, ?outcome, effects = trial.effects.len(), "trial finished");
    let payload = BuiltinPayload::TrialFinished(TrialFinishedPayload {
        run_id: run.id.clone(),
        job: run.runnable.clone(),
        branch: trial.branch,
        outcome,
        exit_code,
        reason,
        logs_tail: failures::tail(&logs, 40),
        effects: trial.effects,
        withheld_secrets: trial.withheld,
    });
    hub.emit(crate::hub::builtin(payload, Source::Hub)?.to(trial.instance)).await?;
    Ok(())
}
