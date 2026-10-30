//! Schedule math: next fire times, the missed-run policy, and plain-language descriptions.
//!
//! The scheduler stores only `last_fired` per trigger (a fact). Everything else is derived here,
//! which is what lets the hub recompute its whole timetable after a restart or a transaction.

use std::str::FromStr;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use serde::Serialize;

use crate::defs::Missed;
use crate::duration::HumanDuration;

/// A cron schedule in a time zone. Only constructible through [`ScheduleSpec::cron`], so it is
/// always valid and never re-parsed.
#[derive(Clone, Debug)]
pub struct CronSchedule {
    expr: String,
    tz: Tz,
    cron: Box<Cron>,
}

impl CronSchedule {
    pub fn expr(&self) -> &str {
        &self.expr
    }

    pub fn tz(&self) -> Tz {
        self.tz
    }
}

impl PartialEq for CronSchedule {
    fn eq(&self, other: &Self) -> bool {
        self.expr == other.expr && self.tz == other.tz
    }
}

impl Serialize for CronSchedule {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("CronSchedule", 2)?;
        st.serialize_field("expr", &self.expr)?;
        st.serialize_field("tz", self.tz.name())?;
        st.end()
    }
}

/// A validated schedule.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ScheduleSpec {
    Cron(CronSchedule),
    Every(HumanDuration),
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("invalid cron expression {expr:?}: {source}")]
    InvalidCron { expr: String, source: croner::errors::CronError },
    #[error("unknown time zone {0:?}")]
    UnknownTimeZone(String),
    #[error("cron expression {0:?} never fires")]
    NeverFires(String),
}

/// A fire that is this late still counts as on time (scheduler tick jitter, short restarts).
pub const ON_TIME_GRACE: chrono::Duration = chrono::Duration::seconds(90);

impl ScheduleSpec {
    pub fn cron(expr: &str, tz: &str) -> Result<Self, ScheduleError> {
        let cron = Cron::from_str(expr).map_err(|source| ScheduleError::InvalidCron { expr: expr.into(), source })?;
        let tz: Tz = tz.parse().map_err(|_| ScheduleError::UnknownTimeZone(tz.into()))?;
        let spec = Self::Cron(CronSchedule { expr: expr.to_string(), tz, cron: Box::new(cron) });
        // Patterns like "0 0 30 2 *" parse but match nothing; reject them up front.
        if spec.next_after(Utc::now()).is_none() {
            return Err(ScheduleError::NeverFires(expr.into()));
        }
        Ok(spec)
    }

    /// The first fire strictly after `after`, or `None` if the schedule has no further fires
    /// (croner's search limit: the pattern matches no reachable date).
    pub fn next_after(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        match self {
            Self::Every(d) => Some(after + d.as_chrono()),
            Self::Cron(c) => {
                let local = after.with_timezone(&c.tz);
                match c.cron.find_next_occurrence(&local, false) {
                    Ok(t) => Some(t.with_timezone(&Utc)),
                    Err(croner::errors::CronError::TimeSearchLimitExceeded) => None,
                    Err(e) => {
                        // Only the search-limit error is expected for a parsed pattern; anything
                        // else is a croner bug worth seeing, and the trigger then simply does not fire.
                        tracing::warn!(expr = %c.expr, error = %e, "cron evaluation failed");
                        None
                    }
                }
            }
        }
    }

    /// The next `n` fire times after `after`.
    pub fn upcoming(&self, after: DateTime<Utc>, n: usize) -> Vec<DateTime<Utc>> {
        let mut out = Vec::with_capacity(n);
        let mut t = after;
        while out.len() < n {
            match self.next_after(t) {
                Some(next) => {
                    out.push(next);
                    t = next;
                }
                None => break,
            }
        }
        out
    }

    /// Plain-language description, e.g. "At 07:00, Monday through Friday (Europe/Berlin)".
    pub fn describe(&self) -> String {
        match self {
            Self::Every(d) => format!("Every {d}"),
            Self::Cron(c) => format!("{} ({})", c.cron.describe(), c.tz.name()),
        }
    }
}

/// What the scheduler should do for one schedule trigger at `now`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirePlan {
    /// Fire once now.
    pub fire: bool,
    /// How many scheduled fires were dropped (downtime), for the record.
    pub skipped: u32,
    /// New value for `last_fired` (the latest due time considered), if anything was due.
    pub mark: Option<DateTime<Utc>>,
    /// When to look at this trigger again.
    pub next: Option<DateTime<Utc>>,
}

/// Decide whether a schedule fires at `now`.
///
/// `last` is the stored `last_fired`; `anchor` is when the trigger was first applied (used when it
/// has never fired, so a new trigger does not fire for all of history).
pub fn plan(
    spec: &ScheduleSpec,
    missed: Missed,
    last: Option<DateTime<Utc>>,
    anchor: DateTime<Utc>,
    now: DateTime<Utc>,
) -> FirePlan {
    let base = last.unwrap_or(anchor);
    let mut due = Vec::new();
    let mut t = base;
    // Bound the walk: a minutely cron after a month offline is 43k entries; we only need the
    // count and the latest, so cap and count the rest cheaply.
    const CAP: usize = 10_000;
    while let Some(next) = spec.next_after(t) {
        if next > now || due.len() >= CAP {
            break;
        }
        due.push(next);
        t = next;
    }
    // Intervals stay on their grid (last + k * every); crons are wall-clock anchored.
    let next_future = match spec {
        ScheduleSpec::Every(_) => spec.next_after(t),
        ScheduleSpec::Cron(_) => spec.next_after(now.max(base)),
    };
    let Some(&latest) = due.last() else {
        return FirePlan { fire: false, skipped: 0, mark: None, next: next_future };
    };
    let on_time = now - latest <= ON_TIME_GRACE;
    let fire = on_time || missed == Missed::RunOnce;
    let skipped = (due.len() as u32).saturating_sub(u32::from(fire));
    FirePlan { fire, skipped, mark: Some(latest), next: next_future }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    #[test]
    fn cron_respects_timezone() {
        let s = ScheduleSpec::cron("0 7 * * *", "Europe/Berlin").unwrap();
        // 2026-10-30 is CET+1 after DST ended on 10-25: 07:00 Berlin = 06:00 UTC
        let next = s.next_after(utc(2026, 10, 30, 0, 0)).unwrap();
        assert_eq!(next, utc(2026, 10, 30, 6, 0));
        assert!(s.describe().contains("Europe/Berlin"));
    }

    #[test]
    fn invalid_cron_and_tz_are_errors() {
        assert!(ScheduleSpec::cron("not cron", "UTC").is_err());
        assert!(matches!(ScheduleSpec::cron("0 7 * * *", "Mars/Olympus"), Err(ScheduleError::UnknownTimeZone(_))));
        assert!(matches!(ScheduleSpec::cron("0 0 30 2 *", "UTC"), Err(ScheduleError::NeverFires(_))));
    }

    #[test]
    fn upcoming_lists_n() {
        let s = ScheduleSpec::Every(HumanDuration::from_secs(300));
        let t = utc(2026, 1, 1, 0, 0);
        assert_eq!(s.upcoming(t, 3), vec![utc(2026, 1, 1, 0, 5), utc(2026, 1, 1, 0, 10), utc(2026, 1, 1, 0, 15)]);
    }

    #[test]
    fn plan_on_time_fires() {
        let s = ScheduleSpec::cron("0 * * * *", "UTC").unwrap();
        let p = plan(&s, Missed::Skip, Some(utc(2026, 1, 1, 9, 0)), utc(2025, 1, 1, 0, 0), utc(2026, 1, 1, 10, 0));
        assert!(p.fire);
        assert_eq!(p.skipped, 0);
        assert_eq!(p.mark, Some(utc(2026, 1, 1, 10, 0)));
        assert_eq!(p.next, Some(utc(2026, 1, 1, 11, 0)));
    }

    #[test]
    fn plan_downtime_skip_vs_run_once() {
        let s = ScheduleSpec::cron("0 * * * *", "UTC").unwrap();
        let start = utc(2026, 1, 1, 0, 0);
        let last = Some(start);
        let now = utc(2026, 1, 1, 5, 30); // 05:00 is 30 minutes late: stale
        let skip = plan(&s, Missed::Skip, last, start, now);
        assert!(!skip.fire);
        assert_eq!(skip.skipped, 5);
        assert_eq!(skip.mark, Some(utc(2026, 1, 1, 5, 0)));
        let once = plan(&s, Missed::RunOnce, last, start, now);
        assert!(once.fire);
        assert_eq!(once.skipped, 4);
        assert_eq!(once.next, Some(utc(2026, 1, 1, 6, 0)));
    }

    #[test]
    fn plan_new_trigger_uses_anchor() {
        let s = ScheduleSpec::cron("0 7 * * *", "UTC").unwrap();
        let p = plan(&s, Missed::RunOnce, None, utc(2026, 1, 1, 8, 0), utc(2026, 1, 1, 8, 1));
        assert!(!p.fire);
        assert_eq!(p.next, Some(utc(2026, 1, 2, 7, 0)));
    }
}
