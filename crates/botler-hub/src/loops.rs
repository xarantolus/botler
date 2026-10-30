//! Loop defences between agents: how an agent-to-agent event is delivered, and resuming a
//! paused pair. The rules themselves are pure, in [`botler_core::loops`].

use botler_core::event::kinds;
use botler_core::event::payload::{BreakerKind, BreakerTripped, MessageExcerpt};
use botler_core::loops::{PairLimit, pair_members, pair_overflow, pair_subject, wakes};
use botler_core::{Event, Provenance, Source};
use chrono::{DateTime, Utc};

use crate::hub::{Hub, HubError, Result};

/// kv owner for the times the user resumed a pair (a reset point for the pair's count).
const RESUMED: &str = "hub:breakers";
/// Messages shown in a pair breaker notice.
const EXCERPTS: usize = 6;
const EXCERPT_CHARS: usize = 200;

/// How one agent-to-agent event reaches its recipient.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentDelivery {
    /// Deliver; `quiet` ones are notes that never start a turn.
    Deliver { quiet: bool },
    /// Hold until the user clears the pair breaker `subject`.
    Hold { subject: String },
}

/// The text of an agent-to-agent event, if it is one the loop defences look at.
fn exchange_text(ev: &Event) -> Option<&str> {
    let field = match ev.kind.as_str() {
        kinds::MESSAGE => "text",
        kinds::AGENT_IDLE => "last_reply",
        _ => return None,
    };
    Some(ev.payload.get(field).and_then(|t| t.as_str()).unwrap_or(""))
}

/// Decide how `ev` from agent `from` reaches agent `to` (`ev` is already journaled).
pub async fn agent_delivery(hub: &Hub, ev: &Event, from: &str, to: &str) -> Result<AgentDelivery> {
    let Some(text) = exchange_text(ev) else { return Ok(AgentDelivery::Deliver { quiet: false }) };
    let subject = pair_subject(from, to);
    if hub.store.breaker(&subject).await?.is_some() {
        return Ok(AgentDelivery::Hold { subject });
    }
    let now = Utc::now();
    let limit = PairLimit::default();
    let exchanges = hub.store.agent_exchanges(from, to, now - limit.window).await?;
    let times: Vec<DateTime<Utc>> = exchanges.iter().map(|e| e.at).collect();
    let outside = hub.store.last_outside_event_to(&[from, to]).await?;
    let resumed = resumed_at(hub, &subject).await?;
    let reset = outside.max(resumed);
    if pair_overflow(&times, reset, now, limit) {
        trip(hub, &subject, from, to, &exchanges, reset).await?;
        return Ok(AgentDelivery::Hold { subject });
    }
    let asked = hub.store.last_message(to, from).await?;
    let asked = asked.as_ref().and_then(|m| m.payload.get("text")).and_then(|t| t.as_str());
    Ok(AgentDelivery::Deliver { quiet: !wakes(text, asked) })
}

async fn resumed_at(hub: &Hub, subject: &str) -> Result<Option<DateTime<Utc>>> {
    let Some(v) = hub.store.kv_get(RESUMED, subject).await? else { return Ok(None) };
    DateTime::parse_from_rfc3339(&v).map(|t| Some(t.with_timezone(&Utc))).map_err(|e| {
        HubError::Store(botler_store::Error::Corrupt { table: "kv", detail: format!("{RESUMED}/{subject}: {e}") })
    })
}

async fn trip(
    hub: &Hub,
    subject: &str,
    from: &str,
    to: &str,
    exchanges: &[Event],
    reset: Option<DateTime<Utc>>,
) -> Result<()> {
    let counted = exchanges.iter().filter(|e| reset.is_none_or(|r| e.at > r)).count();
    let reason = format!(
        "`{from}` and `{to}` exchanged {counted} messages within {} minutes without you or anything outside in between",
        PairLimit::default().window.num_minutes()
    );
    if !hub.store.trip_breaker(subject, "pair", &reason).await? {
        return Ok(());
    }
    tracing::warn!(%subject, %reason, "pair breaker tripped");
    let recent = exchanges
        .iter()
        .rev()
        .take(EXCERPTS)
        .rev()
        .map(|e| MessageExcerpt {
            from: match &e.source {
                Source::Agent(a) => a.clone(),
                other => other.key(),
            },
            to: e.to.clone().unwrap_or_default(),
            at: e.at,
            text: exchange_text(e).unwrap_or("").chars().take(EXCERPT_CHARS).collect(),
        })
        .collect();
    let (a, b) = pair_members(subject).unwrap_or((from, to));
    let payload = BreakerTripped {
        subject: subject.into(),
        breaker: BreakerKind::Pair,
        reason,
        correlation: None,
        pair: Some([a.into(), b.into()]),
        recent,
    };
    let alarm = Event::new(kinds::BREAKER_TRIPPED, Source::Hub, Provenance::Trusted, Utc::now())
        .to(botler_core::FRONT_DOOR)
        .urgent(true)
        .payload(to_value(&payload)?);
    hub.emit(alarm).await?;
    Ok(())
}

/// Serialise a typed payload.
pub fn to_value<T: serde::Serialize>(payload: &T) -> Result<serde_json::Value> {
    serde_json::to_value(payload).map_err(|e| HubError::Io(std::io::Error::other(e)))
}

/// The user resumes whatever a breaker paused. For a pair: held messages are delivered and the
/// pair's count starts again from now. Returns whether a breaker was tripped and how many
/// deliveries were released.
pub async fn clear_breaker(hub: &Hub, subject: &str) -> Result<(bool, u64)> {
    let cleared = hub.store.clear_breaker(subject).await?;
    let mut released = 0;
    if pair_members(subject).is_some() {
        hub.store.kv_set(RESUMED, subject, &Utc::now().to_rfc3339()).await?;
        released = hub.store.release_held(subject).await?;
    }
    tracing::info!(%subject, cleared, released, "breaker cleared by the user");
    hub.dispatch_notify.notify_one();
    Ok((cleared, released))
}
