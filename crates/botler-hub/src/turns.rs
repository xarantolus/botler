//! Turns that end in a model error, and answers that do not come.
//!
//! pi retries a failing model call a few times within seconds. When the turn still ends in an
//! error, the hub classifies it ([`botler_core::model_error`]): transient errors are retried later
//! with backoff, a context overflow is compacted and retried once, and everything else (or a
//! transient error after the last retry) is given up on: an incident, a failed status, and an
//! `agent.turn_failed` to whoever waits on the chat. Separately, `reply.overdue` tells an agent
//! that someone it expects an answer from has gone quiet. Both are kept in SQLite and swept by the
//! scheduler, so a restart loses neither.
//!
//! A retry is a new message from the hub ("the previous attempt failed ...; continue"), not the
//! failed delivery again: the session already holds the original input, and the extension drops
//! a delivery id it has seen, so re-sending it would be a no-op.

use std::sync::Arc;

use botler_core::model_error::{ErrorClass, classify, retry_delay};
use botler_core::payloads::{AgentTurnFailedPayload, BuiltinPayload, MessagePayload, ReplyOverduePayload};
use botler_core::{FRONT_DOOR, Source};
use botler_store::{Dialog, DialogKind, DialogStatus, NewDialog, Priority, ReplyExpectation, TurnRetry};
use chrono::{DateTime, Utc};

use crate::agents::{self, LiveAgent};
use crate::holds;
use crate::hub::{Hub, Result, builtin};
use crate::phase::{AgentStatus, WakePhase};

/// The sender of expectations the hub holds itself (the reviewer's verdict).
pub const HUB: &str = "hub";
/// How much of a provider error is kept and shown.
const ERROR_CHARS: usize = 300;

fn clip(s: &str) -> String {
    s.trim().chars().take(ERROR_CHARS).collect()
}

/// `agent`'s turn ended in `error` (after pi's own retries) while `called` (the model and
/// provider), which is kept with the error: "Connection error." alone says nothing.
pub async fn failed(hub: &Arc<Hub>, agent: &LiveAgent, error: &str, called: Option<&str>) -> Result<()> {
    let class = classify(error);
    let started = agents::turn_start(hub, agent).await?.map(|d| d.event_id);
    let error = match called {
        Some(c) => format!("{} ({c})", clip(error)),
        None => clip(error),
    };
    let retry = hub.store.record_turn_failure(&agent.instance, started.as_deref(), &error).await?;
    tracing::warn!(instance = %agent.instance, ?class, attempts = retry.attempts, %error, "turn failed");
    let now = Utc::now();
    let reason = format!("{}: {}", class.describe(), retry.error);
    match class {
        ErrorClass::Transient => {
            if let Some(delay) = retry_delay(retry.attempts) {
                let at = now + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::MAX);
                return schedule(hub, &agent.instance, at, &retry.error).await;
            }
            // Out of retries: the provider's problem when it refuses a plain call too, and then
            // every chat on it waits for it instead of giving up one by one.
            if let Some(choice) = holds::model_of(hub, &agent.instance).await?
                && let Err(e) = crate::gateway::probe(hub, &choice).await
            {
                return holds::hold(hub, &agent.instance, &choice, &format!("{reason} (a check call failed too: {e})"))
                    .await;
            }
        }
        // Compacting frees context; the same failure after that is final.
        ErrorClass::ContextLength if retry.attempts == 1 => match agent.pi.compact().await {
            Ok(()) => return schedule(hub, &agent.instance, now, &retry.error).await,
            Err(e) => {
                let error = format!("{} (compacting failed: {e})", retry.error);
                return give_up(hub, &agent.instance, class, &error, &retry).await;
            }
        },
        // The key or the account, not this chat: it waits with every other chat on the provider.
        ErrorClass::Auth | ErrorClass::Billing => {
            if let Some(choice) = holds::model_of(hub, &agent.instance).await? {
                return holds::hold(hub, &agent.instance, &choice, &reason).await;
            }
        }
        ErrorClass::ContextLength | ErrorClass::Permanent => {}
    }
    give_up(hub, &agent.instance, class, &retry.error, &retry).await
}

/// `instance` finished a turn normally: any retries of an earlier failure are over.
pub async fn succeeded(hub: &Hub, instance: &str) -> Result<()> {
    if hub.store.clear_turn_retry(instance).await? {
        tracing::info!(instance, "turn succeeded after retrying");
    }
    close_retry_requests(hub, instance, "the chat works again").await
}

/// The hub's "`<chat>` stopped" request, answered with `retry`.
fn is_retry_request(d: &Dialog, instance: &str) -> bool {
    d.instance == instance
        && d.kind == DialogKind::Question
        && d.ctas == ["retry"]
        && d.question.contains("` stopped: ")
}

/// Close a chat's open retry requests: a chat has at most one, about its latest failure.
async fn close_retry_requests(hub: &Hub, instance: &str, why: &str) -> Result<()> {
    for d in hub.store.open_dialogs().await?.into_iter().filter(|d| is_retry_request(d, instance)) {
        if hub.store.close_dialog(&d.id, DialogStatus::Aborted, why).await? {
            crate::notify::dialog_closed(hub, &d);
        }
    }
    Ok(())
}

async fn schedule(hub: &Hub, instance: &str, at: DateTime<Utc>, error: &str) -> Result<()> {
    hub.store.schedule_turn_retry(instance, at).await?;
    agents::show_phase(hub, instance, WakePhase::Retrying { at, error: error.into() }).await;
    hub.schedule_notify.notify_one();
    Ok(())
}

/// Stop retrying: open an incident, show the chat as failed, and tell whoever waits on it.
async fn give_up(hub: &Arc<Hub>, instance: &str, class: ErrorClass, error: &str, retry: &TurnRetry) -> Result<()> {
    hub.store.clear_turn_retry(instance).await?;
    let (incident, _) = hub.store.record_failure(&format!("agent:{instance}"), "model", None).await?;
    let reason = format!("{}: {error}", class.describe());
    tracing::error!(instance, incident = %incident.id, %reason, "gave up on the turn");
    match class {
        ErrorClass::Auth | ErrorClass::Billing => {
            agents::broadcast_state(hub, instance, AgentStatus::Paused { reason: reason.clone() })
        }
        _ => agents::show_phase(hub, instance, WakePhase::Failed { error: reason.clone() }).await,
    }

    let inst = hub.store.instance(instance).await?;
    let trigger = match &retry.event_id {
        Some(id) => Some(hub.store.event(id).await?),
        None => None,
    };
    // Waiting on this chat: its parent, and the agent whose message started the turn.
    let mut waiters: Vec<String> = inst.parent.iter().cloned().collect();
    if let Some(Source::Agent(a)) = trigger.as_ref().map(|t| &t.source)
        && a != instance
        && !waiters.contains(a)
    {
        waiters.push(a.clone());
    }
    let payload = AgentTurnFailedPayload {
        instance: instance.into(),
        class,
        error: error.into(),
        attempts: retry.attempts,
        event_id: retry.event_id.clone(),
    };
    let event = |to: Option<&String>| -> Result<botler_core::Event> {
        let mut ev = builtin(BuiltinPayload::AgentTurnFailed(payload.clone()), Source::Agent(instance.into()))?;
        if let Some(t) = &trigger {
            ev = ev.in_reply_to(t);
        }
        Ok(match to {
            Some(to) => ev.to(to.clone()),
            None => ev,
        })
    };
    if waiters.is_empty() {
        // Journaled for subscribers and the web app even when no agent waits.
        hub.emit(event(None)?).await?;
    }
    for w in &waiters {
        hub.emit(event(Some(w))?).await?;
    }
    hub.store.clear_expectations(None, instance).await?;

    let user_waits = trigger.as_ref().is_some_and(|t| t.source == Source::User) || instance == FRONT_DOOR;
    if user_waits || matches!(class, ErrorClass::Auth | ErrorClass::Billing) {
        tell_user(hub, &inst, class, error, retry.attempts).await?;
    }
    crate::transactions::reviewer_unavailable(hub, instance, &reason).await
}

/// Put the failure in the user's inbox; answering `retry` wakes the chat to try again.
async fn tell_user(
    hub: &Hub,
    inst: &botler_store::Instance,
    class: ErrorClass,
    error: &str,
    attempts: u32,
) -> Result<()> {
    let name = inst.title.clone().unwrap_or_else(|| inst.name.clone());
    let advice = match class {
        ErrorClass::Billing => {
            "The model provider says the account is out of credits. Add credits (OpenRouter: \
             https://openrouter.ai/settings/credits), clear this chat's breaker, then answer `retry`."
        }
        ErrorClass::Auth => {
            "The model provider rejected the API key. Fix the key in Secrets, clear this chat's breaker, \
             then answer `retry`."
        }
        _ => "Answer `retry` to have it try again.",
    };
    let priority = match class {
        ErrorClass::Auth | ErrorClass::Billing => Priority::Blocking,
        _ => Priority::Normal,
    };
    close_retry_requests(hub, &inst.name, "replaced by the chat's latest failure").await?;
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: inst.name.clone(),
            kind: DialogKind::Question,
            question: format!("`{name}` stopped: {}", class.describe()),
            detail: Some(format!("```\n{error}\n```\nTried {attempts} time(s). {advice}")),
            ctas: vec!["retry".into()],
            about: vec![],
            priority,
            proposal_id: None,
            spec: None,
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(())
}

/// Send retries that are due and report overdue answers. Returns when the next one is due.
pub async fn sweep(hub: &Arc<Hub>, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    for r in hub.store.take_due_retries(now).await? {
        if holds::waits(hub, &r.instance).await? {
            continue; // its provider was held again meanwhile
        }
        let mut text = format!("The previous attempt failed ({}); trying again. Continue where you left off.", r.error);
        if hub.store.instance(&r.instance).await?.archive_requested_at.is_some() {
            text.push_str(" You were asked to hand over before being archived: finish that with `handover_ready`.");
        }
        let mut ev = builtin(
            BuiltinPayload::Message(MessagePayload { text, files: vec![], mode: None, via: None }),
            Source::Hub,
        )?
        .to(r.instance.clone());
        if let Some(id) = &r.event_id {
            ev = ev.in_reply_to(&hub.store.event(id).await?);
        }
        tracing::info!(instance = %r.instance, attempt = r.attempts + 1, "retrying failed turn");
        hub.emit(ev).await?;
    }
    for x in hub.store.take_overdue(now).await? {
        overdue(hub, &x).await?;
    }
    Ok(hub.store.next_wait_at().await?)
}

async fn overdue(hub: &Arc<Hub>, x: &ReplyExpectation) -> Result<()> {
    tracing::info!(sender = %x.sender, recipient = %x.recipient, "answer overdue");
    let payload = ReplyOverduePayload { instance: x.recipient.clone(), event_id: x.event_id.clone(), due_at: x.due_at };
    let mut ev = builtin(BuiltinPayload::ReplyOverdue(payload), Source::Hub)?;
    if let Some(id) = &x.event_id {
        ev = ev.in_reply_to(&hub.store.event(id).await?);
    }
    // The hub's own waits are journaled for subscribers; agents get theirs delivered.
    if x.sender != HUB {
        ev = ev.to(x.sender.clone());
    }
    hub.emit(ev).await?;
    let reason = format!("no answer by {}", x.due_at.format("%Y-%m-%d %H:%M UTC"));
    crate::transactions::reviewer_unavailable(hub, &x.recipient, &reason).await
}

/// `sender` (an instance, or [`HUB`]) expects `recipient` to answer within the configured
/// `reply_within` (or `within`).
pub async fn expect_reply(
    hub: &Hub,
    sender: &str,
    recipient: &str,
    event_id: Option<&str>,
    within: Option<std::time::Duration>,
) -> Result<()> {
    let within = match within {
        Some(w) => w,
        None => hub.applied().await.defs.instance.defaults.reply_within.0,
    };
    let due = Utc::now() + chrono::Duration::from_std(within).unwrap_or(chrono::Duration::MAX);
    hub.store.expect_reply(sender, recipient, event_id, due).await?;
    hub.schedule_notify.notify_one();
    Ok(())
}
