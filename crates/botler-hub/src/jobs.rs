//! Job runs: oneshot containers started by triggers, timers, agents or replays, and the shared
//! launch and exit handling that services (`services.rs`) use too.

use botler_core::defs::{Environment, JobDef, Lifetime, Overlap};
use botler_core::payloads::{BuiltinPayload, RunnableFinishedPayload};
use botler_core::{Event, Source};
use botler_runtime::Mount;
use botler_store::{NewRun, Run, RunKind, RunStatus};
use chrono::{Duration, Utc};
use serde_json::json;

use crate::environment::{self, Runnable};
use crate::failures::{self, Failure};
use crate::hub::{Hub, HubError, Result};
use crate::{images, services};

pub const ROLE_LABEL: &str = "botler.role";
pub const RUN_LABEL: &str = "botler.run";
pub const RUNNABLE_LABEL: &str = "botler.runnable";

pub fn container_name(hub: &Hub, run_id: &str) -> String {
    format!("botler-{}-run-{run_id}", hub.id)
}

/// The container of `run`: one per run for oneshots, one stable name per service.
pub fn run_container(hub: &Hub, run: &Run) -> String {
    match run.kind {
        RunKind::Service => services::container_name(hub, &run.runnable),
        _ => container_name(hub, &run.id),
    }
}

/// Start a run of oneshot job `job`. Returns the run id, or `None` if skipped by its overlap
/// policy.
pub async fn start(hub: &Hub, job: &str, trigger: Option<&str>, event: Option<&Event>) -> Result<Option<String>> {
    let applied = hub.applied().await;
    let overlap = trigger.and_then(|t| applied.defs.triggers.get(t)).map_or(Overlap::Allow, |t| t.spec.overlap);
    let Some(admitted) = admit(hub, job, trigger, event, overlap).await? else { return Ok(None) };
    if admitted.launch {
        launch(hub, &admitted.run, event).await?;
    }
    Ok(Some(admitted.run.id))
}

/// A run [`admit`] created.
pub struct Admitted {
    pub run: Run,
    /// Start it now; otherwise it is queued behind the active run.
    pub launch: bool,
}

/// Create a run of oneshot job `job` as `overlap` allows, without starting its container;
/// `None` if skipped.
pub async fn admit(
    hub: &Hub,
    job: &str,
    trigger: Option<&str>,
    event: Option<&Event>,
    overlap: Overlap,
) -> Result<Option<Admitted>> {
    let applied = hub.applied().await;
    let def = applied.defs.jobs.get(job).ok_or_else(|| HubError::Undefined { kind: "job", name: job.into() })?;
    if def.spec.lifetime != Lifetime::Oneshot {
        return Err(HubError::Forbidden(format!("{job} is a service; services are not triggered")));
    }
    if !def.spec.enabled {
        tracing::info!(job, trigger, "disabled (enabled = false); not started");
        return Ok(None);
    }
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
    let queued = busy && overlap == Overlap::Queue;
    if queued {
        tracing::info!(job, run = %run.id, "queued behind the active run");
    }
    Ok(Some(Admitted { run, launch: !queued }))
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

/// Where a runnable finds its settings.
pub const SETTINGS_FILE: &str = "/botler/settings.json";

/// Start the container for `run` of any kind: image, secrets, SDK, token and event file, plus a
/// service's network, stable name and published ports.
pub async fn try_launch(hub: &Hub, run: &Run, event: Option<&Event>) -> Result<()> {
    let applied = hub.applied().await;
    let def = applied
        .defs
        .jobs
        .get(&run.runnable)
        .ok_or_else(|| HubError::Undefined { kind: "job", name: run.runnable.clone() })?;
    let timeout = def.spec.timeout.unwrap_or(applied.defs.instance.defaults.job_timeout).as_chrono();
    launch_job(hub, run, def, &def.spec.env, timeout, event).await
}

/// Start the container for `run` of `job` (from any commit) in `env`. Its token lives `timeout`
/// plus a margin, or as long as the run for a service.
pub(crate) async fn launch_job(
    hub: &Hub,
    run: &Run,
    def: &JobDef,
    env: &Environment,
    timeout: Duration,
    event: Option<&Event>,
) -> Result<()> {
    let commit = match &run.commit_id {
        Some(c) => c.clone(),
        None => hub.applied().await.commit.clone(),
    };
    let image = images::ensure_job_image(hub, &run.runnable, &commit).await?;
    let service = run.kind == RunKind::Service;
    // A service's token lives as long as its run: `Caller::resolve` refuses it once the run ends.
    let ttl = if service { Duration::days(3650) } else { timeout + Duration::minutes(5) };
    let what = Runnable::Run { run, ttl };
    let mut spec = environment::container_spec(hub, env, what, image, def.spec.command.clone(), &[]).await?;
    let run_dir = hub.paths.root.join("runs").join(&run.id);
    tokio::fs::create_dir_all(&run_dir).await?;
    let event_file = run_dir.join("event.json");
    let event_json = serde_json::to_vec_pretty(&event.map_or(json!(null), |e| json!(e)))
        .map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    tokio::fs::write(&event_file, event_json).await?;
    spec.mounts.push(Mount::Bind { src: event_file, dst: "/botler/event.json".into(), read_only: true });
    spec.env.insert("BOTLER_EVENT_FILE".into(), "/botler/event.json".into());
    if let Some(settings) = &def.settings {
        // `botler.settings`: the values from settings.toml; the model fills in its defaults.
        let file = run_dir.join("settings.json");
        let json = serde_json::to_vec_pretty(&settings.values).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
        tokio::fs::write(&file, json).await?;
        spec.mounts.push(Mount::Bind { src: file, dst: SETTINGS_FILE.into(), read_only: true });
    }
    if run.kind == RunKind::Action {
        // Exactly the arguments the user approved.
        let p = crate::actions::proposal_of(hub, run).await?;
        let args = serde_json::to_vec_pretty(&p.args).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
        let args_file = run_dir.join("args.json");
        tokio::fs::write(&args_file, args).await?;
        spec.mounts.push(Mount::Bind { src: args_file, dst: crate::actions::ARGS_FILE.into(), read_only: true });
        spec.env.insert("BOTLER_ACTION_ARGS".into(), crate::actions::ARGS_FILE.into());
    }
    // The name is stable: clear the leftover of a run the hub could not finalize.
    if service
        && let Some(c) = hub.containers.inspect(&spec.name).await?
        && !c.running
    {
        hub.containers.remove(&spec.name).await?;
    }
    let cid = hub.containers.run_detached(&spec).await.map_err(|e| match &e {
        botler_runtime::ContainerError::Command { stderr, .. }
            if !spec.ports.is_empty() && stderr.contains("address already in use") =>
        {
            let ports: Vec<String> = spec.ports.iter().map(|p| format!("{}:{}", p.bind, p.host_port)).collect();
            HubError::PortInUse { ports: ports.join(", "), source: e }
        }
        _ => e.into(),
    })?;
    hub.store.start_run(&run.id, &cid).await?;
    tracing::info!(run = %run.id, job = %run.runnable, "job started");
    Ok(())
}

/// Time out every running oneshot run past its deadline. Called periodically; stateless, so
/// it needs no re-arming after a restart.
pub async fn sweep_timeouts(hub: &Hub, now: chrono::DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    for run in hub.store.active_runs().await? {
        if run.kind == RunKind::Trial {
            crate::trials::sweep(hub, &run, now).await?;
            continue;
        }
        if !matches!(run.kind, RunKind::Oneshot | RunKind::Action) || run.status != RunStatus::Running {
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
    let name = run_container(hub, &run);
    let logs = collect_logs(hub, &name, run_id).await;
    match hub.containers.stop(&name, 10).await {
        Ok(()) | Err(botler_runtime::ContainerError::NotFound(_)) => {}
        Err(e) => tracing::error!(container = %name, error = %e, "could not stop timed-out job"),
    }
    finalize_container(hub, &run).await;
    if run.kind == RunKind::Action {
        return crate::actions::finished(hub, &run, false, &logs, Some("timed out")).await;
    }
    failures::report(hub, failure(&run, "timeout", None, failures::tail(&logs, 40))).await?;
    start_queued(hub, &run.runnable).await
}

/// A job container exited (from the runtime event stream or boot reconciliation).
pub async fn on_exit(hub: &Hub, run_id: &str, exit_code: Option<i64>, oom: bool) -> Result<()> {
    let run = hub.store.run(run_id).await?;
    if run.kind == RunKind::Trial {
        return crate::trials::on_exit(hub, &run, exit_code, oom).await;
    }
    let name = run_container(hub, &run);
    let logs = collect_logs(hub, &name, run_id).await;
    if run.kind == RunKind::Service {
        // A service never ends by itself on purpose: every exit is a failure. Stops the hub makes
        // (pause, change, removal) finish the run first, so they are not reported here.
        let reason = if oom { "out of memory".to_string() } else { format!("service exited (code {exit_code:?})") };
        let transitioned = hub.store.finish_run(run_id, RunStatus::Failed, exit_code, Some(&reason)).await?;
        finalize_container(hub, &run).await;
        if transitioned {
            let kind = if oom { "oom" } else { "exit" };
            failures::report(hub, failure(&run, kind, exit_code, failures::tail(&logs, 40))).await?;
        }
        hub.reconcile_notify.notify_one();
        return Ok(());
    }
    let ok = exit_code == Some(0) && !oom;
    let status = if ok { RunStatus::Succeeded } else { RunStatus::Failed };
    let reason = (!ok).then(|| if oom { "out of memory".to_string() } else { format!("exit code {exit_code:?}") });
    let transitioned = hub.store.finish_run(run_id, status, exit_code, reason.as_deref()).await?;
    finalize_container(hub, &run).await;
    if !transitioned {
        return Ok(()); // timeout already handled it
    }
    if run.kind == RunKind::Action {
        // The invoking chat hears the result either way; a failed action is no incident.
        return crate::actions::finished(hub, &run, ok, &logs, reason.as_deref()).await;
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

pub(crate) fn failure(run: &Run, kind: &'static str, exit_code: Option<i64>, logs_tail: String) -> Failure {
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
pub(crate) async fn collect_logs(hub: &Hub, container: &str, run_id: &str) -> String {
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

pub(crate) async fn finalize_container(hub: &Hub, run: &Run) {
    let name = run_container(hub, run);
    if let Err(e) = hub.containers.remove(&name).await {
        tracing::error!(container = %name, error = %e, "could not remove finished container");
    }
    environment::cleanup_secrets(&hub.paths.root.join("runs").join(&run.id).join("secrets")).await;
}
