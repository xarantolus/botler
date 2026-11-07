//! Failures of runnables become incidents, and incidents go to an agent.

use botler_core::Source;
use botler_core::payloads::{BuiltinPayload, RunnableFailedPayload};
use botler_core::routing::{Action, Dispatch};

use botler_store::{InstanceState, ProposalStatus};
use chrono::{Duration, Utc};

use crate::hub::{Hub, Result};
use crate::router;

/// kv: runnable → the chat spawned for its failures, which gets its later ones too.
const ON_IT: &str = "hub:incident-chat";
/// A runnable failing this soon after a merged proposal changed it goes to the proposing chat.
const AFTER_MERGE_HOURS: i64 = 2;

/// What went wrong with a runnable.
#[derive(Clone, Debug)]
pub struct Failure {
    pub runnable: String,
    /// exit | timeout | oom | unhealthy | crashloop | start | build (an image; `runnable` is its Dockerfile)
    /// | interrupted (an agent's turn the hub kept stopping during)
    pub kind: &'static str,
    pub exit_code: Option<i64>,
    pub run_id: Option<String>,
    pub logs_tail: String,
    pub commit: Option<String>,
    pub event_id: Option<String>,
}

/// Record the failure. A new incident (or a crash loop) is routed to the runnable's `on_failure`
/// target and its subscribers; repeats of an open incident only bump its count.
pub async fn report(hub: &Hub, f: Failure) -> Result<()> {
    let (incident, is_new) = hub.store.record_failure(&f.runnable, f.kind, f.run_id.as_deref()).await?;
    tracing::warn!(runnable = %f.runnable, kind = f.kind, count = incident.count, "runnable failed");
    if !is_new && f.kind != "crashloop" {
        return Ok(());
    }
    let payload = BuiltinPayload::RunnableFailed(RunnableFailedPayload {
        runnable: f.runnable.clone(),
        kind: f.kind.into(),
        exit_code: f.exit_code,
        run_id: f.run_id.clone(),
        incident_id: incident.id.clone(),
        count: incident.count,
        logs_tail: f.logs_tail.clone(),
        commit: f.commit.clone(),
        event_id: f.event_id.clone(),
    });
    let ev = crate::hub::builtin(payload, Source::Hub)?;
    let applied = hub.applied().await;
    let target = applied.defs.failure_target(&f.runnable);
    // Without a triage agent the failure still reaches the user through the front door.
    let action = match Action::from(&target) {
        Action::Spawn(d) if !applied.defs.agents.contains_key(&d) => Action::Deliver(botler_core::FRONT_DOOR.into()),
        // A turn the hub kept stopping during is nothing a code change in the monorepo fixes: the
        // user (through the front door) decides, no triage chat. One about the front door itself
        // stays an open incident, not a message that would wake it into the same turn.
        _ if f.kind == "interrupted" => {
            if f.runnable == format!("agent:{}", botler_core::FRONT_DOOR) {
                return router::dispatch(hub, ev, Vec::new()).await;
            }
            Action::Deliver(botler_core::FRONT_DOOR.into())
        }
        a => a,
    };
    let prompt = format!("Job `{}` failed ({}). Investigate and resolve the incident.", f.runnable, f.kind);
    // A chat already on it gets the failure instead of a new one: the chat whose merged change
    // just broke it, or the one that handled its last failure.
    let action = match action {
        Action::Spawn(def) => match on_it(hub, &f.runnable).await? {
            Some(chat) => Action::Deliver(chat),
            None => {
                let chat = router::new_instance(hub, &def, None, Some(&prompt)).await?;
                hub.store.kv_set(ON_IT, &f.runnable, &chat).await?;
                Action::Deliver(chat)
            }
        },
        a => a,
    };
    let d = Dispatch { trigger: None, action, prompt: Some(prompt), shadow: false, quiet: false };
    router::dispatch(hub, ev, vec![d]).await
}

/// The chat that should hear about `runnable`'s failure, if one is on it: whoever's proposal
/// changing it was merged in the last two hours, else the chat its last failure went to, while
/// neither is archived.
async fn on_it(hub: &Hub, runnable: &str) -> Result<Option<String>> {
    let since = Utc::now() - Duration::hours(AFTER_MERGE_HOURS);
    let mut chats = Vec::new();
    for p in hub.store.recent_proposals(50).await? {
        let changed = p.summary.as_ref().is_some_and(|s| s.items.iter().any(|i| i.name == runnable));
        if p.status == ProposalStatus::Merged && p.updated_at >= since && changed {
            chats.push(p.instance);
        }
    }
    chats.extend(hub.store.kv_get(ON_IT, runnable).await?);
    for chat in chats {
        if hub.store.find_instance(&chat).await?.is_some_and(|i| i.state != InstanceState::Archived) {
            return Ok(Some(chat));
        }
    }
    Ok(None)
}

/// A service up this long since its last failure has recovered.
const RECOVERED_AFTER_MINUTES: i64 = 10;

/// Resolve the incidents that went away by themselves: a service running again (and
/// heartbeating, if it has `[health]`) for ten minutes since it last failed, a job whose next run
/// succeeded. A crash loop is not among them (the hub keeps such a service stopped until its
/// incident is resolved), nor builds, held providers and interrupted turns. The chat working on
/// it gets a note.
pub async fn recover(hub: &Hub, now: chrono::DateTime<Utc>) -> Result<()> {
    for incident in hub.store.open_incidents().await? {
        if !matches!(incident.kind.as_str(), "exit" | "timeout" | "oom" | "unhealthy" | "start") {
            continue;
        }
        let Some(run) = hub.store.runs_for(&incident.runnable, 1).await?.into_iter().next() else { continue };
        let Some(started) = run.started_at.filter(|t| *t > incident.last_at) else { continue };
        let health = hub.applied().await.defs.jobs.get(&incident.runnable).is_some_and(|j| j.spec.health.is_some());
        let how = match (run.kind, run.status) {
            (botler_store::RunKind::Service, botler_store::RunStatus::Running)
                if now - started >= Duration::minutes(RECOVERED_AFTER_MINUTES)
                    && (!health || run.heartbeat_at.is_some_and(|h| h > incident.last_at)) =>
            {
                format!("Recovered by itself: the service has been running again since {started} without failing.")
            }
            (botler_store::RunKind::Oneshot, botler_store::RunStatus::Succeeded) => {
                format!("Recovered by itself: its next run ({}) succeeded.", run.id)
            }
            _ => continue,
        };
        hub.store.resolve_incident(&incident.id, &how).await?;
        tracing::info!(runnable = %incident.runnable, incident = %incident.id, "incident recovered by itself");
        if let Some(chat) = hub.store.kv_get(ON_IT, &incident.runnable).await?
            && hub.store.find_instance(&chat).await?.is_some_and(|i| i.state != InstanceState::Archived)
        {
            let text = format!("Incident {} (`{}`) is resolved: {how}", incident.id, incident.runnable);
            let ev = crate::hub::builtin(
                BuiltinPayload::Message(botler_core::payloads::MessagePayload {
                    text,
                    files: vec![],
                    mode: None,
                    via: None,
                }),
                Source::Hub,
            )?;
            let d = Dispatch { trigger: None, action: Action::Deliver(chat), prompt: None, shadow: false, quiet: true };
            router::dispatch(hub, ev, vec![d]).await?;
        }
    }
    Ok(())
}

/// The last `lines` lines of a log.
pub fn tail(log: &str, lines: usize) -> String {
    let all: Vec<&str> = log.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
