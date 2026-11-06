//! How busy each chat has been lately, for the sidebar's order: a chat in a long exchange (many
//! messages in the last hours) ranks above a one-off chat touched a minute ago.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Duration, Utc};

use crate::hub::{Hub, Result};

/// A message's weight halves in about two hours (`exp(-age / 3h)`).
const DECAY_HOURS: f64 = 3.0;
/// The user's own messages count this many times.
const USER_WEIGHT: f64 = 3.0;
/// Chats of agents that do not hand over (reviewers, triage): short-lived helpers.
const HELPER_WEIGHT: f64 = 0.3;

/// The score of each chat from its deliveries (`instance`, when, the event's source): the sum
/// of their weights, each decaying with age. The hub's own notes do not count.
pub fn score(
    deliveries: &[(String, DateTime<Utc>, String)],
    helpers: &HashSet<String>,
    now: DateTime<Utc>,
) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    for (instance, at, source) in deliveries {
        let weight = match source.as_str() {
            "hub" => continue,
            "user" => USER_WEIGHT,
            _ => 1.0,
        };
        let hours = (now - *at).num_seconds().max(0) as f64 / 3600.0;
        let helper = if helpers.contains(instance) { HELPER_WEIGHT } else { 1.0 };
        *out.entry(instance.clone()).or_default() += weight * helper * (-hours / DECAY_HOURS).exp();
    }
    out
}

/// Every live chat's score over the last day.
pub async fn by_instance(hub: &Hub, now: DateTime<Utc>) -> Result<BTreeMap<String, f64>> {
    let deliveries = hub.store.deliveries_since(now - Duration::hours(24)).await?;
    let applied = hub.applied().await;
    let helpers = hub
        .store
        .live_instances()
        .await?
        .into_iter()
        .filter(|i| applied.defs.agents.get(&i.definition).is_some_and(|d| !d.spec.hands_over()))
        .map(|i| i.name)
        .collect();
    Ok(score(&deliveries, &helpers, now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_exchange_outranks_a_fresh_helper() {
        let now = Utc::now();
        let ago = |m: i64| now - Duration::minutes(m);
        let mut d: Vec<(String, DateTime<Utc>, String)> =
            (0..20).map(|i| ("curator".into(), ago(30 + i * 6), "runnable:rightmove".into())).collect();
        d.push(("curator".into(), ago(90), "user".into()));
        d.push(("triage-1".into(), ago(1), "hub".into()));
        d.push(("triage-1".into(), ago(1), "agent:botler".into()));
        d.push(("old".into(), ago(23 * 60), "user".into()));
        let s = score(&d, &["triage-1".to_string()].into(), now);
        assert!(s["curator"] > 10.0, "{s:?}");
        assert!((s["triage-1"] - 0.3).abs() < 0.01, "{s:?}");
        assert!(s["old"] < 0.01, "{s:?}");
    }
}
