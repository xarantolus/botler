//! Typed payloads of the built-in event types (see [`crate::catalog`] for what each means).
//!
//! Events travel as [`Event`] with a JSON payload, because agent-declared event types have
//! payloads only their schemas know. Built-in types have a fixed shape, defined here once:
//! the hub builds them through [`Event::builtin`], and the web app's TypeScript types are
//! generated from them (`BuiltinPayload`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::{Event, Provenance, Source, kinds};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct MessagePayload {
    pub text: String,
    /// Paths of attached files under `/inbox/files`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RunnableFailedPayload {
    pub runnable: String,
    /// `exit`, `timeout`, `oom`, `unhealthy`, `crashloop`, `start`, ...
    pub kind: String,
    pub exit_code: Option<i64>,
    pub run_id: Option<String>,
    pub incident_id: String,
    pub count: i64,
    pub logs_tail: String,
    pub commit: Option<String>,
    pub event_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RunnableFinishedPayload {
    pub runnable: String,
    pub run_id: String,
    pub output: String,
}

/// What a credentials request did when the user answered it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SecretsOutcome {
    pub declined: bool,
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub filled: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct DialogAnsweredPayload {
    pub dialog_id: String,
    pub question: String,
    pub answer: String,
    /// Credentials requests only: the secret group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group: Option<String>,
    /// Credentials requests only: what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub outcome: Option<SecretsOutcome>,
    /// Guidance for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct DialogAbortedPayload {
    pub dialog_id: String,
    pub question: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ProposalUpdatedPayload {
    pub proposal_id: String,
    pub title: String,
    /// `gating`, `reviewing`, `awaiting_user`, `merged`, `rejected`, `failed`.
    pub status: String,
    pub detail: String,
    /// The deployment summary (markdown), when it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ActionResultPayload {
    pub proposal_id: String,
    pub action: String,
    pub ok: bool,
    pub result: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct BreakerTrippedPayload {
    pub subject: String,
    /// `cycle`, `rate` or `wake`.
    pub breaker: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correlation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ArchiveRequestedPayload {
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AgentIdlePayload {
    pub instance: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub last_reply: String,
}

/// A fired timer: the payload it was set with, plus its id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct TimerPayload {
    pub timer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    /// Whatever else the timer was set with.
    #[serde(flatten)]
    #[ts(skip)]
    pub data: BTreeMap<String, serde_json::Value>,
}

/// Every built-in event type with its payload. Serialized as `{ "type": ..., "payload": ... }`,
/// the same two fields an [`Event`] has.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", content = "payload")]
pub enum BuiltinPayload {
    #[serde(rename = "message")]
    Message(MessagePayload),
    #[serde(rename = "runnable.failed")]
    RunnableFailed(RunnableFailedPayload),
    #[serde(rename = "runnable.finished")]
    RunnableFinished(RunnableFinishedPayload),
    #[serde(rename = "dialog.answered")]
    DialogAnswered(DialogAnsweredPayload),
    #[serde(rename = "dialog.aborted")]
    DialogAborted(DialogAbortedPayload),
    #[serde(rename = "proposal.updated")]
    ProposalUpdated(ProposalUpdatedPayload),
    #[serde(rename = "action.result")]
    ActionResult(ActionResultPayload),
    #[serde(rename = "breaker.tripped")]
    BreakerTripped(BreakerTrippedPayload),
    #[serde(rename = "archive.requested")]
    ArchiveRequested(ArchiveRequestedPayload),
    #[serde(rename = "agent.idle")]
    AgentIdle(AgentIdlePayload),
    #[serde(rename = "timer")]
    Timer(TimerPayload),
}

impl BuiltinPayload {
    /// The event type this payload belongs to.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Message(_) => kinds::MESSAGE,
            Self::RunnableFailed(_) => kinds::RUNNABLE_FAILED,
            Self::RunnableFinished(_) => kinds::RUNNABLE_FINISHED,
            Self::DialogAnswered(_) => kinds::DIALOG_ANSWERED,
            Self::DialogAborted(_) => kinds::DIALOG_ABORTED,
            Self::ProposalUpdated(_) => kinds::PROPOSAL_UPDATED,
            Self::ActionResult(_) => kinds::ACTION_RESULT,
            Self::BreakerTripped(_) => kinds::BREAKER_TRIPPED,
            Self::ArchiveRequested(_) => kinds::ARCHIVE_REQUESTED,
            Self::AgentIdle(_) => kinds::AGENT_IDLE,
            Self::Timer(_) => kinds::TIMER,
        }
    }

    fn to_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        match self {
            Self::Message(p) => serde_json::to_value(p),
            Self::RunnableFailed(p) => serde_json::to_value(p),
            Self::RunnableFinished(p) => serde_json::to_value(p),
            Self::DialogAnswered(p) => serde_json::to_value(p),
            Self::DialogAborted(p) => serde_json::to_value(p),
            Self::ProposalUpdated(p) => serde_json::to_value(p),
            Self::ActionResult(p) => serde_json::to_value(p),
            Self::BreakerTripped(p) => serde_json::to_value(p),
            Self::ArchiveRequested(p) => serde_json::to_value(p),
            Self::AgentIdle(p) => serde_json::to_value(p),
            Self::Timer(p) => serde_json::to_value(p),
        }
    }

    /// Read the typed payload of a built-in event; `None` for other event types.
    pub fn of(event: &Event) -> Option<Result<Self, serde_json::Error>> {
        crate::catalog::builtin(&event.kind)?;
        Some(serde_json::from_value(serde_json::json!({ "type": event.kind, "payload": event.payload })))
    }
}

impl Event {
    /// A built-in event with its typed payload.
    pub fn builtin(
        payload: BuiltinPayload,
        source: Source,
        provenance: Provenance,
        at: DateTime<Utc>,
    ) -> Result<Self, serde_json::Error> {
        Ok(Event::new(payload.kind(), source, provenance, at).payload(payload.to_value()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::BUILTIN;

    fn fields<T: JsonSchema>() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
        let mut f: Vec<String> =
            schema["properties"].as_object().map(|p| p.keys().cloned().collect()).unwrap_or_default();
        f.sort();
        f
    }

    /// The catalog tells agents what a payload contains; it must match the real shape.
    #[test]
    fn catalog_fields_match_payload_types() {
        let typed: Vec<(&str, Vec<String>)> = vec![
            (kinds::MESSAGE, fields::<MessagePayload>()),
            (kinds::RUNNABLE_FAILED, fields::<RunnableFailedPayload>()),
            (kinds::RUNNABLE_FINISHED, fields::<RunnableFinishedPayload>()),
            (kinds::DIALOG_ANSWERED, fields::<DialogAnsweredPayload>()),
            (kinds::DIALOG_ABORTED, fields::<DialogAbortedPayload>()),
            (kinds::PROPOSAL_UPDATED, fields::<ProposalUpdatedPayload>()),
            (kinds::ACTION_RESULT, fields::<ActionResultPayload>()),
            (kinds::BREAKER_TRIPPED, fields::<BreakerTrippedPayload>()),
            (kinds::ARCHIVE_REQUESTED, fields::<ArchiveRequestedPayload>()),
            (kinds::AGENT_IDLE, fields::<AgentIdlePayload>()),
            (kinds::TIMER, fields::<TimerPayload>()),
        ];
        assert_eq!(typed.len(), BUILTIN.len(), "every built-in event type has a payload type");
        for (kind, typed_fields) in typed {
            let entry = crate::catalog::builtin(kind).unwrap_or_else(|| panic!("{kind} is not in the catalog"));
            let mut documented: Vec<String> = entry.fields.iter().map(|(n, _)| n.to_string()).collect();
            documented.sort();
            assert_eq!(documented, typed_fields, "catalog fields of {kind}");
        }
    }

    #[test]
    fn builtin_roundtrip() {
        let p = BuiltinPayload::Timer(TimerPayload {
            timer_id: "t1".into(),
            note: Some("vacuum".into()),
            data: BTreeMap::from([("room".into(), serde_json::json!("kitchen"))]),
        });
        let ev = Event::builtin(p.clone(), Source::Hub, Provenance::Trusted, Utc::now()).unwrap();
        assert_eq!(ev.kind, "timer");
        assert_eq!(ev.payload["room"], "kitchen");
        assert_eq!(BuiltinPayload::of(&ev).unwrap().unwrap(), p);
        let custom = Event::new("mail.received", Source::Hub, Provenance::Trusted, Utc::now());
        assert!(BuiltinPayload::of(&custom).is_none());
    }
}
