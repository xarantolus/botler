//! Watches containers: exits from the runtime's event stream, timeouts, and reconciliation of
//! runs that were active when the hub stopped.

use std::sync::Arc;
use std::time::Duration;

use botler_store::{RunKind, RunStatus};
use chrono::Utc;

use crate::hub::{Hub, Result};
use crate::jobs;

/// After a restart: every run the store thinks is active is checked against the runtime.
pub async fn reconcile_runs(hub: &Hub) -> Result<()> {
    for run in hub.store.active_runs().await? {
        match run.status {
            RunStatus::Queued => {} // started when its predecessor finishes, or swept below
            // Services are checked by their own reconcile (`services.rs`).
            RunStatus::Running if run.kind != RunKind::Service => {
                let name = jobs::container_name(hub, &run.id);
                match hub.containers.inspect(&name).await? {
                    Some(c) if c.running => tracing::info!(run = %run.id, "re-adopted running job"),
                    Some(c) => jobs::on_exit(hub, &run.id, c.exit_code, c.oom_killed).await?,
                    None => {
                        tracing::warn!(run = %run.id, "job container vanished while the hub was down");
                        jobs::on_exit(hub, &run.id, None, false).await?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Handle one container event.
pub async fn on_event(hub: &Hub, ev: &botler_runtime::ContainerEvent) -> Result<()> {
    if ev.status != "died" {
        return Ok(());
    }
    // Containers of other hubs on the same runtime carry another id.
    let Some(rest) = ev.name.strip_prefix(&format!("botler-{}-", hub.id)) else { return Ok(()) };
    if rest.starts_with("svc-") {
        // The service reconcile inspects the container and handles the exit.
        hub.reconcile_notify.notify_one();
        return Ok(());
    }
    if let Some(run_id) = rest.strip_prefix("run-") {
        let oom = match hub.containers.inspect(&ev.name).await? {
            Some(c) => c.oom_killed,
            None => false,
        };
        return jobs::on_exit(hub, run_id, ev.exit_code, oom).await;
    }
    Ok(())
}

/// Run forever: consume runtime events (restarting the stream if it ends) and sweep timeouts.
pub async fn run(hub: Arc<Hub>) {
    let sweeper = {
        let hub = hub.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = jobs::sweep_timeouts(&hub, Utc::now()).await {
                    tracing::error!(error = %e, "timeout sweep failed");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
    };
    loop {
        match hub.containers.events().await {
            Ok(mut rx) => {
                // Anything that died while the stream was down is caught by reconciliation.
                if let Err(e) = reconcile_runs(&hub).await {
                    tracing::error!(error = %e, "run reconciliation failed");
                }
                while let Some(ev) = rx.recv().await {
                    if let Err(e) = on_event(&hub, &ev).await {
                        tracing::error!(container = %ev.name, error = %e, "handling container event failed");
                    }
                }
                tracing::warn!("container event stream ended; restarting");
            }
            Err(e) => tracing::error!(error = %e, "cannot open container event stream"),
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        if sweeper.is_finished() {
            tracing::error!("timeout sweeper stopped unexpectedly");
        }
    }
}
