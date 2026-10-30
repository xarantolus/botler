//! A chat's lifecycle as the user steers it: stopping it (and what it started).

use std::sync::Arc;

use botler_core::payloads::{BuiltinPayload, ChatStoppedPayload};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;
use crate::router;

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct StopChat {
    /// The chat (name or slug).
    pub chat: String,
    /// Also stop every chat it started, and theirs.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub cascade: bool,
    /// Why; the chat reads it when it next wakes.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatsStopped {
    pub stopped: Vec<ChatStoppedPayload>,
}

impl Op for StopChat {
    const NAME: &'static str = "stop_chat";
    const DESCRIPTION: &'static str = "Stop a chat: abort its running turn and cancel what is queued for it; with `cascade`, also every chat it started (recursively). The chat and its parent are told with a `chat.stopped` note.";
    const WHO: Who = Who::User;
    type Output = ChatsStopped;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatsStopped, OpError> {
        let root = hub
            .store
            .resolve_instance(&self.chat)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {}", self.chat)))?
            .name;
        let mut chats = vec![root.clone()];
        let mut i = 0;
        while self.cascade && i < chats.len() {
            for c in hub.store.children(&chats[i]).await? {
                if !chats.contains(&c) {
                    chats.push(c);
                }
            }
            i += 1;
        }
        let mut stopped = Vec::new();
        for chat in chats {
            let with = (chat != root).then(|| root.clone());
            stopped.push(stop(hub, &chat, self.reason.clone(), with).await?);
        }
        Ok(ChatsStopped { stopped })
    }
}

/// Abort `chat`'s turn, cancel its queue and pending retry, and tell it and its parent.
async fn stop(
    hub: &Hub,
    chat: &str,
    reason: Option<String>,
    with: Option<String>,
) -> Result<ChatStoppedPayload, OpError> {
    let parent = hub.store.instance(chat).await?.parent;
    let aborted = match hub.agents.get(chat).await {
        Some(agent) if agent.busy() => {
            // The parent hears of it as `chat.stopped`, not as the end of an ordinary turn.
            if let Some(p) = &parent {
                agent.record_message(p);
            }
            agent.pi.abort().await.map_err(crate::hub::HubError::from)?;
            true
        }
        _ => false,
    };
    let note = format!("stopped by the user{}", reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default());
    let cancelled = hub.store.cancel_deliveries(chat, &note).await?;
    hub.store.clear_turn_retry(chat).await?;
    let payload = ChatStoppedPayload { chat: chat.into(), reason, aborted, cancelled, with };
    tracing::info!(chat, aborted, cancelled, "chat stopped by the user");
    router::note(hub, chat, BuiltinPayload::ChatStopped(payload.clone())).await?;
    if let Some(parent) = parent
        && hub.store.find_instance(&parent).await?.is_some()
    {
        router::note(hub, &parent, BuiltinPayload::ChatStopped(payload.clone())).await?;
    }
    Ok(payload)
}
