//! Services: long-running job containers the hub keeps running.
//!
//! Crash-only: [`reconcile`] holds no state. It compares the applied definitions with the runs
//! table and the runtime, and the same pass runs at boot, after every apply, on resume, after a
//! service exits and on a periodic tick. Backoff and crash loops are computed from the runs table;
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

/// The stable container name of a service. On the services network it is also reachable by its
/// plain name (a network alias).
pub fn container_name(hub: &Hub, service: &str) -> String {
    format!("botler-{}-svc-{service}", hub.id)
}

/// Converge the running services with the applied definitions, now.
pub async fn reconcile(hub: &Hub) -> Result<()> {
    reconcile_at(hub, Utc::now()).await
}

/// [`reconcile`] at `now` (tests move the clock).
pub async fn reconcile_at(hub: &Hub, now: DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    let paused = hub.paused().await?;
    for run in hub.store.active_runs().await?.into_iter().filter(|r| r.kind == RunKind::Service) {
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
            if images::tag(hub, &recipe, commit).await? != images::tag(hub, &recipe, &applied.commit).await? {
                stop(hub, &run, "its definition or code changed").await?;
                continue;
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
    let last = (started + h.grace.as_chrono()).max(run.heartbeat_at.unwrap_or(started));
    if now > last + h.heartbeat.as_chrono() {
        let reason = format!("no health heartbeat since {last} (expected every {})", h.heartbeat);
        unhealthy(hub, run, &reason).await?;
    }
    Ok(())
}

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
    for run in hub.store.active_runs().await?.into_iter().filter(|r| r.kind == RunKind::Service) {
        stop(hub, &run, "botler is paused").await?;
    }
    Ok(())
}

/// Every defined service, whether it runs, and its addresses (`botler status`, the web app).
pub async fn status(hub: &Hub) -> Result<Vec<crate::wire::ServiceStatus>> {
    let applied = hub.applied().await;
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
                crate::wire::PublishedPort {
                    bind: p.bind.clone(),
                    host_port: p.host_port,
                    port: p.port,
                    url: format!("http://{addr}:{}", p.host_port),
                }
            })
            .collect();
        out.push(crate::wire::ServiceStatus {
            name: def.name.clone(),
            running: last.is_some_and(|r| r.status == RunStatus::Running),
            failed,
            internal_url: def.spec.env.port.map(|p| format!("http://{}:{p}", def.name)),
            published,
        });
    }
    Ok(out)
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
