//! Running the hub: background loops plus the HTTP server.

use std::sync::Arc;
use std::time::Duration;

use crate::hub::{Hub, Result};
use crate::{agents, api, images, router, scheduler, services, supervisor};

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

/// Listen on the hub's Unix socket, replacing one a previous run left behind. Only the hub's user
/// may connect (containers run as that user under rootless Podman).
pub fn bind_socket(path: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    // `sun_path` holds 108 bytes including the terminating NUL.
    const MAX_SOCKET_PATH: usize = 107;
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "the hub's socket path {} is {len} bytes, more than the {MAX_SOCKET_PATH} Unix sockets allow; \
                 set XDG_RUNTIME_DIR to a shorter directory",
                path.display()
            ),
        ));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serve until `shutdown` resolves. Startup is recovery: everything is reconciled from the store,
/// the monorepo and the container runtime before work starts.
pub async fn serve(hub: Arc<Hub>, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    crate::assets::unpack(&hub.paths)?;
    // Services and the agents granted them meet on this network (created once, kept).
    hub.containers.ensure_network(&hub.network()).await?;
    agents::reconcile_on_boot(&hub).await?;
    // Interrupted proposals are collected first: the template upgrade may open a new one (its
    // pipeline already runs) or supersede an interrupted one (which must not restart).
    let interrupted = crate::transactions::interrupted(&hub).await?;
    if let Err(e) = crate::template::upgrade_on_boot(&hub).await {
        tracing::error!(error = %e, "could not propose the template upgrade");
    }
    crate::transactions::resume_on_boot(&hub, interrupted).await?;
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
    tokio::spawn(services::run(hub.clone()));
    tokio::spawn(images::collect_garbage_loop(hub.clone()));
    tokio::spawn(crate::storage::run(hub.clone()));
    tokio::spawn(images::prebuild_loop(hub.clone()));
    tokio::spawn(agents::run(hub.clone()));
    tokio::spawn(crate::search::run(hub.clone()));
    tokio::spawn(crate::model_catalog::run(hub.clone()));

    let listener = tokio::net::TcpListener::bind(hub.config.listen).await?;
    tracing::info!(addr = %hub.config.listen, commit = %hub.applied().await.commit, "botler hub listening");
    let app = api::router(hub.clone());
    let socket = hub.paths.socket()?;
    let unix = bind_socket(&socket)?;
    tracing::info!(socket = %socket.display(), "containers reach the hub on its socket");
    let socket_app = app.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(unix, socket_app).await {
            tracing::error!(error = %e, "the hub's socket stopped serving");
        }
    });
    let apps_addr = std::net::SocketAddr::new(hub.config.listen.ip(), crate::apps::port(&hub));
    // Apps are optional: botler runs without them when their port is taken.
    match tokio::net::TcpListener::bind(apps_addr).await {
        Ok(apps) => {
            tracing::info!(addr = %apps_addr, "apps listening");
            let apps_app = crate::apps::router(hub.clone());
            tokio::spawn(async move {
                if let Err(e) = axum::serve(apps, apps_app).await {
                    tracing::error!(error = %e, "the apps port stopped serving");
                }
            });
        }
        Err(e) => tracing::error!(addr = %apps_addr, error = %e, "cannot serve apps; the web app still works"),
    }
    let stop_hub = hub.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            tracing::info!("shutting down: stopping agents");
            agents::shutdown(&stop_hub).await;
        })
        .await?;
    Ok(())
}
