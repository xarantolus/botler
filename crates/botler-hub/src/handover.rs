//! Handing a conversation over. `to = self` (rollover): the chat continues on a fresh pi session
//! seeded with its own handover report, keeping its instance, home, grants, subscriptions and
//! memory; the old session file stays readable and searchable. Any other `to` (a chat, or an agent
//! definition to spawn) gets the report and the chat is archived. Persistent agents roll over by
//! themselves when their context or compactions pass the `[defaults]` limits.

use std::sync::Arc;

use botler_core::Source;
use botler_core::payloads::{BuiltinPayload, HandoverRequestedPayload, SessionStartedPayload};
use botler_store::HandoverReport;

use crate::agents::LiveAgent;
use crate::hub::{Hub, Result, builtin};

/// kv owner of pending handovers: instance → target.
const PENDING: &str = "hub:handover";
/// The target that means "a fresh session of this chat".
pub const SELF: &str = "self";
/// How long the agent's turn gets to end after its report before the container restarts.
pub const RESTART_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

/// Ask `chat` for a handover report for `to`. A second request while one is pending is a no-op.
pub async fn request(hub: &Hub, chat: &str, to: &str, reason: Option<String>) -> Result<bool> {
    if pending(hub, chat).await?.is_some() {
        return Ok(false);
    }
    hub.store.kv_set(PENDING, chat, to).await?;
    if to != SELF {
        hub.store.request_archive(chat).await?;
    }
    tracing::info!(chat, to, ?reason, "handover requested");
    let payload = BuiltinPayload::HandoverRequested(HandoverRequestedPayload { to: to.into(), reason });
    hub.emit(builtin(payload, Source::Hub)?.to(chat)).await?;
    Ok(true)
}

/// What a chat resumed after a failed or cut-off turn is reminded of: the handover it was asked
/// for (the request was in the turn that was lost, and the chat would otherwise never report).
pub async fn reminder(hub: &Hub, chat: &str) -> Result<Option<&'static str>> {
    if hub.store.instance(chat).await?.archive_requested_at.is_some() {
        return Ok(Some(" You were asked to hand over before being archived: finish that with `handover_ready`."));
    }
    Ok(pending(hub, chat).await?.map(
        |_| " You were asked for a handover report to continue in a fresh session: finish that with `handover_ready`.",
    ))
}

/// Where `chat`'s pending handover goes, if one was requested.
pub async fn pending(hub: &Hub, chat: &str) -> Result<Option<String>> {
    Ok(hub.store.kv_get(PENDING, chat).await?)
}

/// The report for a pending handover arrived: it is no longer pending.
pub async fn done(hub: &Hub, chat: &str) -> Result<()> {
    Ok(hub.store.kv_delete(PENDING, chat).await?)
}

/// Continue `chat` on a fresh session seeded with `report`. Returns the new session file. The
/// running container is restarted once the current turn had time to end; the report is queued as
/// a `session.started` note only then, so it lands in the new session.
pub async fn rollover(hub: &Arc<Hub>, chat: &str, report: &HandoverReport) -> Result<String> {
    let inst = hub.store.instance(chat).await?;
    let file = format!("{chat}-{}.jsonl", chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f"));
    hub.store.set_session_file(chat, &file).await?;
    tracing::info!(chat, from = %inst.session_file, to = %file, "rolling over to a fresh session");
    let note = BuiltinPayload::SessionStarted(SessionStartedPayload {
        previous: inst.session_file,
        summary: report.summary.clone(),
        outstanding: report.outstanding.clone(),
        state: report.state.clone(),
    });
    let Some(agent) = hub.agents.get(chat).await else {
        crate::router::note(hub, chat, note).await?;
        return Ok(file);
    };
    let (hub, chat) = (hub.clone(), chat.to_string());
    tokio::spawn(async move {
        tokio::time::sleep(RESTART_AFTER).await;
        agent.stop().await;
        if let Err(e) = crate::router::note(&hub, &chat, note).await {
            tracing::error!(%chat, error = %e, "could not queue the handover report for the new session");
        }
    });
    Ok(file)
}

/// After a persistent agent's turn: ask it to roll over when its context or its compactions
/// passed the limits in `[defaults]`.
pub async fn check(hub: &Hub, agent: &LiveAgent) -> Result<()> {
    let inst = hub.store.instance(&agent.instance).await?;
    if !inst.persistent || pending(hub, &inst.name).await?.is_some() {
        return Ok(());
    }
    let d = hub.applied().await.defs.instance.defaults.clone();
    let mut reason = None;
    if d.rollover_tokens > 0 {
        let stats = agent.pi.session_stats().await?;
        let tokens = stats.pointer("/contextUsage/tokens").and_then(serde_json::Value::as_u64).unwrap_or(0);
        if tokens >= d.rollover_tokens {
            reason = Some(format!("your context holds {tokens} tokens (limit {})", d.rollover_tokens));
        }
    }
    if reason.is_none() && d.rollover_compactions > 0 {
        let path = hub.paths.sessions(&inst.name).join(&inst.session_file);
        let compactions = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text.lines().filter(|l| l.contains("\"type\":\"compaction\"")).count(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        if compactions >= d.rollover_compactions as usize {
            reason = Some(format!("this session was compacted {compactions} times"));
        }
    }
    if let Some(reason) = reason {
        request(hub, &inst.name, SELF, Some(reason)).await?;
    }
    Ok(())
}
