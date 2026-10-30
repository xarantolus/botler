//! The kill switch.

use crate::hub::{Hub, Result};

/// Freeze scheduling and dispatch and stop every running agent. Events are still journaled and
/// picked up after resume; the `missed` policy of each schedule decides what to catch up.
pub async fn pause(hub: &Hub) -> Result<()> {
    hub.store.set_paused(true).await?;
    crate::agents::stop_all(hub).await;
    tracing::warn!("botler paused");
    Ok(())
}

pub async fn resume(hub: &Hub) -> Result<()> {
    hub.store.set_paused(false).await?;
    hub.route_notify.notify_one();
    hub.dispatch_notify.notify_one();
    hub.schedule_notify.notify_one();
    tracing::warn!("botler resumed");
    Ok(())
}
