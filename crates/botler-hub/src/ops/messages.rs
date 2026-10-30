//! Messages and events: talking to agents, emitting facts, reading the journal.

use std::sync::Arc;

use botler_core::defs::Undeclared;
use botler_core::{Event, Provenance};
use botler_store::EventQuery;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

use super::{Caller, Op, OpError, Who};
use crate::auth::Principal;
use crate::hub::Hub;

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct SendMessage {
    /// Recipient instance name.
    pub to: String,
    pub text: String,
    /// Interrupt the recipient at its next turn instead of waiting until it is idle.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub urgent: bool,
    /// Event id this answers, to keep the conversation thread.
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct MessageSent {
    pub event_id: Uuid,
    /// The recipient's instance name.
    pub to: String,
}

impl Op for SendMessage {
    const NAME: &'static str = "send_message";
    const DESCRIPTION: &'static str = "Send a message to an agent instance (e.g. `botler`). Replies come back to you as messages. Set `reply_to` to the event you are answering to keep the thread.";
    const WHO: Who = Who::Everyone;
    type Output = MessageSent;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<MessageSent, OpError> {
        let to = match hub.store.resolve_instance(&self.to).await? {
            Some(i) => i.name,
            None if hub.applied().await.defs.agents.contains_key(&self.to) => self.to.clone(),
            None => return Err(OpError::BadRequest(format!("no agent instance or chat named {}", self.to))),
        };
        if caller.instance() == Some(to.as_str()) {
            return Err(OpError::BadRequest(
                "that is you: a message to yourself would only wake you again. To remember something, write it to a \
                 file in your home; to come back to it later, use set_timer."
                    .into(),
            ));
        }
        // Messages between agents are trusted as messages; content an agent relays from outside is
        // marked by the event it came from (replies inherit the lower provenance of their parent).
        let mut ev = crate::hub::builtin(
            botler_core::payloads::BuiltinPayload::Message(botler_core::payloads::MessagePayload {
                text: self.text,
                files: vec![],
            }),
            caller.source(),
        )?
        .to(to.clone())
        .urgent(self.urgent);
        if let Some(parent) = &self.reply_to {
            let parent = hub.store.event(parent).await?;
            ev = ev.in_reply_to(&parent);
        }
        let ev = hub.emit(ev).await?;
        if let Some(me) = caller.instance()
            && let Some(live) = hub.agents.get(me).await
        {
            live.record_message(&to);
        }
        Ok(MessageSent { event_id: ev.id, to })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct EmitEvent {
    /// Dotted event type, e.g. `mail.received`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    #[ts(optional, as = "Option<Value>")]
    pub payload: Value,
    /// `external` (from a known outside source) or `untrusted` (arbitrary content). Defaults to
    /// `external`; events from agents and jobs are never `trusted`.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Serialize, TS)]
pub struct EventEmitted {
    pub event_id: Uuid,
    /// What is off about the event, if it was accepted anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning: Option<String>,
}

impl Op for EmitEvent {
    const NAME: &'static str = "emit_event";
    const DESCRIPTION: &'static str = "Emit a typed event. Triggers subscribed to its type react to it. Use for facts (`mail.received`), not for talking to someone (use send_message).";
    const WHO: Who = Who::Everyone;
    type Output = EventEmitted;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<EventEmitted, OpError> {
        if self.kind.is_empty()
            || !self.kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        {
            return Err(OpError::BadRequest("type must be lowercase dotted, e.g. mail.received".into()));
        }
        if botler_core::catalog::builtin(&self.kind).is_some() {
            return Err(OpError::Forbidden(format!("{} is a hub event type", self.kind)));
        }
        let provenance = match (caller.principal == Principal::User, self.provenance) {
            (true, p) => p.unwrap_or(Provenance::Trusted),
            // Agents and jobs can never claim trusted content.
            (false, Some(Provenance::Untrusted)) => Provenance::Untrusted,
            (false, _) => Provenance::External,
        };
        let applied = hub.applied().await;
        let check = botler_core::event_check::check(&applied.defs, &self.kind, &self.payload);
        let warning = check.feedback(&self.kind);
        if !check.accepted() {
            tracing::info!(kind = %self.kind, caller = %caller.subject(), "rejected invalid event");
            return Err(OpError::InvalidEvent(warning.unwrap_or_else(|| format!("event {} rejected", self.kind))));
        }
        let ev = hub.emit(Event::new(self.kind, caller.source(), provenance, Utc::now()).payload(self.payload)).await?;
        Ok(EventEmitted { event_id: ev.id, warning })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListEventTypes {}

#[derive(Debug, Serialize, TS)]
pub struct EventTypes {
    pub declared: Vec<DeclaredEvent>,
    pub undeclared_policy: Undeclared,
}

#[derive(Debug, Serialize, TS)]
pub struct DeclaredEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    /// JSON Schema of the payload.
    pub schema: Option<Value>,
}

impl Op for ListEventTypes {
    const NAME: &'static str = "list_event_types";
    const DESCRIPTION: &'static str =
        "Declared event types with their payload schemas. emit_event validates payloads against these.";
    const WHO: Who = Who::Everyone;
    type Output = EventTypes;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<EventTypes, OpError> {
        let applied = hub.applied().await;
        let declared = applied
            .defs
            .events
            .values()
            .map(|e| DeclaredEvent {
                kind: e.kind.clone(),
                description: e.description.clone(),
                schema: e.schema.clone(),
            })
            .collect();
        Ok(EventTypes { declared, undeclared_policy: applied.defs.instance.events.undeclared })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct GetEvents {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub correlation: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl Op for GetEvents {
    const NAME: &'static str = "get_events";
    const DESCRIPTION: &'static str =
        "Read the event journal, newest first: filter by type (`mail.*`) or conversation (`correlation`).";
    const WHO: Who = Who::UserAndAgents;
    type Output = Vec<Event>;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Vec<Event>, OpError> {
        let q = EventQuery {
            kind: self.kind,
            correlation: self.correlation,
            since: None,
            limit: self.limit.unwrap_or(20).min(200),
        };
        Ok(hub.store.events(&q).await?)
    }
}
