//! Memory checkpoints. Agents rarely write memory on their own, so before one that talked with the
//! user (or did a lot of work) goes idle without having written any, it gets one short turn to
//! save what is worth keeping. Then it sleeps; always-on agents just stay idle. The checkpoint
//! turn itself does not count as talking, so it never triggers another, and a checkpoint that
//! fails or hangs never keeps an agent awake.

use botler_core::Source;
use botler_core::defs::AgentDef;
use botler_core::payloads::{BuiltinPayload, MessagePayload};
use chrono::{DateTime, Duration, Utc};

use crate::agents::LiveAgent;
use crate::hub::{Hub, Result, builtin};

/// What the agent is asked.
pub const PROMPT: &str = "You are about to go idle. Is there anything from this conversation worth keeping in memory: \
    the user's preferences, corrections, facts about them or their setup, decisions, non-obvious findings, state to \
    pick up later? If so, save it now with `memory_write` or `memory_append` (update existing files, keep `INDEX.md` \
    current) and reply in one line with what you saved. If not, reply `nothing to keep`. Do no other work.";
/// Tool calls in one wake that count as substantial work even without the user.
const WORK_TOOLS: u32 = 20;
/// How long a checkpoint may take before the agent sleeps anyway.
pub const TIMEOUT_SECS: i64 = 300;

/// What happened since the agent woke (or since its last checkpoint).
#[derive(Debug, Default)]
pub struct Since {
    /// A message from the user reached it.
    talked: bool,
    /// Tools it started.
    tools: u32,
    /// It wrote, appended to or deleted a memory file.
    remembered: bool,
    /// The checkpoint's delivery, and when it was sent.
    sent: Option<(String, DateTime<Utc>)>,
}

impl Since {
    pub fn talked(&mut self) {
        self.talked = true;
    }

    pub fn tool(&mut self) {
        self.tools = self.tools.saturating_add(1);
    }

    pub fn remembered(&mut self) {
        self.remembered = true;
    }

    /// The checkpoint sent at, if one is under way.
    pub fn sent_at(&self) -> Option<DateTime<Utc>> {
        self.sent.as_ref().map(|(_, at)| *at)
    }

    fn due(&self) -> bool {
        self.sent.is_none() && !self.remembered && (self.talked || self.tools >= WORK_TOOLS)
    }
}

/// Whether `agent` (of `def`) should get a checkpoint before it goes idle. Agents without a
/// handover (reviewers, triage) are left out: their verdict or incident resolution records what
/// they found.
pub async fn due(hub: &Hub, agent: &LiveAgent, def: &AgentDef) -> Result<bool> {
    if !hub.applied().await.defs.instance.defaults.memory_checkpoint || !def.spec.hands_over() || !agent.since().due() {
        return Ok(false);
    }
    // A handover (archiving, rollover) asks for memory itself.
    Ok(crate::handover::pending(hub, &agent.instance).await?.is_none())
}

/// Ask `agent` to save what is worth keeping (delivered by the dispatcher like any message).
pub async fn send(hub: &Hub, agent: &LiveAgent, now: DateTime<Utc>) -> Result<()> {
    let payload = BuiltinPayload::Message(MessagePayload { text: PROMPT.into(), files: vec![], mode: None, via: None });
    let ev = builtin(payload, Source::Hub)?.to(agent.instance.clone());
    hub.store.append_event(&ev).await?;
    hub.store.mark_routed(&ev.id.to_string()).await?;
    let d = hub.store.enqueue_delivery(&ev.id.to_string(), &agent.instance, None, None).await?;
    tracing::info!(instance = %agent.instance, "memory checkpoint before going idle");
    agent.since().sent = Some((d.id, now));
    hub.dispatch_notify.notify_one();
    Ok(())
}

/// Whether a checkpoint sent at `at` has had its time.
pub fn overdue(at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now - at > Duration::seconds(TIMEOUT_SECS)
}

/// A turn of `agent` ended (`failed`: with that error). Returns whether it was the checkpoint,
/// which ends quietly: no parent notice, no retries or incident on failure. Then the agent sleeps
/// unless it is always on or something new is queued for it.
pub async fn ended(hub: &Hub, agent: &LiveAgent, failed: Option<&str>) -> Result<bool> {
    let Some(sent) = agent.since().sent.as_ref().map(|(id, _)| id.clone()) else { return Ok(false) };
    let started = crate::agents::turn_start(hub, agent).await?;
    if started.is_none_or(|d| d.id != sent) {
        // Something else reached it first; it goes idle again later and may be asked then.
        agent.since().sent = None;
        return Ok(false);
    }
    match failed {
        Some(error) => tracing::warn!(instance = %agent.instance, %error, "memory checkpoint failed; sleeping anyway"),
        None => tracing::info!(instance = %agent.instance, "memory checkpoint done"),
    }
    *agent.since() = Since::default();
    let instance = hub.store.instance(&agent.instance).await?;
    let always_on = hub.applied().await.defs.agents.get(&instance.definition).is_some_and(|d| d.spec.always_on);
    if !always_on && hub.store.pending_deliveries(&agent.instance).await?.is_empty() {
        agent.stop().await;
    }
    Ok(true)
}
