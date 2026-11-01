//! A chat's lifecycle as the user steers it: stopping it (and what it started), and handing it
//! over (to a fresh session of itself, or to another chat).

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
            agent.abort().await.map_err(crate::hub::HubError::from)?;
            true
        }
        _ => false,
    };
    // Ended now, even if the hub stops before pi reports the abort: a stopped chat is not resumed.
    hub.store.end_turn(chat, true).await?;
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

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct Handover {
    /// The chat (name or slug).
    pub chat: String,
    /// `self` (default): continue on a fresh session seeded with its report. Otherwise a chat or
    /// an agent definition that takes over; the chat is then archived.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct HandoverRequested {
    pub chat: String,
    pub to: String,
    /// False when a handover was already pending.
    pub requested: bool,
}

impl Op for Handover {
    const NAME: &'static str = "handover";
    const DESCRIPTION: &'static str = "Ask a chat to hand over now: to a fresh session of itself (`to = self`, the default) or to another chat or agent (then it is archived). The chat writes its report with `handover_ready`.";
    const WHO: Who = Who::User;
    type Output = HandoverRequested;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<HandoverRequested, OpError> {
        let chat = hub
            .store
            .resolve_instance(&self.chat)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {}", self.chat)))?
            .name;
        let to = self.to.unwrap_or_else(|| crate::handover::SELF.into());
        let requested = crate::handover::request(hub, &chat, &to, self.reason).await?;
        Ok(HandoverRequested { chat, to, requested })
    }
}

#[derive(Debug, Serialize, TS)]
pub struct ChatDone {
    pub chat: String,
    pub note: String,
}

async fn chat_named(hub: &Hub, chat: &str) -> Result<botler_store::Instance, OpError> {
    let inst =
        hub.store.resolve_instance(chat).await?.ok_or_else(|| OpError::BadRequest(format!("no chat named {chat}")))?;
    if inst.state == botler_store::InstanceState::Archived {
        return Err(OpError::BadRequest(format!("{} is archived; restore it first", inst.name)));
    }
    Ok(inst)
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct CompactChat {
    /// The chat (name or slug).
    pub chat: String,
}

impl Op for CompactChat {
    const NAME: &'static str = "compact_chat";
    const DESCRIPTION: &'static str = "Compact a chat's conversation now: pi summarizes the older part and the chat goes on from the summary (`/compact`).";
    const WHO: Who = Who::User;
    type Output = ChatDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatDone, OpError> {
        let inst = chat_named(hub, &self.chat).await?;
        let agent = crate::agents::ensure_awake(hub, &inst).await?;
        if agent.busy() {
            return Err(OpError::BadRequest("it is working; compact once its turn ends, or stop it".into()));
        }
        let note = match agent.pi.compact().await {
            Ok(()) => "Compacted.".into(),
            // pi keeps compacting; its compaction_end shows in the chat when it is done.
            Err(botler_runtime::PiError::Timeout { .. }) => "Still compacting; the chat shows when it is done.".into(),
            Err(e) => return Err(crate::hub::HubError::from(e).into()),
        };
        Ok(ChatDone { chat: inst.name, note })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ClearChat {
    /// The chat (name or slug).
    pub chat: String,
}

impl Op for ClearChat {
    const NAME: &'static str = "clear_chat";
    const DESCRIPTION: &'static str = "Start a chat over on an empty session, carrying nothing over (`/clear`). Its home, memory and the old session stay; `handover` instead carries a report over.";
    const WHO: Who = Who::User;
    type Output = ChatDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatDone, OpError> {
        let inst = chat_named(hub, &self.chat).await?;
        let report = botler_store::HandoverReport {
            summary: "The user cleared the chat: a fresh start, nothing carried over from the previous session.".into(),
            outstanding: Vec::new(),
            state: Vec::new(),
            handoff: None,
        };
        crate::handover::rollover(hub, &inst.name, &report).await?;
        Ok(ChatDone { chat: inst.name, note: "Cleared: the chat continues on an empty session.".into() })
    }
}
