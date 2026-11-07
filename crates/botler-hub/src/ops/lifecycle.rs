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

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct PinChat {
    /// The chat (name or slug).
    pub chat: String,
    pub pinned: bool,
}

impl Op for PinChat {
    const NAME: &'static str = "pin_chat";
    const DESCRIPTION: &'static str = "Pin a chat to the top of the list (right after the front door), or unpin it.";
    const WHO: Who = Who::User;
    type Output = ChatDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatDone, OpError> {
        let inst = hub
            .store
            .resolve_instance(&self.chat)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {}", self.chat)))?;
        hub.store.set_pinned(&inst.name, self.pinned).await?;
        let note = if self.pinned { "Pinned." } else { "Unpinned." };
        Ok(ChatDone { chat: inst.name, note: note.into() })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct SetChatContext {
    /// The chat (name or slug).
    pub chat: String,
    /// Compact once its context passes this many tokens; null: its agent's default.
    pub compact_at: Option<u64>,
}

impl Op for SetChatContext {
    const NAME: &'static str = "set_chat_context";
    const DESCRIPTION: &'static str = "Set how large a chat's context grows before it is compacted (tokens), or \
        reset it to its agent's default. A running agent is restarted after its turn to apply it.";
    const WHO: Who = Who::User;
    type Output = ChatDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatDone, OpError> {
        if self.compact_at.is_some_and(|t| t < 20_000) {
            return Err(OpError::BadRequest("a context under 20k tokens leaves no room to work".into()));
        }
        let inst = chat_named(hub, &self.chat).await?;
        hub.store.set_compact_at(&inst.name, self.compact_at).await?;
        crate::agents::write_pi_settings(hub, &inst.name).await?;
        hub.dispatch_notify.notify_one();
        let note = match self.compact_at {
            Some(t) => format!("Compacts at {}k tokens.", t / 1000),
            None => "Compacts at its agent's default.".into(),
        };
        Ok(ChatDone { chat: inst.name, note })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GetContextAdvice {
    /// The chat (name or slug).
    pub chat: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ContextAdviceView {
    /// Null while the chat has too few recent calls to say.
    pub advice: Option<crate::context_advice::ContextAdvice>,
    /// Whether the best size beats the current one clearly enough to suggest it.
    pub suggested: bool,
}

impl Op for GetContextAdvice {
    const NAME: &'static str = "context_advice";
    const DESCRIPTION: &'static str =
        "What the chat's model calls of the last week would have cost at each context size.";
    const WHO: Who = Who::User;
    type Output = ContextAdviceView;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ContextAdviceView, OpError> {
        let inst = chat_named(hub, &self.chat).await?;
        let advice = crate::context_advice::for_chat(hub, &inst.name).await?;
        let suggested = advice.as_ref().and_then(crate::context_advice::suggestion).is_some();
        Ok(ContextAdviceView { advice, suggested })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct RenameChat {
    /// The chat (name or slug).
    pub chat: String,
    pub title: String,
}

impl Op for RenameChat {
    const NAME: &'static str = "rename_chat";
    const DESCRIPTION: &'static str = "Give a chat the user's own title; its agent keeps it from then on.";
    const WHO: Who = Who::User;
    type Output = ChatDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatDone, OpError> {
        let inst = hub
            .store
            .resolve_instance(&self.chat)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {}", self.chat)))?;
        let title = self.title.trim();
        if title.is_empty() || title.len() > 120 {
            return Err(OpError::BadRequest("the title must be 1-120 characters".into()));
        }
        hub.store.set_user_title(&inst.name, title).await?;
        Ok(ChatDone { chat: inst.name, note: format!("Renamed to {title}.") })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct RaiseBudget {
    /// The chat (name or slug).
    pub chat: String,
    /// Dollars more today.
    #[serde(default)]
    pub usd: Option<f64>,
    /// Tokens more today (for a budget in tokens).
    #[serde(default)]
    pub tokens: Option<u64>,
}

#[derive(Debug, Serialize, TS)]
pub struct BudgetRaised {
    pub chat: String,
    /// Today's budget now.
    pub usd_per_day: Option<f64>,
    pub tokens_per_day: Option<u64>,
    pub spent_usd: f64,
    pub used_tokens: i64,
}

impl Op for RaiseBudget {
    const NAME: &'static str = "raise_budget";
    const DESCRIPTION: &'static str = "Allow a chat more than its daily budget, for today only (the definition is unchanged), and resume it if the budget had stopped it.";
    const WHO: Who = Who::User;
    type Output = BudgetRaised;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<BudgetRaised, OpError> {
        let inst = chat_named(hub, &self.chat).await?;
        let usd = self.usd.unwrap_or(0.0);
        if !usd.is_finite() || usd < 0.0 {
            return Err(OpError::BadRequest("usd must be a positive amount".into()));
        }
        let tokens =
            i64::try_from(self.tokens.unwrap_or(0)).map_err(|_| OpError::BadRequest("too many tokens".into()))?;
        let (_, _, day) = crate::agents::budget_today(hub, &inst.name).await?;
        hub.store.add_budget_extra(&inst.name, &day, usd, tokens).await?;
        let (budget, today, _) = crate::agents::budget_today(hub, &inst.name).await?;
        let under = budget.usd_per_day.is_none_or(|b| today.cost_usd <= b)
            && budget.tokens_per_day.is_none_or(|b| today.tokens <= i64::try_from(b).unwrap_or(i64::MAX));
        if under {
            let subject = format!("instance:{}", inst.name);
            if hub.store.breaker(&subject).await?.is_some_and(|b| b.kind == "budget") {
                crate::loops::clear_breaker(hub, &subject).await?;
            }
        }
        Ok(BudgetRaised {
            chat: inst.name,
            usd_per_day: budget.usd_per_day,
            tokens_per_day: budget.tokens_per_day,
            spent_usd: today.cost_usd,
            used_tokens: today.tokens,
        })
    }
}
