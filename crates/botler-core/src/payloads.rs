//! Typed payloads of the built-in event types (see [`crate::catalog`] for what each means), and
//! the delivery envelope the pi extension reads.
//!
//! Events travel as [`Event`] with a JSON payload, because agent-declared event types have
//! payloads only their schemas know. Built-in types have a fixed shape, defined here once:
//! the hub builds them through [`Event::builtin`] (never ad-hoc JSON), and the web app's
//! TypeScript types are generated from them (`BuiltinPayload`).

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
    /// For a message from the user while the agent works: wait for the turn to end (`queue`, held
    /// by the hub, cancellable until then) or interrupt it (`steer`). Absent: `steer`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mode: Option<SendMode>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    #[default]
    Queue,
    Steer,
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

/// How a trial run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum TrialOutcome {
    /// Exited with code 0.
    Succeeded,
    /// Exited non-zero, could not start, or reported itself unhealthy.
    Failed,
    /// Still running when its minutes were up, and stopped (what a healthy service does).
    TimeUp,
}

/// Something a trial run did that the hub recorded instead of carrying out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct TrialEffect {
    /// `emit_event`, `send_message` or `set_timer`.
    pub op: String,
    /// What it would have done: the event (emit_event, send_message) or the timer (set_timer).
    pub detail: serde_json::Value,
    pub at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct TrialFinishedPayload {
    pub run_id: String,
    pub job: String,
    pub branch: String,
    pub outcome: TrialOutcome,
    pub exit_code: Option<i64>,
    pub reason: Option<String>,
    pub logs_tail: String,
    /// The events, messages and timers it would have produced, in order.
    pub effects: Vec<TrialEffect>,
    /// Secret groups the job declares that it did not get, because you do not hold them.
    pub withheld_secrets: Vec<String>,
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
    /// The user's reason for rejecting it, or comment on approving it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ActionResultPayload {
    pub proposal_id: String,
    pub action: String,
    pub ok: bool,
    pub result: serde_json::Value,
}

/// Which breaker tripped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum BreakerKind {
    /// A correlation chain grew too long.
    Cycle,
    /// Two agents kept messaging each other without the user or the outside world in between.
    Pair,
}

/// One recent message of a paused conversation, for the user deciding whether to resume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct MessageExcerpt {
    pub from: String,
    pub to: String,
    pub at: DateTime<Utc>,
    /// The start of the message text.
    pub text: String,
}

/// `breaker.tripped`: something was paused until the user clears it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct BreakerTrippedPayload {
    /// The breaker's subject; clearing it resumes (`chain:<id>`, `pair:<a>|<b>`).
    pub subject: String,
    pub breaker: BreakerKind,
    pub reason: String,
    /// The correlation chain, for cycle breakers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub correlation: Option<String>,
    /// The two agents, for pair breakers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pair: Option<[String; 2]>,
    /// The last messages between them, oldest first (pair breakers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<MessageExcerpt>>")]
    pub recent: Vec<MessageExcerpt>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct ArchiveRequestedPayload {
    pub reason: Option<String>,
}

/// `agent.idle`: a spawned agent ended its turn without its last action being a message to
/// its parent. Delivered to the parent like a message from the child.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AgentIdlePayload {
    pub instance: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    /// The child's final answer of the turn (its last assistant text), clipped.
    pub last_reply: String,
}

/// A fired one-off timer. What the woken agent is told is the delivery's prompt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct TimerPayload {
    pub timer_id: String,
    /// What it was set for, when the setter gave a note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// A schedule (cron or interval trigger) fired.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SchedulePayload {
    /// The trigger id.
    pub trigger: String,
    /// The fire time this run stands for.
    pub scheduled_for: DateTime<Utc>,
    /// Fires missed during downtime and folded into this one (`missed = run_once`).
    pub skipped: u32,
}

/// `access.changed`: a grant of this chat was added without a request dialog (an `auto`
/// policy) or revoked. Its container changes on the next wake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct AccessChangedPayload {
    pub grant_id: String,
    /// `<kind>:<name>`, e.g. `secret:github`.
    pub resource: String,
    /// `granted` or `revoked`.
    pub change: String,
    /// What it means for the agent.
    pub note: String,
}

/// What the hub sends pi for one delivery (`botler:delivery <json>`); the botler pi extension
/// parses it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct DeliveryEnvelope {
    /// Delivery id: the extension drops ids it has already seen.
    pub id: String,
    pub event: String,
    pub kind: String,
    pub from: String,
    pub provenance: Provenance,
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
    #[ts(optional)]
    pub gap: Option<String>,
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
    #[serde(rename = "trial.finished")]
    TrialFinished(TrialFinishedPayload),
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
    #[serde(rename = "schedule")]
    Schedule(SchedulePayload),
    #[serde(rename = "access.changed")]
    AccessChanged(AccessChangedPayload),
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
            Self::TrialFinished(_) => kinds::TRIAL_FINISHED,
            Self::ActionResult(_) => kinds::ACTION_RESULT,
            Self::BreakerTripped(_) => kinds::BREAKER_TRIPPED,
            Self::ArchiveRequested(_) => kinds::ARCHIVE_REQUESTED,
            Self::AgentIdle(_) => kinds::AGENT_IDLE,
            Self::Timer(_) => kinds::TIMER,
            Self::Schedule(_) => kinds::SCHEDULE,
            Self::AccessChanged(_) => kinds::ACCESS_CHANGED,
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
            Self::TrialFinished(p) => serde_json::to_value(p),
            Self::ActionResult(p) => serde_json::to_value(p),
            Self::BreakerTripped(p) => serde_json::to_value(p),
            Self::ArchiveRequested(p) => serde_json::to_value(p),
            Self::AgentIdle(p) => serde_json::to_value(p),
            Self::Timer(p) => serde_json::to_value(p),
            Self::Schedule(p) => serde_json::to_value(p),
            Self::AccessChanged(p) => serde_json::to_value(p),
        }
    }

    /// Read the typed payload of a built-in event; `None` for other event types.
    pub fn of(event: &Event) -> Option<Result<Self, serde_json::Error>> {
        crate::catalog::builtin(&event.kind)?;
        Some(serde_json::from_value(serde_json::json!({ "type": event.kind, "payload": event.payload })))
    }
}

/// The JSON Schema of a built-in event type's payload.
pub fn builtin_schema(kind: &str) -> Option<serde_json::Value> {
    fn s<T: JsonSchema>() -> Option<serde_json::Value> {
        serde_json::to_value(schemars::schema_for!(T)).ok()
    }
    match kind {
        kinds::MESSAGE => s::<MessagePayload>(),
        kinds::RUNNABLE_FAILED => s::<RunnableFailedPayload>(),
        kinds::RUNNABLE_FINISHED => s::<RunnableFinishedPayload>(),
        kinds::DIALOG_ANSWERED => s::<DialogAnsweredPayload>(),
        kinds::DIALOG_ABORTED => s::<DialogAbortedPayload>(),
        kinds::PROPOSAL_UPDATED => s::<ProposalUpdatedPayload>(),
        kinds::ACTION_RESULT => s::<ActionResultPayload>(),
        kinds::BREAKER_TRIPPED => s::<BreakerTrippedPayload>(),
        kinds::ARCHIVE_REQUESTED => s::<ArchiveRequestedPayload>(),
        kinds::AGENT_IDLE => s::<AgentIdlePayload>(),
        kinds::TIMER => s::<TimerPayload>(),
        kinds::SCHEDULE => s::<SchedulePayload>(),
        kinds::TRIAL_FINISHED => s::<TrialFinishedPayload>(),
        kinds::ACCESS_CHANGED => s::<AccessChangedPayload>(),
        _ => None,
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
            (kinds::TRIAL_FINISHED, fields::<TrialFinishedPayload>()),
            (kinds::ACTION_RESULT, fields::<ActionResultPayload>()),
            (kinds::BREAKER_TRIPPED, fields::<BreakerTrippedPayload>()),
            (kinds::ARCHIVE_REQUESTED, fields::<ArchiveRequestedPayload>()),
            (kinds::AGENT_IDLE, fields::<AgentIdlePayload>()),
            (kinds::TIMER, fields::<TimerPayload>()),
            (kinds::SCHEDULE, fields::<SchedulePayload>()),
            (kinds::ACCESS_CHANGED, fields::<AccessChangedPayload>()),
        ];
        for b in BUILTIN {
            assert!(builtin_schema(b.kind).is_some(), "{} has no schema", b.kind);
        }
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
        let p = BuiltinPayload::Timer(TimerPayload { timer_id: "t1".into(), note: Some("vacuum".into()) });
        let ev = Event::builtin(p.clone(), Source::Hub, Provenance::Trusted, Utc::now()).unwrap();
        assert_eq!(ev.kind, "timer");
        assert_eq!(ev.payload["note"], "vacuum");
        assert_eq!(BuiltinPayload::of(&ev).unwrap().unwrap(), p);
        let custom = Event::new("mail.received", Source::Hub, Provenance::Trusted, Utc::now());
        assert!(BuiltinPayload::of(&custom).is_none());
    }
}
