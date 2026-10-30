//! Failures of runnables become incidents, and incidents go to an agent.

use botler_core::event::kinds;
use botler_core::routing::{Action, Dispatch};
use botler_core::{Event, Provenance, Source};
use chrono::Utc;
use serde_json::json;

use crate::hub::{Hub, Result};
use crate::router;

/// What went wrong with a runnable.
#[derive(Clone, Debug)]
pub struct Failure {
    pub runnable: String,
    /// exit | timeout | oom | unhealthy | crashloop | start
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
    let ev = Event::new(kinds::RUNNABLE_FAILED, Source::Hub, Provenance::Trusted, Utc::now()).payload(json!({
        "runnable": f.runnable,
        "kind": f.kind,
        "exit_code": f.exit_code,
        "run_id": f.run_id,
        "incident_id": incident.id,
        "count": incident.count,
        "logs_tail": f.logs_tail,
        "commit": f.commit,
        "event_id": f.event_id,
    }));
    let applied = hub.applied().await;
    let target = applied.defs.failure_target(&f.runnable);
    // Without a triage agent the failure still reaches the user through main.
    let action = match Action::from(&target) {
        Action::Spawn(d) if !applied.defs.agents.contains_key(&d) => Action::Deliver("main".into()),
        a => a,
    };
    let prompt = format!("Job `{}` failed ({}). Investigate and resolve the incident.", f.runnable, f.kind);
    let d = Dispatch { trigger: None, action, prompt: Some(prompt), shadow: false };
    router::dispatch(hub, ev, vec![d]).await
}

/// The last `lines` lines of a log.
pub fn tail(log: &str, lines: usize) -> String {
    let all: Vec<&str> = log.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}
