//! Services: long-running job containers the hub keeps running.
//!
//! Crash-only: [`reconcile`] holds no state but a count of failed health checks in a row, which a
//! restart may forget. It compares the applied definitions with the runs table and the runtime,
//! and the same pass runs at boot, after every apply, on resume, after a service exits and on a
//! periodic tick. Backoff and crash loops are computed from the runs table;
//! an open `crashloop` incident is the failed state, and resolving it lets the next pass restart
//! the service.

use std::sync::Arc;
use std::time::Duration;

use botler_core::defs::JobDef;
use botler_store::{NewRun, Run, RunKind, RunStatus};
use chrono::{DateTime, Utc};

use crate::failures::{self, Failure};
use crate::hub::{Hub, Result};
use crate::images::{self, Recipe};
use crate::jobs;
use crate::wire::{PublishedPort, ServiceState, ServiceStatus};

/// The stable container name of a service. On the services network it is also reachable by its
/// plain name (a network alias).
pub fn container_name(hub: &Hub, service: &str) -> String {
    format!("botler-{}-svc-{service}", hub.id)
}

/// Converge the running services with the applied definitions, now.
pub async fn reconcile(hub: &Arc<Hub>) -> Result<()> {
    reconcile_at(hub, Utc::now()).await
}

/// [`reconcile`] at `now` (tests move the clock).
pub async fn reconcile_at(hub: &Arc<Hub>, now: DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    let paused = hub.paused().await?;
    let runs: Vec<Run> = hub.store.active_runs().await?.into_iter().filter(|r| r.kind == RunKind::Service).collect();
    // Failed checks of runs that ended are forgotten.
    hub.failed_checks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|id, _| runs.iter().any(|r| r.id == *id));
    for run in runs {
        let def = applied.defs.services().find(|s| s.name == run.runnable);
        if run.status == RunStatus::Queued {
            // Launches happen inside this pass, so a queued service run was cut off by a restart.
            end(hub, &run, RunStatus::Cancelled, "hub restarted before it started").await?;
            continue;
        }
        if !alive(hub, &run).await? {
            continue;
        }
        let Some(def) = def.filter(|_| !paused) else {
            stop(hub, &run, if paused { "botler is paused" } else { "removed from the definitions" }).await?;
            continue;
        };
        // Only a change of the image's inputs restarts a service, never an unrelated merge.
        if let Some(commit) = run.commit_id.as_deref().filter(|c| *c != applied.commit) {
            let recipe = Recipe::job(&run.runnable);
            let next = images::tag(hub, &recipe, &applied.commit).await?;
            if images::tag(hub, &recipe, commit).await? != next {
                if hub.containers.image_id(&next).await?.is_none() {
                    // Build the new image while this one keeps serving; the swap follows it.
                    prebuild(hub, &run, &applied.commit, next).await;
                } else {
                    stop(hub, &run, "its definition or code changed").await?;
                    continue;
                }
            }
        }
        check_heartbeat(hub, def, &run, now).await?;
    }
    if paused {
        return Ok(());
    }
    for def in applied.defs.services() {
        ensure_running(hub, def, &applied.commit, now).await?;
    }
    Ok(())
}

/// Whether the run's container is still running; if not, its exit is handled here (the event
/// stream is only the fast path).
async fn alive(hub: &Hub, run: &Run) -> Result<bool> {
    let c = hub.containers.inspect(&container_name(hub, &run.runnable)).await?;
    match c.filter(|c| c.labels.get(jobs::RUN_LABEL) == Some(&run.id)) {
        Some(c) if c.running => Ok(true),
        Some(c) => {
            jobs::on_exit(hub, &run.id, c.exit_code, c.oom_killed).await?;
            Ok(false)
        }
        None => {
            tracing::warn!(run = %run.id, service = %run.runnable, "service container vanished");
            jobs::on_exit(hub, &run.id, None, false).await?;
            Ok(false)
        }
    }
}

async fn check_heartbeat(hub: &Hub, def: &JobDef, run: &Run, now: DateTime<Utc>) -> Result<()> {
    let (Some(h), Some(started)) = (&def.spec.health, run.started_at) else { return Ok(()) };
    // Nor while the hub was down: a service is due one from the hub's start at the earliest.
    let last = (started + h.grace.as_chrono()).max(run.heartbeat_at.unwrap_or(started)).max(hub.started);
    let overdue = now > last + h.heartbeat.as_chrono();
    if h.command.is_empty() {
        if overdue {
            unhealthy(hub, run, &format!("no health heartbeat since {last} (expected every {})", h.heartbeat)).await?;
        }
        return Ok(());
    }
    // `command`: the hub checks, a few times per heartbeat interval, and after a failure again
    // every RETRY_GAP: one slow exec on a busy host is not an outage.
    let failed = || hub.failed_checks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = failed().get(&run.id).cloned();
    let due = match &previous {
        Some(f) => now >= f.at + RETRY_GAP,
        None => now >= run.heartbeat_at.unwrap_or(started) + h.heartbeat.as_chrono() / 3,
    };
    let failures = if due {
        let name = container_name(hub, &run.runnable);
        let reason = match tokio::time::timeout(PROBE_TIMEOUT, hub.containers.exec(&name, &h.command)).await {
            Ok(Ok(_)) => {
                failed().remove(&run.id);
                hub.store.heartbeat(&run.id, now).await?;
                return Ok(());
            }
            Ok(Err(e)) => format!("its health check `{}` failed: {e}", h.command.join(" ")),
            Err(_) => format!("its health check `{}` took over {PROBE_TIMEOUT:?}", h.command.join(" ")),
        };
        if now > started + h.grace.as_chrono() {
            tracing::warn!(service = %run.runnable, %reason, "service health check failed");
        }
        let f = FailedChecks { count: previous.map_or(0, |p| p.count) + 1, at: now, reason };
        failed().insert(run.id.clone(), f.clone());
        Some(f)
    } else {
        previous
    };
    // Unhealthy once the heartbeat is overdue and the last few checks in a row failed.
    if let Some(f) = failures.filter(|f| overdue && f.count >= CHECKS_BEFORE_UNHEALTHY) {
        failed().remove(&run.id);
        let reason = format!("{} ({} checks in a row failed; no heartbeat since {last})", f.reason, f.count);
        unhealthy(hub, run, &reason).await?;
    }
    Ok(())
}

/// A service run's failed `[health] command` checks in a row.
#[derive(Clone, Debug)]
pub struct FailedChecks {
    count: u32,
    /// The last one.
    at: DateTime<Utc>,
    reason: String,
}

/// A `[health] command` that takes longer than this fails.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// A failed `[health] command` is retried this long after it.
const RETRY_GAP: chrono::TimeDelta = chrono::TimeDelta::seconds(30);
/// Failed `[health] command` checks in a row (and an overdue heartbeat) that make a service
/// unhealthy.
const CHECKS_BEFORE_UNHEALTHY: u32 = 3;

/// Start `def` unless it runs, is backing off or is crash-looping.
async fn ensure_running(hub: &Hub, def: &JobDef, commit: &str, now: DateTime<Utc>) -> Result<()> {
    let name = &def.name;
    let last = hub.store.runs_for(name, 1).await?.into_iter().next();
    if last.as_ref().is_some_and(|r| !r.status.is_terminal()) {
        return Ok(());
    }
    let restart = &def.spec.restart;
    let mut since = now - restart.crashloop_window.as_chrono();
    if let Some(i) = hub.store.incidents_for(name, 1).await?.into_iter().next().filter(|i| i.kind == "crashloop") {
        if i.status == botler_store::IncidentStatus::Open {
            return Ok(()); // failed until the incident is resolved
        }
        since = since.max(i.last_at); // resolved: earlier crashes no longer count
    }
    let n = hub.store.failures_since(name, since).await?;
    if n >= i64::from(restart.crashloop_count) {
        let f = Failure {
            runnable: name.clone(),
            kind: "crashloop",
            exit_code: last.as_ref().and_then(|r| r.exit_code),
            run_id: last.as_ref().map(|r| r.id.clone()),
            logs_tail: format!(
                "{n} failures within {}; not restarted until the incident is resolved.\nLast: {}",
                restart.crashloop_window,
                last.as_ref().and_then(|r| r.reason.as_deref()).unwrap_or("-")
            ),
            commit: Some(commit.to_string()),
            event_id: None,
        };
        return failures::report(hub, f).await;
    }
    if n > 0
        && let Some(finished) = last.as_ref().and_then(|r| r.finished_at)
    {
        let backoff = chrono::Duration::seconds(1i64 << n.min(30)).min(restart.backoff_max.as_chrono());
        if now < finished + backoff {
            return Ok(());
        }
    }
    let run = hub
        .store
        .create_run(&NewRun {
            runnable: name.clone(),
            kind: RunKind::Service,
            trigger_id: None,
            event_id: None,
            commit_id: Some(commit.to_string()),
        })
        .await?;
    tracing::info!(service = %name, run = %run.id, restarts = n, "starting service");
    jobs::launch(hub, &run, None).await
}

/// End a running service run with `status`: stop and remove its container, keeping its log.
/// Returns the log tail, or `None` if something else ended the run first.
async fn end(hub: &Hub, run: &Run, status: RunStatus, reason: &str) -> Result<Option<String>> {
    if !hub.store.finish_run(&run.id, status, None, Some(reason)).await? {
        return Ok(None);
    }
    let name = container_name(hub, &run.runnable);
    match hub.containers.stop(&name, 10).await {
        Ok(()) | Err(botler_runtime::ContainerError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let logs = jobs::collect_logs(hub, &name, &run.id).await;
    jobs::finalize_container(hub, run).await;
    Ok(Some(logs))
}

/// Stop a service on purpose (pause, change, removal). Not a failure.
pub async fn stop(hub: &Hub, run: &Run, why: &str) -> Result<()> {
    if end(hub, run, RunStatus::Cancelled, why).await?.is_some() {
        tracing::info!(service = %run.runnable, run = %run.id, why, "service stopped");
    }
    Ok(())
}

/// The service is unhealthy (missed heartbeat or its own `health` report): stop it and report.
/// The next reconcile restarts it with backoff.
pub async fn unhealthy(hub: &Hub, run: &Run, reason: &str) -> Result<()> {
    let Some(logs) = end(hub, run, RunStatus::Failed, &format!("unhealthy: {reason}")).await? else { return Ok(()) };
    let logs_tail = format!("{reason}\n{}", failures::tail(&logs, 40));
    failures::report(hub, jobs::failure(run, "unhealthy", None, logs_tail)).await?;
    hub.reconcile_notify.notify_one();
    Ok(())
}

/// Stop every running service (the kill switch).
pub async fn stop_all(hub: &Hub) -> Result<()> {
    let runs: Vec<Run> = hub.store.active_runs().await?.into_iter().filter(|r| r.kind == RunKind::Service).collect();
    // Failed checks of runs that ended are forgotten.
    hub.failed_checks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|id, _| runs.iter().any(|r| r.id == *id));
    for run in runs {
        stop(hub, &run, "botler is paused").await?;
    }
    Ok(())
}

/// Every defined service, its state and its addresses (`botler status`, the web app).
pub async fn status(hub: &Hub) -> Result<Vec<ServiceStatus>> {
    let applied = hub.applied().await;
    let paused = hub.paused().await?;
    let host = tokio::fs::read_to_string("/proc/sys/kernel/hostname").await.map(|h| h.trim().to_string());
    let mut out = Vec::new();
    for def in applied.defs.services() {
        let last = hub.store.runs_for(&def.name, 1).await?.into_iter().next();
        let failed = hub
            .store
            .incidents_for(&def.name, 1)
            .await?
            .first()
            .is_some_and(|i| i.kind == "crashloop" && i.status == botler_store::IncidentStatus::Open);
        let state = match last.map(|r| r.status) {
            _ if paused => ServiceState::Paused,
            _ if failed => ServiceState::Failed,
            Some(RunStatus::Running) if updating(hub, &def.name).await => ServiceState::Updating,
            Some(RunStatus::Running) => ServiceState::Running,
            Some(RunStatus::Failed | RunStatus::Timeout) => ServiceState::Backoff,
            _ => ServiceState::Starting,
        };
        let published = def
            .spec
            .env
            .publish
            .iter()
            .map(|p| {
                let addr = match (&host, p.bind.as_str()) {
                    (Ok(h), "0.0.0.0" | "::") if !h.is_empty() => h.clone(),
                    _ => p.bind.clone(),
                };
                PublishedPort {
                    bind: p.bind.clone(),
                    host_port: p.host_port,
                    port: p.port,
                    url: format!("http://{addr}:{}", p.host_port),
                }
            })
            .collect();
        out.push(ServiceStatus {
            name: def.name.clone(),
            state,
            internal_url: def.spec.env.port.map(|p| format!("http://{}:{p}", def.name)),
            published,
        });
    }
    Ok(out)
}

/// The image tag being built for a changed service, by `<hub> <service>`, and whether its build
/// failed (then the old one keeps serving and it is not retried for this tag).
static PREBUILDS: std::sync::LazyLock<tokio::sync::Mutex<std::collections::HashMap<String, (String, bool)>>> =
    std::sync::LazyLock::new(Default::default);

/// The service runs an older image while the one for the applied code is built; it swaps to it
/// by itself once built.
pub async fn updating(hub: &Hub, service: &str) -> bool {
    PREBUILDS.lock().await.get(&format!("{} {service}", hub.id)).is_some_and(|(_, failed)| !failed)
}

/// Build the image of `run`'s service at `commit` in the background, then reconcile again. A
/// failed build is reported and not retried for this tag: the old one keeps serving.
async fn prebuild(hub: &Arc<Hub>, run: &Run, commit: &str, tag: String) {
    let key = format!("{} {}", hub.id, run.runnable);
    {
        let mut builds = PREBUILDS.lock().await;
        if builds.get(&key).is_some_and(|(t, _)| *t == tag) {
            return;
        }
        builds.insert(key.clone(), (tag.clone(), false));
    }
    tracing::info!(service = %run.runnable, %tag, "building the changed service's image; the running one serves until then");
    let (hub, run, commit) = (hub.clone(), run.clone(), commit.to_string());
    tokio::spawn(async move {
        let built = images::ensure_job_image(&hub, &run.runnable, &commit).await;
        {
            let mut builds = PREBUILDS.lock().await;
            if builds.get(&key).is_some_and(|(t, _)| *t == tag) {
                match built {
                    Ok(_) => builds.remove(&key),
                    Err(_) => builds.insert(key, (tag, true)),
                };
            }
        }
        match built {
            Ok(_) => hub.reconcile_notify.notify_one(),
            Err(e) => {
                let why = format!("its new image did not build, so it keeps running the old one: {e}");
                if let Err(e2) = failures::report(&hub, jobs::failure(&run, "build", None, why)).await {
                    tracing::error!(service = %run.runnable, error = %e2, "could not report the failed build");
                }
            }
        }
    });
}

/// Reconcile forever: when woken (apply, resume, a service exit) and every few seconds.
pub async fn run(hub: Arc<Hub>) {
    loop {
        if let Err(e) = reconcile(&hub).await {
            tracing::error!(error = %e, "service reconciliation failed");
        }
        tokio::select! {
            _ = hub.reconcile_notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
        }
    }
}
