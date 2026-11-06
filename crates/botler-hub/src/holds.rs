//! Provider holds. A model provider that refuses every call (the key rejected, out of credits, a
//! budget spent, down for hours) is one problem, not one per chat. The first chat that runs into
//! it trips `provider:<name>`: chats on that provider then wait, their failed turn kept and their
//! deliveries queued, instead of failing one by one, and the user gets one request. The hub
//! probes the provider with backoff; once it answers (or the user answers `retry`, or clears the
//! breaker) every waiting chat continues where it stopped.

use std::sync::Arc;

use botler_core::FRONT_DOOR;
use botler_core::defs::{ModelChoice, OPENROUTER};
use botler_store::{Breaker, Dialog, DialogKind, DialogStatus, NewDialog, Priority};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::hub::{Hub, HubError, Result};
use crate::phase::AgentStatus;

/// The kind of a provider breaker, and of its incident.
pub const KIND: &str = "provider";
const PREFIX: &str = "provider:";
/// kv: waiting chat → the provider it waits for.
const WAITING: &str = "hub:provider-wait";
/// kv: hold subject → its [`Probe`].
const PROBES: &str = "hub:provider-probe";
/// Minutes between probes, by attempt; the last repeats.
const BACKOFF_MINUTES: [i64; 5] = [5, 10, 20, 40, 60];

pub fn subject(provider: &str) -> String {
    format!("{PREFIX}{provider}")
}

/// When the next probe of a hold goes out, and with which model.
#[derive(Serialize, Deserialize)]
struct Probe {
    model: String,
    attempt: usize,
    next_at: DateTime<Utc>,
}

fn json_err(e: serde_json::Error) -> HubError {
    HubError::Io(std::io::Error::other(e))
}

/// The model `instance` runs on.
pub async fn model_of(hub: &Hub, instance: &str) -> Result<Option<ModelChoice>> {
    let inst = hub.store.instance(instance).await?;
    let applied = hub.applied().await;
    Ok(applied.defs.agents.get(&inst.definition).and_then(|d| applied.defs.model_for(&d.spec)))
}

/// The hold on the provider `instance` runs on, if there is one.
pub async fn held(hub: &Hub, instance: &str) -> Result<Option<Breaker>> {
    let Some(m) = model_of(hub, instance).await? else { return Ok(None) };
    Ok(hub.store.breaker(&subject(&m.provider)).await?)
}

/// Whether `instance`'s provider is held; it then waits for the hold to lift.
pub async fn waits(hub: &Hub, instance: &str) -> Result<bool> {
    let Some(m) = model_of(hub, instance).await? else { return Ok(false) };
    if hub.store.breaker(&subject(&m.provider)).await?.is_none() {
        return Ok(false);
    }
    hub.store.kv_set(WAITING, instance, &m.provider).await?;
    Ok(true)
}

/// The provider a request of the hub's is about, if it is a hold's.
pub fn hold_of(d: &Dialog) -> Option<&str> {
    d.about.iter().find_map(|a| a.strip_prefix(PREFIX))
}

/// `instance`'s turn failed because its provider refuses calls: it waits for the provider, its
/// failed turn kept for when the hold lifts.
pub async fn hold(hub: &Hub, instance: &str, choice: &ModelChoice, reason: &str) -> Result<()> {
    let provider = &choice.provider;
    let subject = subject(provider);
    hub.store.kv_set(WAITING, instance, provider).await?;
    crate::agents::broadcast_state(
        hub,
        instance,
        AgentStatus::Paused { reason: format!("waiting for {provider} to accept calls again") },
    );
    if !hub.store.trip_breaker(&subject, KIND, reason).await? {
        return Ok(());
    }
    tracing::error!(provider, %reason, "the provider refuses calls; chats on it wait");
    hub.store.record_failure(&subject, KIND, None).await?;
    let probe =
        Probe { model: choice.id.clone(), attempt: 0, next_at: Utc::now() + Duration::minutes(BACKOFF_MINUTES[0]) };
    hub.store.kv_set(PROBES, &subject, &serde_json::to_string(&probe).map_err(json_err)?).await?;
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: FRONT_DOOR.into(),
            kind: DialogKind::Question,
            question: format!("{provider} stopped accepting model calls"),
            detail: Some(format!(
                "```\n{reason}\n```\nChats that use it wait and continue by themselves once it answers again (the hub \
                 checks every few minutes). Fixed it already? Answer `retry` to continue now."
            )),
            ctas: vec!["retry".into()],
            about: vec![subject],
            priority: Priority::Blocking,
            proposal_id: None,
            spec: None,
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    hub.schedule_notify.notify_one();
    Ok(())
}

/// `provider` takes calls again: lift its hold, and every chat waiting on it continues. Returns
/// whether it was held.
pub async fn release(hub: &Hub, provider: &str, why: &str) -> Result<bool> {
    let subject = subject(provider);
    let held = hub.store.clear_breaker(&subject).await?;
    hub.store.kv_delete(PROBES, &subject).await?;
    for d in hub.store.open_dialogs().await? {
        if hold_of(&d) == Some(provider) && hub.store.close_dialog(&d.id, DialogStatus::Aborted, why).await? {
            crate::notify::dialog_closed(hub, &d);
        }
    }
    for i in hub.store.open_incidents().await? {
        if i.runnable == subject {
            hub.store.resolve_incident(&i.id, why).await?;
        }
    }
    let now = Utc::now();
    let mut resumed = 0;
    for (instance, p) in hub.store.kv_list(WAITING).await? {
        if p != provider {
            continue;
        }
        hub.store.kv_delete(WAITING, &instance).await?;
        if hub.store.turn_retry(&instance).await?.is_some() {
            hub.store.schedule_turn_retry(&instance, now).await?;
            resumed += 1;
        }
    }
    tracing::info!(provider, held, resumed, why, "provider hold lifted");
    hub.schedule_notify.notify_one();
    hub.dispatch_notify.notify_one();
    Ok(held)
}

/// Probe the held providers that are due, each in a task of its own (a probe takes up to 30 s).
/// Returns when the next probe is due.
pub async fn sweep(hub: &Arc<Hub>, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let mut next: Option<DateTime<Utc>> = None;
    for (subject, value) in hub.store.kv_list(PROBES).await? {
        let Some(provider) = subject.strip_prefix(PREFIX).map(str::to_owned) else { continue };
        let mut probe: Probe = serde_json::from_str(&value).map_err(json_err)?;
        if probe.next_at > now {
            next = Some(next.map_or(probe.next_at, |n| n.min(probe.next_at)));
            continue;
        }
        probe.attempt += 1;
        let wait = BACKOFF_MINUTES[probe.attempt.min(BACKOFF_MINUTES.len() - 1)];
        probe.next_at = now + Duration::minutes(wait);
        hub.store.kv_set(PROBES, &subject, &serde_json::to_string(&probe).map_err(json_err)?).await?;
        next = Some(next.map_or(probe.next_at, |n| n.min(probe.next_at)));
        let choice = hub.applied().await.defs.instance.models.resolve(&probe.model);
        if choice.provider != provider || provider != OPENROUTER && choice.base_url.is_none() {
            // A deprecated route to another provider: only the user lifts it.
            continue;
        }
        let hub = hub.clone();
        let attempt = probe.attempt;
        tokio::spawn(async move {
            match crate::gateway::probe(&hub, &choice).await {
                Ok(()) => {
                    if let Err(e) = release(&hub, &provider, "the provider answered again").await {
                        tracing::error!(provider, error = %e, "could not lift the provider hold");
                    }
                }
                Err(e) => tracing::info!(provider, attempt, error = %e, "the provider still refuses calls"),
            }
        });
    }
    Ok(next)
}
