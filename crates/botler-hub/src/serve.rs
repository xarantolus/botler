//! Running the hub: background loops plus the HTTP server.

use std::sync::Arc;
use std::time::Duration;

use crate::hub::{Hub, Result};
use crate::{agents, api, images, router, scheduler, supervisor};

async fn route_loop(hub: Arc<Hub>) {
    loop {
        if let Err(e) = router::route_pending(&hub).await {
            tracing::error!(error = %e, "routing failed");
        }
        tokio::select! {
            _ = hub.route_notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
    }
}

/// Serve until `shutdown` resolves. Startup is recovery: everything is reconciled from the store,
/// the monorepo and the container runtime before work starts.
pub async fn serve(hub: Arc<Hub>, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    crate::assets::unpack(&hub.paths)?;
    agents::reconcile_on_boot(&hub).await?;
    crate::transactions::resume_on_boot(&hub).await?;
    {
        // Building the base image can take minutes on a fresh host; agents wait for it on wake.
        let hub = hub.clone();
        tokio::spawn(async move {
            if let Err(e) = images::ensure_base(&hub).await {
                tracing::error!(error = %e, "could not build the agent base image");
            }
        });
    }
    tokio::spawn(route_loop(hub.clone()));
    tokio::spawn(scheduler::run(hub.clone()));
    tokio::spawn(supervisor::run(hub.clone()));
    tokio::spawn(agents::run(hub.clone()));

    let listener = tokio::net::TcpListener::bind(hub.config.listen).await?;
    tracing::info!(addr = %hub.config.listen, commit = %hub.applied().await.commit, "botler hub listening");
    let app = api::router(hub.clone());
    let stop_hub = hub.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            tracing::info!("shutting down: stopping agents");
            agents::stop_all(&stop_hub).await;
        })
        .await?;
    Ok(())
}
