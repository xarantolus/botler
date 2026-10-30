//! Rate limits: token buckets keyed by (subject, operation). Pure; time is passed in.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::duration::HumanDuration;

/// `burst` operations at once, refilled at `burst` per `per`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RateLimit {
    pub burst: u32,
    pub per: HumanDuration,
}

impl RateLimit {
    pub const fn new(burst: u32, per_secs: u64) -> Self {
        Self { burst, per: HumanDuration::from_secs(per_secs) }
    }

    fn refill_per_sec(&self) -> f64 {
        f64::from(self.burst) / self.per.0.as_secs_f64().max(1e-9)
    }

    /// True if `self` allows no more than `other` (used to check agents only tighten limits).
    pub fn is_at_most(&self, other: &RateLimit) -> bool {
        self.burst <= other.burst && self.refill_per_sec() <= other.refill_per_sec() + 1e-9
    }
}

/// Built-in ceilings for every hub operation. Definitions may tighten these freely; raising one
/// is a capability change that needs the user's approval.
pub fn default_limit(operation: &str) -> RateLimit {
    match operation {
        "send_message" => RateLimit::new(30, 60),
        "emit_event" => RateLimit::new(60, 60),
        "set_timer" => RateLimit::new(20, 60),
        "spawn_agent" => RateLimit::new(5, 300),
        "ask_user" => RateLimit::new(5, 600),
        "propose_change" => RateLimit::new(10, 3600),
        "trial_run" => RateLimit::new(10, 3600),
        "invoke_action" => RateLimit::new(10, 600),
        "replay_event" => RateLimit::new(10, 600),
        "wake" => RateLimit::new(30, 3600),
        _ => RateLimit::new(120, 60),
    }
}

#[derive(Clone, Debug)]
struct Bucket {
    tokens: f64,
    updated: DateTime<Utc>,
}

/// In-memory token buckets. Losing them on restart is acceptable: limits only get looser for one
/// window, which is why breakers (persisted) exist for the dangerous patterns.
#[derive(Debug, Default)]
pub struct Limiter {
    buckets: HashMap<(String, String), Bucket>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Allow,
    /// Denied; retry after this long.
    Deny {
        retry_after: std::time::Duration,
    },
}

impl Limiter {
    pub fn check(&mut self, subject: &str, operation: &str, limit: RateLimit, now: DateTime<Utc>) -> Decision {
        let cap = f64::from(limit.burst);
        let rate = limit.refill_per_sec();
        let bucket = self
            .buckets
            .entry((subject.to_owned(), operation.to_owned()))
            .or_insert(Bucket { tokens: cap, updated: now });
        let elapsed = (now - bucket.updated).num_milliseconds().max(0) as f64 / 1000.0;
        bucket.tokens = (bucket.tokens + elapsed * rate).min(cap);
        bucket.updated = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Decision::Allow
        } else {
            let wait = (1.0 - bucket.tokens) / rate;
            Decision::Deny { retry_after: std::time::Duration::from_secs_f64(wait) }
        }
    }
}

/// Correlation chains longer than this trip the cycle breaker. A coarse backstop only: it must
/// not punish deep agent trees (8 levels reporting up and down take ~32 hops), and real
/// ping-pong is caught earlier by the per-pair conversation breaker and acknowledgement notes.
pub const MAX_CHAIN_HOPS: u32 = 64;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn bucket_allows_burst_then_refills() {
        let mut l = Limiter::default();
        let lim = RateLimit::new(2, 60);
        let t0 = Utc::now();
        assert_eq!(l.check("a", "op", lim, t0), Decision::Allow);
        assert_eq!(l.check("a", "op", lim, t0), Decision::Allow);
        assert!(matches!(l.check("a", "op", lim, t0), Decision::Deny { .. }));
        // other subject unaffected
        assert_eq!(l.check("b", "op", lim, t0), Decision::Allow);
        // 30s refills one token at 2/min
        assert_eq!(l.check("a", "op", lim, t0 + Duration::seconds(30)), Decision::Allow);
    }

    #[test]
    fn tighten_check() {
        assert!(RateLimit::new(5, 60).is_at_most(&RateLimit::new(10, 60)));
        assert!(!RateLimit::new(5, 10).is_at_most(&RateLimit::new(10, 60)));
    }
}
