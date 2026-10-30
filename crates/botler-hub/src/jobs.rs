//! Oneshot jobs: deterministic containers started by triggers, timers, agents or replays.

use std::collections::BTreeMap;

use botler_core::defs::{Lifetime, Overlap};
use botler_core::payloads::{BuiltinPayload, RunnableFinishedPayload};
use botler_core::{Event, Source};
use botler_runtime::{ContainerSpec, Mount, NetworkMode};
use botler_store::{NewRun, Run, RunKind, RunStatus};
use chrono::{Duration, Utc};
use serde_json::json;

use crate::auth::Principal;
use crate::failures::{self, Failure};
use crate::hub::{Hub, HubError, Result};
use crate::{images, plumbing};

pub const ROLE_LABEL: &str = "botler.role";
pub const RUN_LABEL: &str = "botler.run";
pub const RUNNABLE_LABEL: &str = "botler.runnable";

pub fn container_name(run_id: &str) -> String {
    format!("botler-run-{run_id}")
}

/// Start a run of oneshot job `job`. Returns the run id, or `None` if skipped by its overlap
/// policy.
pub async fn start(hub: &Hub, job: &str, trigger: Option<&str>, event: Option<&Event>) -> Result<Option<String>> {
    let applied = hub.applied().await;
    let def = applied.defs.jobs.get(job).ok_or_else(|| HubError::Undefined { kind: "job", name: job.into() })?;
    if def.spec.lifetime != Lifetime::Oneshot {
        return Err(HubError::Forbidden(format!("{job} is a service; services are not triggered")));
    }
    let overlap = trigger.and_then(|t| applied.defs.triggers.get(t)).map_or(Overlap::Allow, |t| t.spec.overlap);
    let busy = hub.store.runs_for(job, 20).await?.iter().any(|r| !r.status.is_terminal());
    if busy && overlap == Overlap::Skip {
        tracing::info!(job, trigger, "previous run still active; skipped (overlap = skip)");
        return Ok(None);
    }
    let run = hub
        .store
        .create_run(&NewRun {
            runnable: job.into(),
            kind: RunKind::Oneshot,
            trigger_id: trigger.map(String::from),
            event_id: event.map(|e| e.id.to_string()),
            commit_id: Some(applied.commit.clone()),
        })
        .await?;
    if busy && overlap == Overlap::Queue {
        tracing::info!(job, run = %run.id, "queued behind the active run");
        return Ok(Some(run.id));
    }
    launch(hub, &run, event).await?;
    Ok(Some(run.id))
}

/// Start the container for a queued run. Start failures finish the run and are reported.
pub async fn launch(hub: &Hub, run: &Run, event: Option<&Event>) -> Result<()> {
    match try_launch(hub, run, event).await {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(run = %run.id, job = %run.runnable, error = %e, "could not start job");
            if hub.store.finish_run(&run.id, RunStatus::Failed, None, Some(&e.to_string())).await? {
                failures::report(hub, failure(run, "start", None, e.to_string())).await?;
            }
            Ok(())
        }
    }
}

async fn try_launch(hub: &Hub, run: &Run, event: Option<&Event>) -> Result<()> {
    let applied = hub.applied().await;
    let def = applied
        .defs
        .jobs
        .get(&run.runnable)
        .ok_or_else(|| HubError::Undefined { kind: "job", name: run.runnable.clone() })?;
    let commit = run.commit_id.clone().unwrap_or_else(|| applied.commit.clone());
    let image = images::ensure_job_image(hub, &run.runnable, &commit).await?;
    let timeout = def.spec.timeout.unwrap_or(applied.defs.instance.defaults.job_timeout);
    let run_dir = hub.paths.root.join("runs").join(&run.id);
    let p = plumbing::plumbing(
        hub,
        &Principal::Run { run_id: run.id.clone() },
        timeout.as_chrono() + Duration::minutes(5),
        &def.spec.secrets,
        &[],
        run_dir.join("secrets"),
    )
    .await?;
    let mut env = p.env;
    env.insert("BOTLER_RUN_ID".into(), run.id.clone());
    env.insert("BOTLER_RUNNABLE".into(), run.runnable.clone());
    let mut mounts = p.mounts;
    tokio::fs::create_dir_all(&run_dir).await?;
    let event_file = run_dir.join("event.json");
    let event_json = serde_json::to_vec_pretty(&event.map_or(json!(null), |e| json!(e)))
        .map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    tokio::fs::write(&event_file, event_json).await?;
    mounts.push(Mount::Bind { src: event_file, dst: "/botler/event.json".into(), read_only: true });
    env.insert("BOTLER_EVENT_FILE".into(), "/botler/event.json".into());
    if def.spec.volume {
        mounts.push(Mount::Volume { name: format!("botler-job-{}", run.runnable), dst: "/data".into() });
    }
    let spec = ContainerSpec {
        name: container_name(&run.id),
        image,
        command: def.spec.command.clone(),
        env,
        mounts,
        labels: BTreeMap::from([
            (ROLE_LABEL.into(), "job".into()),
            (RUN_LABEL.into(), run.id.clone()),
            (RUNNABLE_LABEL.into(), run.runnable.clone()),
        ]),
        network: match def.spec.network {
            botler_core::defs::Network::Default => NetworkMode::Default,
            botler_core::defs::Network::None => NetworkMode::None,
        },
        ..Default::default()
    };
    let cid = hub.containers.run_detached(&spec).await?;
    hub.store.start_run(&run.id, &cid).await?;
    tracing::info!(run = %run.id, job = %run.runnable, "job started");
    Ok(())
}

/// Time out every running oneshot run past its deadline. Called periodically; stateless, so
/// it needs no re-arming after a restart.
pub async fn sweep_timeouts(hub: &Hub, now: chrono::DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    for run in hub.store.active_runs().await? {
        if run.kind != RunKind::Oneshot || run.status != RunStatus::Running {
            continue;
        }
        let Some(started) = run.started_at else { continue };
        let timeout = applied
            .defs
            .jobs
            .get(&run.runnable)
            .and_then(|j| j.spec.timeout)
            .unwrap_or(applied.defs.instance.defaults.job_timeout);
        if now - started > timeout.as_chrono() {
            on_timeout(hub, &run.id).await?;
        }
    }
    Ok(())
}

async fn on_timeout(hub: &Hub, run_id: &str) -> Result<()> {
    if !hub.store.finish_run(run_id, RunStatus::Timeout, None, Some("timed out")).await? {
        return Ok(()); // finished normally first
    }
    let run = hub.store.run(run_id).await?;
    let name = container_name(run_id);
    let logs = collect_logs(hub, &name, run_id).await;
    match hub.containers.stop(&name, 10).await {
        Ok(()) | Err(botler_runtime::ContainerError::NotFound(_)) => {}
        Err(e) => tracing::error!(container = %name, error = %e, "could not stop timed-out job"),
    }
    finalize_container(hub, &run).await;
    failures::report(hub, failure(&run, "timeout", None, failures::tail(&logs, 40))).await?;
    start_queued(hub, &run.runnable).await
}

/// A job container exited (from the runtime event stream or boot reconciliation).
pub async fn on_exit(hub: &Hub, run_id: &str, exit_code: Option<i64>, oom: bool) -> Result<()> {
    let run = hub.store.run(run_id).await?;
    let name = container_name(run_id);
    let logs = collect_logs(hub, &name, run_id).await;
    let ok = exit_code == Some(0) && !oom;
    let status = if ok { RunStatus::Succeeded } else { RunStatus::Failed };
    let reason = (!ok).then(|| if oom { "out of memory".to_string() } else { format!("exit code {exit_code:?}") });
    let transitioned = hub.store.finish_run(run_id, status, exit_code, reason.as_deref()).await?;
    finalize_container(hub, &run).await;
    if !transitioned {
        return Ok(()); // timeout already handled it
    }
    if ok {
        let output = logs.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").to_string();
        let payload = BuiltinPayload::RunnableFinished(RunnableFinishedPayload {
            runnable: run.runnable.clone(),
            run_id: run.id.clone(),
            output,
        });
        let ev = crate::hub::builtin(payload, Source::Runnable(run.runnable.clone()))?;
        crate::router::dispatch(hub, ev, vec![]).await?;
    } else {
        let kind = if oom { "oom" } else { "exit" };
        failures::report(hub, failure(&run, kind, exit_code, failures::tail(&logs, 40))).await?;
    }
    start_queued(hub, &run.runnable).await
}

fn failure(run: &Run, kind: &'static str, exit_code: Option<i64>, logs_tail: String) -> Failure {
    Failure {
        runnable: run.runnable.clone(),
        kind,
        exit_code,
        run_id: Some(run.id.clone()),
        logs_tail,
        commit: run.commit_id.clone(),
        event_id: run.event_id.clone(),
    }
}

async fn start_queued(hub: &Hub, job: &str) -> Result<()> {
    if let Some(next) = hub.store.queued_runs(job).await?.into_iter().next() {
        let event = match &next.event_id {
            Some(id) => Some(hub.store.event(id).await?),
            None => None,
        };
        launch(hub, &next, event.as_ref()).await?;
    }
    Ok(())
}

/// Save the container's output to the run log and return it. Missing logs are not fatal.
async fn collect_logs(hub: &Hub, container: &str, run_id: &str) -> String {
    let logs = match hub.containers.logs(container, 5000).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(container, error = %e, "could not read container logs");
            format!("<logs unavailable: {e}>")
        }
    };
    let path = hub.paths.run_log(run_id);
    if let Err(e) = tokio::fs::write(&path, &logs).await {
        tracing::error!(path = %path.display(), error = %e, "could not save run log");
    }
    logs
}

async fn finalize_container(hub: &Hub, run: &Run) {
    let name = container_name(&run.id);
    if let Err(e) = hub.containers.remove(&name).await {
        tracing::error!(container = %name, error = %e, "could not remove finished container");
    }
    plumbing::cleanup_secrets(&hub.paths.root.join("runs").join(&run.id).join("secrets")).await;
}
