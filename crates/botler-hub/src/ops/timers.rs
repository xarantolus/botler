//! One-off timers: wake an agent later.

use std::sync::Arc;

use botler_core::defs::Target;
use botler_store::{NewTimer, Timer, TimerStatus};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Caller, Op, OpError, Who, parse_when, resolve_chat};
use crate::auth::Principal;
use crate::hub::Hub;

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct SetTimer {
    /// RFC 3339 time, e.g. `2026-11-02T07:30:00+01:00`.
    #[serde(default)]
    pub at: Option<String>,
    /// Delay instead of a time, e.g. `20m`, `2h 30m`.
    #[serde(default, rename = "in")]
    pub after: Option<String>,
    /// What the woken agent is told.
    pub prompt: String,
    /// Instance to wake; defaults to the caller.
    #[serde(default)]
    pub instance: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct TimerSet {
    pub timer_id: String,
    pub fires_at: DateTime<Utc>,
    /// `fires_at` in the instance's time zone, for people.
    pub local: String,
    /// A trial run: recorded, not carried out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub dry_run: bool,
}

impl Op for SetTimer {
    const NAME: &'static str = "set_timer";
    const DESCRIPTION: &'static str = "Wake an agent later with a prompt: at an RFC 3339 time or after a delay like \"2h\". Defaults to waking you. One-off only; recurring work belongs in a trigger file.";
    const WHO: Who = Who::Everyone;
    type Output = TimerSet;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<TimerSet, OpError> {
        let now = Utc::now();
        let fire_at = parse_when(self.at.as_deref(), self.after.as_deref(), now)?;
        if fire_at < now - chrono::Duration::minutes(1) {
            return Err(OpError::BadRequest(format!("{fire_at} is in the past")));
        }
        let target = match self.instance.as_deref().or(caller.instance()) {
            Some(i) => Target::Resume(resolve_chat(hub, i).await?),
            None => return Err(OpError::BadRequest("`instance` is required".into())),
        };
        let tz = hub.applied().await.defs.timezone;
        let local = fire_at.with_timezone(&tz).format("%a %Y-%m-%d %H:%M %Z").to_string();
        if let Some(trial) = crate::trials::of_caller(hub, caller).await? {
            let timer = TimerEffect { fires_at: fire_at, target: &target, prompt: &self.prompt };
            crate::trials::record(hub, &trial, Self::NAME, &timer).await?;
            return Ok(TimerSet { timer_id: String::new(), fires_at: fire_at, local, dry_run: true });
        }
        let t = hub
            .store
            .create_timer(&NewTimer {
                fire_at,
                target,
                prompt: Some(self.prompt),
                payload: json!({}),
                created_by: caller.subject(),
            })
            .await?;
        hub.schedule_notify.notify_one();
        Ok(TimerSet { timer_id: t.id, fires_at: fire_at, local, dry_run: false })
    }
}

/// The timer a trial run would have set.
#[derive(Serialize)]
struct TimerEffect<'a> {
    fires_at: DateTime<Utc>,
    target: &'a Target,
    prompt: &'a str,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct CancelTimer {
    pub id: String,
}

#[derive(Debug, Serialize, TS)]
pub struct TimerCancelled {
    /// Whether it was still pending.
    pub cancelled: bool,
}

impl Op for CancelTimer {
    const NAME: &'static str = "cancel_timer";
    const DESCRIPTION: &'static str = "Cancel a pending timer.";
    const WHO: Who = Who::Everyone;
    type Output = TimerCancelled;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<TimerCancelled, OpError> {
        let t = hub.store.timer(&self.id).await?;
        if caller.principal != Principal::User && t.created_by != caller.subject() {
            return Err(OpError::Forbidden("you can only cancel your own timers".into()));
        }
        let cancelled = hub.store.set_timer_status(&self.id, TimerStatus::Cancelled).await?;
        Ok(TimerCancelled { cancelled })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListTimers {}

impl Op for ListTimers {
    const NAME: &'static str = "list_timers";
    const DESCRIPTION: &'static str = "Pending timers you created (all timers for the user).";
    const WHO: Who = Who::UserAndAgents;
    type Output = Vec<Timer>;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Vec<Timer>, OpError> {
        let mine = (caller.principal != Principal::User).then(|| caller.subject());
        Ok(hub.store.pending_timers(mine.as_deref()).await?)
    }
}

/// How a `wait`'s timer prompt starts.
const WAITED: &str = "You waited (at most";

/// Cancel `instance`'s pending `wait`s: something arrived (or it waits anew), so they would only
/// wake it again for nothing.
pub async fn end_waits(hub: &Hub, instance: &str) -> crate::hub::Result<()> {
    for t in hub.store.pending_timers(None).await? {
        if t.target == Target::Resume(instance.to_string())
            && t.prompt.as_deref().is_some_and(|p| p.starts_with(WAITED))
        {
            hub.store.set_timer_status(&t.id, TimerStatus::Cancelled).await?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct Wait {
    /// At most how long, e.g. `10m`, `2h` (up to a day).
    #[serde(rename = "in")]
    pub after: String,
    /// What you are waiting for, e.g. "the trial of mail-watch"; you are reminded of it.
    pub reason: String,
}

#[derive(Debug, Serialize, TS)]
pub struct Waiting {
    pub timer_id: String,
    pub wakes_at: String,
    pub note: String,
}

impl Op for Wait {
    const NAME: &'static str = "wait";
    const DESCRIPTION: &'static str = "Wait for something nothing will tell you about (an outside page or process), instead of sleeping or polling in a shell: your turn ends now, and you wake when anything arrives for you or at the latest after `in`, reminded of `reason`. Not for answers, approvals, grants, proposals, trials or children: those wake you by themselves, so just end your turn. A new wait replaces the last one, and anything arriving ends it.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = Waiting;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Waiting, OpError> {
        let instance = caller.agent("only agents wait")?.to_string();
        let now = Utc::now();
        let wake = parse_when(None, Some(&self.after), now)?;
        if wake > now + chrono::Duration::days(1) {
            return Err(OpError::BadRequest("wait at most a day; use set_timer for later".into()));
        }
        // One wait at a time: this one replaces any still pending.
        end_waits(hub, &instance).await?;
        let prompt = format!(
            "{WAITED} {}) for: {}. Check on it now; if it already arrived and you dealt with it, carry on.",
            self.after, self.reason
        );
        let timer = SetTimer { at: None, after: Some(self.after.clone()), prompt, instance: Some(instance) };
        let set = timer.call(hub, caller).await?;
        Ok(Waiting {
            timer_id: set.timer_id,
            wakes_at: set.local.clone(),
            note: format!("Waiting: you wake when something arrives, or at {} at the latest.", set.local),
        })
    }
}
