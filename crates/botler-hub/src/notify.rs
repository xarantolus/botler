//! Telling the user something needs them. Live viewers get every dialog; push (ntfy, milestone 7)
//! will carry only system-written text for blocking dialogs.

use botler_store::Dialog;

use crate::hub::Hub;

#[derive(Clone, Debug, serde::Serialize)]
pub struct DialogNotice {
    pub dialog_id: String,
    pub instance: String,
    pub priority: String,
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
}
