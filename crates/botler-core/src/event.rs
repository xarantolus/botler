//! Events: the single message shape that moves through the hub.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How far the content of an event can be trusted.
///
/// `Trusted` is reserved for the user and the hub itself. Anything an agent or job relays from the
/// outside world is `External` (a known source such as a connector) or `Untrusted` (arbitrary
/// content such as a web page or an inbound mail body).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    Trusted,
    External,
    Untrusted,
}

impl Provenance {
    /// Combining content lowers trust to the least trusted part.
    pub fn min(self, other: Self) -> Self {
        std::cmp::max(self, other)
    }
}

/// Who produced an event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum Source {
    User,
    Hub,
    Agent(String),
    Runnable(String),
    Trigger(String),
    Webhook(String),
}

impl Source {
    /// Stable string form used for rate limiting and storage, e.g. `agent:botler`.
    pub fn key(&self) -> String {
        match self {
            Self::User => "user".into(),
            Self::Hub => "hub".into(),
            Self::Agent(n) => format!("agent:{n}"),
            Self::Runnable(n) => format!("runnable:{n}"),
            Self::Trigger(n) => format!("trigger:{n}"),
            Self::Webhook(n) => format!("webhook:{n}"),
        }
    }
}

/// An event. Immutable once journaled.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[ts(rename = "BotlerEvent")]
pub struct Event {
    pub id: Uuid,
    /// Dotted type, e.g. `message`, `runnable.failed`, `mail.received`.
    #[serde(rename = "type")]
    pub kind: String,
    pub source: Source,
    pub provenance: Provenance,
    /// Direct address: an agent instance name. `None` means "route by subscription".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub to: Option<String>,
    /// Correlation chain root: replies carry the id of the event that started the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correlation: Option<Uuid>,
    /// How many hops this correlation chain has taken; used by the cycle breaker.
    #[serde(default)]
    pub hops: u32,
    #[serde(default)]
    pub urgent: bool,
    pub at: DateTime<Utc>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl Event {
    pub fn new(kind: impl Into<String>, source: Source, provenance: Provenance, at: DateTime<Utc>) -> Self {
        Self {
            id: Uuid::now_v7(),
            kind: kind.into(),
            source,
            provenance,
            to: None,
            correlation: None,
            hops: 0,
            urgent: false,
            at,
            payload: serde_json::Value::Null,
        }
    }

    pub fn to(mut self, instance: impl Into<String>) -> Self {
        self.to = Some(instance.into());
        self
    }

    pub fn payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }

    pub fn urgent(mut self, urgent: bool) -> Self {
        self.urgent = urgent;
        self
    }

    /// Make this event a reply/continuation of `parent`, extending its correlation chain.
    pub fn in_reply_to(mut self, parent: &Event) -> Self {
        self.correlation = Some(parent.correlation.unwrap_or(parent.id));
        self.hops = parent.hops + 1;
        self.provenance = self.provenance.min(parent.provenance);
        self
    }

    /// Payload field lookup by dotted path, for subscription filters.
    pub fn field(&self, path: &str) -> Option<&serde_json::Value> {
        path.split('.').try_fold(&self.payload, |v, key| v.get(key))
    }
}

/// Well-known event types emitted by the hub itself.
pub mod kinds {
    pub const MESSAGE: &str = "message";
    pub const RUNNABLE_FAILED: &str = "runnable.failed";
    pub const RUNNABLE_FINISHED: &str = "runnable.finished";
    pub const DIALOG_ANSWERED: &str = "dialog.answered";
    pub const DIALOG_ABORTED: &str = "dialog.aborted";
    pub const PROPOSAL_UPDATED: &str = "proposal.updated";
    pub const TRIAL_FINISHED: &str = "trial.finished";
    pub const ACTION_RESULT: &str = "action.result";
    pub const BREAKER_TRIPPED: &str = "breaker.tripped";
    pub const TIMER: &str = "timer";
    pub const ARCHIVE_REQUESTED: &str = "archive.requested";
    pub const AGENT_IDLE: &str = "agent.idle";
    pub const SCHEDULE: &str = "schedule";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(kind: &str) -> Event {
        Event::new(kind, Source::User, Provenance::Trusted, Utc::now())
    }

    #[test]
    fn reply_extends_chain_and_lowers_trust() {
        let root = Event { provenance: Provenance::Untrusted, ..ev("mail.received") };
        let reply = ev("message").in_reply_to(&root);
        assert_eq!(reply.correlation, Some(root.id));
        assert_eq!(reply.hops, 1);
        assert_eq!(reply.provenance, Provenance::Untrusted);
        let again = ev("message").in_reply_to(&reply);
        assert_eq!(again.correlation, Some(root.id));
        assert_eq!(again.hops, 2);
    }

    #[test]
    fn field_lookup() {
        let e = ev("x").payload(json!({"a": {"b": 3}}));
        assert_eq!(e.field("a.b"), Some(&json!(3)));
        assert_eq!(e.field("a.c"), None);
    }

    #[test]
    fn serde_shape() {
        let e = ev("message").to("main");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["source"], json!({"kind": "user"}));
        assert_eq!(v["to"], "main");
    }
}
