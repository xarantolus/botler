//! The System view: everything the applied definitions define, what happens next, and what
//! needs attention. Derived on every call from the definitions, the store and the reconciler.

use botler_core::change::{Status, TriggerPreview, describe_trigger};
use botler_core::defs::{AgentKind, Lifetime, ResolvedTrigger, Target, TriggerKind};
use botler_core::trigger::plan;
use botler_store::{Incident, IncidentStatus, ProposalStatus, Run, RunStatus};
use chrono::{DateTime, Utc};
use serde::Serialize;
use ts_rs::TS;

use crate::hub::{Hub, Result};
use crate::wire::{ServiceState, ServiceStatus};

/// How many fire times each trigger lists.
const NEXT_FIRES: usize = 3;
/// How many recent runs each job lists.
const RECENT_RUNS: u32 = 5;

#[derive(Debug, Serialize, TS)]
pub struct SystemView {
    /// The instance's time zone (IANA name): fire times are shown in it.
    pub timezone: String,
    /// While paused nothing fires and services are stopped.
    pub paused: bool,
    pub commit: AppliedCommit,
    /// Jobs and services, by name.
    pub jobs: Vec<JobView>,
    pub agents: Vec<AgentView>,
    /// Triggers of `triggers/*.toml`.
    pub triggers: Vec<TriggerPreview>,
}

/// The monorepo commit whose definitions are applied.
#[derive(Debug, Serialize, TS)]
pub struct AppliedCommit {
    pub id: String,
    pub short: String,
    pub title: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Serialize, TS)]
pub struct JobView {
    pub name: String,
    pub description: String,
    pub lifetime: Lifetime,
    /// Triggers that run it, from any file; schedules with their next fire times.
    pub triggers: Vec<TriggerPreview>,
    /// The latest runs, newest first.
    pub recent_runs: Vec<Run>,
    /// Its open incident.
    pub incident: Option<Incident>,
    pub secrets: Vec<String>,
    /// For services: state and addresses.
    pub service: Option<ServiceStatus>,
}

#[derive(Debug, Serialize, TS)]
pub struct AgentView {
    pub name: String,
    pub description: String,
    pub kind: AgentKind,
    pub always_on: bool,
    /// Tool globs from `tools/`.
    pub tools: Vec<String>,
    pub secrets: Vec<String>,
    /// Triggers that message or start it.
    pub triggers: Vec<TriggerPreview>,
    /// Instances that are not archived.
    pub instances: usize,
}

/// Counts of what needs attention (the sidebar's badges).
#[derive(Debug, Default, Serialize, TS)]
pub struct Attention {
    /// Crash-looping services plus jobs whose last run failed.
    pub system_failed: usize,
    /// Services waiting out a restart backoff.
    pub system_backoff: usize,
    /// Proposals being gated, reviewed or waiting for approval.
    pub proposals_in_flight: usize,
    /// Of those, the ones waiting for the user.
    pub proposals_awaiting_user: usize,
}

pub async fn view(hub: &Hub) -> Result<SystemView> {
    let applied = hub.applied().await;
    let now = Utc::now();
    let (title, at) = hub.repo.commit_info(&applied.commit).await?;
    let mut previews = Vec::new();
    for t in applied.defs.triggers.values() {
        previews.push((t, preview(hub, t, now).await?));
    }
    let targeting = |want: &dyn Fn(&Target) -> bool| -> Vec<TriggerPreview> {
        previews.iter().filter(|(t, _)| want(&t.target)).map(|(_, p)| p.clone()).collect()
    };
    let mut services = crate::services::status(hub).await?;
    let mut jobs = Vec::new();
    for j in applied.defs.jobs.values() {
        let incident =
            hub.store.incidents_for(&j.name, 1).await?.into_iter().find(|i| i.status == IncidentStatus::Open);
        jobs.push(JobView {
            name: j.name.clone(),
            description: j.spec.description.clone(),
            lifetime: j.spec.lifetime,
            triggers: targeting(&|t| matches!(t, Target::Run(n) if *n == j.name)),
            recent_runs: hub.store.runs_for(&j.name, RECENT_RUNS).await?,
            incident,
            secrets: j.spec.env.secrets.clone(),
            service: services.iter().position(|s| s.name == j.name).map(|i| services.swap_remove(i)),
        });
    }
    let live = hub.store.live_instances().await?;
    let agents = applied
        .defs
        .agents
        .values()
        .map(|a| AgentView {
            name: a.name.clone(),
            description: a.spec.description.clone(),
            kind: a.spec.kind,
            always_on: a.spec.always_on,
            tools: a.spec.env.tools.clone(),
            secrets: a.spec.env.secrets.clone(),
            triggers: targeting(&|t| matches!(t, Target::Resume(n) | Target::Spawn(n) if *n == a.name)),
            instances: live.iter().filter(|i| i.definition == a.name).count(),
        })
        .collect();
    let triggers = previews.iter().filter(|(t, _)| t.id.starts_with("triggers/")).map(|(_, p)| p.clone()).collect();
    Ok(SystemView {
        timezone: applied.defs.timezone.name().into(),
        paused: hub.paused().await?,
        commit: AppliedCommit {
            id: applied.commit.clone(),
            short: applied.commit.chars().take(7).collect(),
            title,
            at,
        },
        jobs,
        agents,
        triggers,
    })
}

/// A trigger in plain language. Schedules list the fire times the scheduler will use: from the
/// stored `last_fired` (intervals stay on their grid), not from now.
async fn preview(hub: &Hub, t: &ResolvedTrigger, now: DateTime<Utc>) -> Result<TriggerPreview> {
    let mut p = describe_trigger(t, Status::Unchanged, now);
    p.upcoming.clear();
    if let TriggerKind::Schedule(spec) = &t.kind {
        let state = hub.store.schedule_state(&t.id, now).await?;
        if let Some(next) = plan(spec, t.spec.missed, state.last_fired, state.anchor, now).next {
            p.upcoming.push(next);
            p.upcoming.extend(spec.upcoming(next, NEXT_FIRES - 1));
        }
    }
    Ok(p)
}

/// What needs attention, given the services' states.
pub async fn attention(hub: &Hub, services: &[ServiceStatus]) -> Result<Attention> {
    let applied = hub.applied().await;
    let mut a = Attention::default();
    for s in services {
        match s.state {
            ServiceState::Failed => a.system_failed += 1,
            ServiceState::Backoff => a.system_backoff += 1,
            ServiceState::Running | ServiceState::Updating | ServiceState::Starting | ServiceState::Paused => {}
        }
    }
    for j in applied.defs.jobs.values().filter(|j| j.spec.lifetime == Lifetime::Oneshot) {
        let last = hub.store.runs_for(&j.name, 1).await?.into_iter().next();
        if last.is_some_and(|r| matches!(r.status, RunStatus::Failed | RunStatus::Timeout)) {
            a.system_failed += 1;
        }
    }
    for p in hub.store.recent_proposals(100).await?.iter().filter(|p| !p.status.is_terminal()) {
        a.proposals_in_flight += 1;
        if p.status == ProposalStatus::AwaitingUser {
            a.proposals_awaiting_user += 1;
        }
    }
    Ok(a)
}
