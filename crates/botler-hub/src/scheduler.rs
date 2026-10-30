//! Schedules and timers. Stateless apart from `last_fired` per trigger and timer rows, so the
//! whole timetable is recomputed from definitions + store on every tick.

use std::time::Duration;

use botler_core::Source;
use botler_core::defs::{Mode, TriggerKind};
use botler_core::payloads::{BuiltinPayload, SchedulePayload, TimerPayload};
use botler_core::trigger::plan;
use botler_store::TimerStatus;
use chrono::{DateTime, Utc};

use crate::hub::{Hub, Result};
use crate::router;

/// Longest sleep between ticks, so clock jumps and missed notifications heal on their own.
pub const MAX_SLEEP: Duration = Duration::from_secs(60);

/// Fire everything due at `now`. Returns when the next thing is due.
pub async fn tick(hub: &Hub, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    if hub.paused().await? {
        // While paused nothing fires; on resume the `missed` policy decides what to catch up.
        return Ok(None);
    }
    let applied = hub.applied().await;
    let mut next: Option<DateTime<Utc>> = None;
    let mut keep = Vec::new();
    for t in applied.defs.triggers.values() {
        let TriggerKind::Schedule(spec) = &t.kind else { continue };
        keep.push(t.id.clone());
        let state = hub.store.schedule_state(&t.id, now).await?;
        let p = plan(spec, t.spec.missed, state.last_fired, state.anchor, now);
        if let Some(mark) = p.mark {
            if p.fire {
                if t.spec.mode == Mode::Shadow {
                    tracing::info!(trigger = %t.id, "shadow schedule due; not executing");
                } else {
                    let fired = SchedulePayload { trigger: t.id.clone(), scheduled_for: mark, skipped: p.skipped };
                    let ev = crate::hub::builtin(BuiltinPayload::Schedule(fired), Source::Trigger(t.id.clone()))?;
                    if let Err(e) = router::fire(hub, &t.target, t.spec.prompt.as_deref(), Some(&t.id), ev).await {
                        // Reported to the owner by `fire`; the schedule still advances so one
                        // broken target cannot make it fire in a tight loop.
                        tracing::warn!(trigger = %t.id, error = %e, "scheduled fire failed");
                    }
                }
            } else {
                tracing::info!(trigger = %t.id, skipped = p.skipped, "missed fires skipped (missed = skip)");
            }
            hub.store.record_schedule(&t.id, mark, p.skipped).await?;
        }
        next = earliest(next, p.next);
    }
    hub.store.prune_schedules(&keep).await?;

    for timer in hub.store.due_timers(now).await? {
        // CAS: only the tick that flips pending→fired fires it.
        if !hub.store.set_timer_status(&timer.id, TimerStatus::Fired).await? {
            continue;
        }
        // Timers carry what to do in their prompt; the stored payload holds at most a note.
        let note = timer.payload.get("note").and_then(|n| n.as_str()).map(String::from);
        let ev =
            crate::hub::builtin(BuiltinPayload::Timer(TimerPayload { timer_id: timer.id.clone(), note }), Source::Hub)?;
        if let Err(e) = router::fire(hub, &timer.target, timer.prompt.as_deref(), None, ev).await {
            tracing::warn!(timer = %timer.id, error = %e, "timer fire failed");
        }
    }
    Ok(earliest(next, hub.store.next_timer_at().await?))
}

fn earliest(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Run forever: tick, then sleep until the next due time (bounded), or until woken.
pub async fn run(hub: std::sync::Arc<Hub>) {
    loop {
        let now = Utc::now();
        let sleep = match tick(&hub, now).await {
            Ok(Some(next)) => (next - now).to_std().unwrap_or(Duration::ZERO).min(MAX_SLEEP),
            Ok(None) => MAX_SLEEP,
            Err(e) => {
                tracing::error!(error = %e, "scheduler tick failed");
                Duration::from_secs(5)
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            _ = hub.schedule_notify.notified() => {}
        }
    }
}
