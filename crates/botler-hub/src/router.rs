//! The router: turns journaled events into deliveries, spawns and job runs.
//!
//! Events are journaled first and marked routed last, so a crash in between re-routes the event
//! on restart (at-least-once). Duplicate deliveries are dropped by the pi extension.

use botler_core::catalog::touches;
use botler_core::defs::{AgentKind, Target};
use botler_core::payloads::{BreakerKind, BreakerTrippedPayload, BuiltinPayload, DialogAbortedPayload, MessagePayload};
use botler_core::routing::{Action, Dispatch, Routed, route};
use botler_core::{Event, Source};
use botler_store::{DialogStatus, Instance, InstanceState, NewInstance};

use crate::hub::{Hub, HubError, Result, builtin};
use crate::loops::AgentDelivery;

/// Route every unrouted event. Returns how many were processed.
pub async fn route_pending(hub: &Hub) -> Result<usize> {
    if hub.paused().await? {
        return Ok(0);
    }
    let events = hub.store.unrouted_events(100).await?;
    let n = events.len();
    for ev in events {
        if let Err(e) = route_one(hub, &ev).await {
            // One bad event must not block the queue; it is marked routed and reported.
            tracing::error!(event = %ev.id, kind = %ev.kind, error = %e, "routing failed");
            report_failure(hub, &ev, &e).await?;
        }
        hub.store.mark_routed(&ev.id.to_string()).await?;
    }
    Ok(n)
}

async fn route_one(hub: &Hub, ev: &Event) -> Result<()> {
    abort_dialogs(hub, ev).await?;
    if let Some(corr) = ev.correlation
        && hub.store.breaker(&format!("chain:{corr}")).await?.is_some()
    {
        tracing::info!(event = %ev.id, "dropping event on a tripped conversation chain");
        return Ok(());
    }
    let applied = hub.applied().await;
    let subs = hub.store.active_subscriptions().await?;
    match route(&applied.defs, ev, &subs) {
        Routed::CycleBreaker { hops } => trip_cycle_breaker(hub, ev, hops).await,
        Routed::Dispatches(ds) => {
            for d in ds {
                if d.shadow {
                    tracing::info!(trigger = ?d.trigger, action = ?d.action, "shadow trigger matched; not executing");
                    continue;
                }
                execute(hub, &d, ev).await?;
            }
            Ok(())
        }
    }
}

/// The one chat of persistent agent `name`, created (asleep) now.
async fn persistent_chat(hub: &Hub, name: &str) -> Result<Instance> {
    Ok(hub
        .store
        .create_instance(&NewInstance {
            name: name.into(),
            definition: name.into(),
            persistent: true,
            session_file: format!("{name}.jsonl"),
            branch: None,
            parent: None,
            depth: 0,
            task: None,
        })
        .await?)
}

/// Every persistent agent has its chat from when it is defined, so the user can open it before
/// anything was delivered to it. It sleeps until something is.
pub async fn create_persistent_chats(hub: &Hub) -> Result<()> {
    let applied = hub.applied().await;
    for (name, def) in &applied.defs.agents {
        if def.spec.kind == AgentKind::Persistent && hub.store.resolve_instance(name).await?.is_none() {
            persistent_chat(hub, name).await?;
            tracing::info!(chat = %name, "created the persistent agent's chat");
        }
    }
    Ok(())
}

/// Carry out one dispatch for `ev` (which is already journaled).
pub async fn execute(hub: &Hub, d: &Dispatch, ev: &Event) -> Result<()> {
    let applied = hub.applied().await;
    match &d.action {
        Action::Deliver(name) => {
            let instance = match hub.store.resolve_instance(name).await? {
                Some(i) => i,
                None => match applied.defs.agents.get(name) {
                    Some(def) if def.spec.kind == AgentKind::Persistent => persistent_chat(hub, name).await?,
                    _ => return Err(HubError::Undefined { kind: "instance", name: name.clone() }),
                },
            };
            // Between agents, the loop defences decide: a note, a held message, or a normal one.
            let (quiet, held) = match &ev.source {
                Source::Agent(from) if *from != instance.name => {
                    match crate::loops::agent_delivery(hub, ev, from, &instance.name).await? {
                        AgentDelivery::Deliver { quiet } => (quiet || d.quiet, None),
                        AgentDelivery::Hold { subject } => (false, Some(subject)),
                    }
                }
                _ => (d.quiet, None),
            };
            // Messaging an archived chat restores it: archiving hides, it never locks away. A
            // note or a held message does not.
            let mut held = held;
            if instance.state == InstanceState::Archived && !quiet && held.is_none() {
                tracing::info!(instance = %instance.name, "restoring archived chat for a new message");
                match crate::restore::restore(hub, &instance.name, false).await {
                    Ok(_) => {}
                    // Held until the user restores it as a generic chat (or deletes it).
                    Err(e @ HubError::DefinitionGone { .. }) => {
                        tracing::warn!(error = %e, "cannot restore for a new message; holding it");
                        held = Some(crate::restore::held_subject(&instance.name));
                    }
                    Err(e) => return Err(e),
                }
            }
            if let Some(subject) = &held {
                tracing::info!(event = %ev.id, %subject, "holding a message on a paused pair");
            }
            hub.store
                .enqueue_delivery_with(
                    &ev.id.to_string(),
                    &instance.name,
                    d.trigger.as_deref(),
                    d.prompt.as_deref(),
                    quiet,
                    held.as_deref(),
                )
                .await?;
            hub.dispatch_notify.notify_one();
        }
        Action::Spawn(def) => {
            let parent = match &ev.source {
                Source::Agent(a) => Some(a.clone()),
                _ => None,
            };
            spawn(hub, def, parent.as_deref(), d.prompt.as_deref(), Some(ev), d.trigger.as_deref()).await?;
        }
        Action::Run(job) => {
            // Boxed: a failing start reports a failure, which may dispatch again. That recursion
            // is bounded because repeats of an open incident do not dispatch.
            Box::pin(crate::jobs::start(hub, job, d.trigger.as_deref(), Some(ev))).await?;
        }
    }
    Ok(())
}

/// Create a new (ephemeral) instance of an agent definition, without delivering anything.
pub async fn new_instance(hub: &Hub, definition: &str, parent: Option<&str>, task: Option<&str>) -> Result<String> {
    let applied = hub.applied().await;
    if !applied.defs.agents.contains_key(definition) {
        return Err(HubError::Undefined { kind: "agent", name: definition.into() });
    }
    let depth = match parent {
        Some(p) => match hub.store.find_instance(p).await? {
            Some(parent) => parent.depth + 1,
            // A parent that is not an instance (e.g. a hub-side caller name) starts a new tree.
            None => 0,
        },
        None => 0,
    };
    let max = applied.defs.instance.defaults.max_spawn_depth;
    if depth > max {
        return Err(HubError::SpawnTooDeep { parent: parent.unwrap_or_default().into(), depth: depth - 1 });
    }
    let suffix = &uuid::Uuid::now_v7().simple().to_string()[20..];
    let name = format!("{definition}-{suffix}");
    hub.store
        .create_instance(&NewInstance {
            name: name.clone(),
            definition: definition.into(),
            persistent: false,
            session_file: format!("{name}.jsonl"),
            branch: None,
            parent: parent.map(String::from),
            depth,
            task: task.map(String::from),
        })
        .await?;
    tracing::info!(instance = %name, definition, "created agent instance");
    Ok(name)
}

/// Create a new instance of an agent definition and queue its first delivery.
pub async fn spawn(
    hub: &Hub,
    definition: &str,
    parent: Option<&str>,
    task: Option<&str>,
    event: Option<&Event>,
    trigger: Option<&str>,
) -> Result<String> {
    let name = new_instance(hub, definition, parent, task).await?;
    send_task(hub, &name, parent, task, event, trigger).await?;
    Ok(name)
}

/// Queue a new instance's first delivery: `event`, or `task` as a message from `parent`.
pub async fn send_task(
    hub: &Hub,
    name: &str,
    parent: Option<&str>,
    task: Option<&str>,
    event: Option<&Event>,
    trigger: Option<&str>,
) -> Result<()> {
    let ev = match event {
        Some(e) => e.clone(),
        None => {
            let task = BuiltinPayload::Message(MessagePayload {
                text: task.unwrap_or("").into(),
                files: vec![],
                mode: None,
                via: None,
            });
            let e = builtin(task, parent.map_or(Source::Hub, |p| Source::Agent(p.into())))?.to(name);
            hub.store.append_event(&e).await?;
            hub.store.mark_routed(&e.id.to_string()).await?;
            e
        }
    };
    let prompt = if event.is_some() { task } else { None };
    hub.store.enqueue_delivery(&ev.id.to_string(), name, trigger, prompt).await?;
    hub.dispatch_notify.notify_one();
    Ok(())
}

/// Journal a built-in event for `to` and queue it as a quiet note: added to its session without a
/// turn, read with whatever wakes it next.
pub async fn note(hub: &Hub, to: &str, payload: BuiltinPayload) -> Result<()> {
    note_event(hub, to, builtin(payload, Source::Hub)?.to(to)).await
}

/// [`note`] for an event already built (its own source and thread).
pub async fn note_event(hub: &Hub, to: &str, ev: Event) -> Result<()> {
    hub.store.append_event(&ev).await?;
    hub.store.mark_routed(&ev.id.to_string()).await?;
    hub.store.enqueue_delivery_with(&ev.id.to_string(), to, None, None, true, None).await?;
    if hub.live.send(crate::hub::Live::Event { event: ev }).is_err() {
        tracing::trace!("no live viewers");
    }
    hub.dispatch_notify.notify_one();
    Ok(())
}

/// Fire a trigger's target directly (schedules and timers).
pub async fn fire(hub: &Hub, target: &Target, prompt: Option<&str>, trigger: Option<&str>, ev: Event) -> Result<()> {
    let d = Dispatch {
        trigger: trigger.map(String::from),
        action: Action::from(target),
        prompt: prompt.map(String::from),
        shadow: false,
        quiet: false,
    };
    dispatch(hub, ev, vec![d]).await
}

/// Journal `ev`, then execute its subscriptions plus `extra` explicit dispatches (deduplicated by
/// action), and mark it routed. Failures are reported to the event's owner and returned.
pub async fn dispatch(hub: &Hub, ev: Event, extra: Vec<Dispatch>) -> Result<()> {
    hub.store.append_event(&ev).await?;
    if hub.live.send(crate::hub::Live::Event { event: ev.clone() }).is_err() {
        tracing::trace!("no live viewers");
    }
    let applied = hub.applied().await;
    let mut all = extra;
    let subs = hub.store.active_subscriptions().await?;
    match route(&applied.defs, &ev, &subs) {
        Routed::Dispatches(ds) => {
            for d in ds {
                if !all.iter().any(|x| x.action == d.action) {
                    all.push(d);
                }
            }
        }
        Routed::CycleBreaker { hops } => trip_cycle_breaker(hub, &ev, hops).await?,
    }
    let mut first_err = None;
    for d in all.iter().filter(|d| !d.shadow) {
        if let Err(e) = execute(hub, d, &ev).await {
            report_failure(hub, &ev, &e).await?;
            first_err.get_or_insert(e);
        }
    }
    hub.store.mark_routed(&ev.id.to_string()).await?;
    first_err.map_or(Ok(()), Err)
}

async fn abort_dialogs(hub: &Hub, ev: &Event) -> Result<()> {
    let targets = touches(ev);
    if targets.is_empty() {
        return Ok(());
    }
    for d in hub.store.open_dialogs().await? {
        // An event answering or closing the dialog itself does not abort it.
        if ev.field("dialog_id").and_then(|v| v.as_str()) == Some(d.id.as_str()) {
            continue;
        }
        if let Some(hit) = d.about.iter().find(|a| targets.contains(a)) {
            let reason = format!("{} changed ({})", hit, ev.kind);
            if hub.store.close_dialog(&d.id, DialogStatus::Aborted, &reason).await? {
                let payload = BuiltinPayload::DialogAborted(DialogAbortedPayload {
                    dialog_id: d.id.clone(),
                    question: d.question.clone(),
                    reason,
                });
                let abort = builtin(payload, Source::Hub)?.to(d.instance.clone());
                hub.emit(abort).await?;
            }
        }
    }
    Ok(())
}

async fn trip_cycle_breaker(hub: &Hub, ev: &Event, hops: u32) -> Result<()> {
    let chain = ev.correlation.unwrap_or(ev.id);
    let subject = format!("chain:{chain}");
    let reason = format!("conversation chain reached {hops} hops (last: {} from {})", ev.kind, ev.source.key());
    if hub.store.trip_breaker(&subject, "cycle", &reason).await? {
        tracing::warn!(%subject, %reason, "cycle breaker tripped");
        let payload = BreakerTrippedPayload {
            subject: subject.clone(),
            breaker: BreakerKind::Cycle,
            reason,
            correlation: Some(chain.to_string()),
            pair: None,
            recent: Vec::new(),
        };
        let alarm =
            builtin(BuiltinPayload::BreakerTripped(payload), Source::Hub)?.to(botler_core::FRONT_DOOR).urgent(true);
        hub.emit(alarm).await?;
    }
    Ok(())
}

/// Tell whoever caused an event that it could not be routed.
async fn report_failure(hub: &Hub, ev: &Event, err: &HubError) -> Result<()> {
    let to = match &ev.source {
        Source::Agent(a) => a.clone(),
        // Failures of hub-originated events go to the front door, unless it is the problem.
        _ if ev.to.as_deref() != Some(botler_core::FRONT_DOOR) => botler_core::FRONT_DOOR.into(),
        _ => return Ok(()),
    };
    let text = format!("Could not route event {} ({}): {err}", ev.id, ev.kind);
    let note =
        builtin(BuiltinPayload::Message(MessagePayload { text, files: vec![], mode: None, via: None }), Source::Hub)?
            .to(to);
    hub.emit(note).await?;
    Ok(())
}
