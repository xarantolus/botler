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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub id: Uuid,
    /// Dotted type, e.g. `message`, `runnable.failed`, `mail.received`.
    #[serde(rename = "type")]
    pub kind: String,
    pub source: Source,
    pub provenance: Provenance,
    /// Direct address: an agent instance name. `None` means "route by subscription".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Correlation chain root: replies carry the id of the event that started the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    pub const ACTION_RESULT: &str = "action.result";
    pub const BREAKER_TRIPPED: &str = "breaker.tripped";
    pub const TIMER: &str = "timer";
    pub const ARCHIVE_REQUESTED: &str = "archive.requested";
    pub const AGENT_IDLE: &str = "agent.idle";
}

/// Typed payloads of hub events (and the delivery envelope the pi extension reads). The hub
/// builds these structs, never ad-hoc JSON, so clients can generate types from them.
pub mod payload {
    use chrono::{DateTime, Utc};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// `agent.idle`: a spawned agent ended its turn without its last action being a message to
    /// its parent. Delivered to the parent like a message from the child.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub struct AgentIdle {
        pub instance: String,
        pub title: Option<String>,
        pub slug: Option<String>,
        /// The child's final answer of the turn (its last assistant text), clipped.
        pub last_reply: String,
    }

    /// Which breaker tripped.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum BreakerKind {
        /// A correlation chain grew too long.
        Cycle,
        /// Two agents kept messaging each other without the user or the outside world in between.
        Pair,
    }

    /// One recent message of a paused conversation, for the user deciding whether to resume.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub struct MessageExcerpt {
        pub from: String,
        pub to: String,
        pub at: DateTime<Utc>,
        /// The start of the message text.
        pub text: String,
    }

    /// `breaker.tripped`: something was paused until the user clears it.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub struct BreakerTripped {
        /// The breaker's subject; clearing it resumes (`chain:<id>`, `pair:<a>|<b>`).
        pub subject: String,
        pub breaker: BreakerKind,
        pub reason: String,
        /// The correlation chain, for cycle breakers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub correlation: Option<String>,
        /// The two agents, for pair breakers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pair: Option<[String; 2]>,
        /// The last messages between them, oldest first (pair breakers).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub recent: Vec<MessageExcerpt>,
    }

    /// What the hub sends pi for one delivery (`botler:delivery <json>`); the botler pi extension
    /// parses it.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    pub struct DeliveryEnvelope {
        /// Delivery id: the extension drops ids it has already seen.
        pub id: String,
        pub event: String,
        pub kind: String,
        pub from: String,
        pub provenance: crate::Provenance,
        pub correlation: String,
        /// The rendered text the agent reads.
        pub body: String,
        /// A note (e.g. an acknowledgement from another agent): added to the session without
        /// starting a turn. The agent reads it with whatever wakes it next.
        #[serde(default)]
        pub quiet: bool,
        /// When it was delivered, in the instance's time zone (`Fri 2026-10-30 17:05
        /// Europe/Berlin`, see [`crate::clock::format_local`]).
        pub at: String,
        /// After a long silence: `first message in 3 days` (see [`crate::clock::gap_note`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub gap: Option<String>,
    }
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
