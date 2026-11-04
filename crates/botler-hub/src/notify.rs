//! Telling the user something needs them: live viewers and paired channels get every dialog.

use botler_store::Dialog;

use crate::hub::Hub;

#[derive(Clone, Debug, serde::Serialize, ts_rs::TS)]
pub struct DialogNotice {
    pub dialog_id: String,
    pub instance: String,
    pub priority: String,
}

/// Live viewers refetch the requests when one closed without an answer.
pub fn dialog_closed(hub: &Hub, d: &Dialog) {
    let notice = crate::hub::Live::Dialog {
        notice: DialogNotice {
            dialog_id: d.id.clone(),
            instance: d.instance.clone(),
            priority: d.priority.to_string(),
        },
    };
    if hub.live.send(notice).is_err() {
        tracing::trace!("no live viewers");
    }
}

pub async fn dialog_opened(hub: &Hub, d: &Dialog) {
    tracing::info!(dialog = %d.id, instance = %d.instance, priority = %d.priority, "dialog opened");
    let notice = crate::hub::Live::Dialog {
        notice: DialogNotice {
            dialog_id: d.id.clone(),
            instance: d.instance.clone(),
            priority: d.priority.to_string(),
        },
    };
    if hub.live.send(notice).is_err() {
        tracing::trace!("no live viewers");
    }
    if let Err(e) = crate::channels::dialog_opened(hub, d).await {
        tracing::error!(dialog = %d.id, error = %e, "could not tell the channel about the request");
    }
}
