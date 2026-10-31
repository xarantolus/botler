//! Agent instances: waking pi in a container, delivering queued events, metering usage, and
//! putting idle instances to sleep.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use botler_core::catalog;
use botler_core::defs::{AgentDef, Budget, Environment, Workspace};
use botler_core::loops::TurnLog;
use botler_core::payloads::{
    AgentIdlePayload, BuiltinPayload, DeliveryEnvelope, EnvironmentUpdatedPayload, MessagePayload, SendMode,
};
use botler_core::{Event, Source};
use botler_runtime::{ContainerSpec, Mount, PiClient, PiError, PiEvent, PiEventKind, Streaming};
use botler_store::{Delivery, Instance, InstanceState};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::agent_env::AgentEnv;
use crate::auth::Principal;
use crate::environment::{self, Runnable};
use crate::hub::{Hub, HubError, Live, Result, builtin};
use crate::phase::{AgentStatus, WakePhase, WakeStep};
use crate::{images, ops, prompt};

/// The forwarder (`images/agent-base/forward.mjs`, shipped with the hub and mounted).
pub const FORWARDER: &str = "/botler/forward/forward.mjs";
/// Attempts before a delivery pi keeps rejecting is dropped.
const MAX_ATTEMPTS: i64 = 5;
/// Every agent's persistent home: `$HOME` and the working directory.
pub const HOME: &str = "/home/agent";
/// Where a `workspace = "monorepo"` agent's own clone is mounted, inside its home.
pub const MONOREPO: &str = "/home/agent/monorepo";

pub struct LiveAgent {
    pub instance: String,
    pub wake: String,
    pub container: String,
    pub pi: PiClient,
    /// What this container was started with. When the applied definitions or the chat's grants
    /// change it, the agent is restarted between turns (see [`drifted`]).
    env: AgentEnv,
    /// The applied commit and grants `env` was last found current for (skips recomputing it).
    checked: std::sync::Mutex<String>,
    /// What changed since its previous container, told with its next delivery.
    changes: std::sync::Mutex<Vec<String>>,
    busy: AtomicBool,
    last_active: std::sync::Mutex<DateTime<Utc>>,
    /// Set when the hub stops the agent on purpose (idle, kill switch), so the exit is not a crash.
    stopping: AtomicBool,
    /// What it did this turn: decides whether its parent hears about the end of the turn.
    turn: std::sync::Mutex<TurnLog>,
    /// The error of the turn's last model call, if it failed: the turn failed when it settles.
    error: std::sync::Mutex<Option<String>>,
    /// Held while a delivery is handed to pi and recorded as delivered. pi can start and settle
    /// the turn before its response to the prompt is processed, so the end-of-turn handling takes
    /// it before looking up the delivery that started the turn.
    delivering: Mutex<()>,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl LiveAgent {
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    /// Being stopped: its container is on the way out, so nothing more is delivered to it (the
    /// exit wakes the dispatcher, which delivers to a fresh container instead).
    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn touch(&self) {
        *self.last_active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Utc::now();
    }

    /// The agent sent `to` a message through the hub during its current turn.
    pub fn record_message(&self, to: &str) {
        self.turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner).messaged(to);
    }

    fn turn(&self) -> std::sync::MutexGuard<'_, TurnLog> {
        self.turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn error(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn last_active(&self) -> DateTime<Utc> {
        *self.last_active.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        if let Some(k) = self.kill.lock().await.take()
            && k.send(()).is_err()
        {
            tracing::debug!(instance = %self.instance, "agent already exited");
        }
    }
}

/// Live pi connections, by instance name.
#[derive(Default)]
pub struct Agents {
    live: Mutex<HashMap<String, Arc<LiveAgent>>>,
    /// The current wake id per instance. Agent tokens are valid only for their wake, so a
    /// stopped session's token is dead immediately. Registered before the container starts,
    /// because pi connects to MCP as soon as it boots.
    wakes: Mutex<HashMap<String, String>>,
    /// Consecutive unexpected exits per instance, and when the next start is allowed.
    crashes: Mutex<HashMap<String, (u32, DateTime<Utc>)>>,
    /// What waking is doing right now, or why the last wake failed (for viewers).
    phases: Mutex<HashMap<String, WakePhase>>,
    /// Serialises waking per hub so one instance is never started twice.
    waking: Mutex<()>,
    /// The applied commit and when it was first seen, to restart agents once per burst of merges.
    applied_at: Mutex<Option<(String, DateTime<Utc>)>>,
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Agents(..)")
    }
}

impl Agents {
    pub async fn get(&self, instance: &str) -> Option<Arc<LiveAgent>> {
        self.live.lock().await.get(instance).cloned()
    }

    pub async fn all(&self) -> Vec<Arc<LiveAgent>> {
        self.live.lock().await.values().cloned().collect()
    }

    /// The current wake phase or last wake error of an instance, if any.
    pub async fn phase(&self, instance: &str) -> Option<WakePhase> {
        self.phases.lock().await.get(instance).cloned()
    }

    /// Make `wake` the current session of `instance`: tokens of any other wake stop working.
    pub async fn begin_wake(&self, instance: &str, wake: &str) {
        self.wakes.lock().await.insert(instance.to_string(), wake.to_string());
    }

    /// Whether `wake` is the current session of `instance`.
    pub async fn is_current(&self, instance: &str, wake: &str) -> bool {
        self.wakes.lock().await.get(instance).is_some_and(|w| w == wake)
    }
}

pub fn container_name(hub: &Hub, instance: &str) -> String {
    format!("botler-{}-agent-{instance}", hub.id)
}

pub fn branch_for(instance: &str) -> String {
    format!("agents/{instance}/work")
}

/// Deliver everything queued, waking instances as needed.
pub async fn dispatch_pending(hub: &Arc<Hub>) -> Result<()> {
    if hub.paused().await? {
        return Ok(());
    }
    for (name, wakes) in hub.store.instances_with_pending().await? {
        // Notes alone never wake an agent; they ride along with whatever wakes it next.
        if !wakes && hub.agents.get(&name).await.is_none_or(|a| a.stopping()) {
            continue;
        }
        if let Err(e) = deliver_to(hub, &name).await {
            tracing::warn!(instance = %name, error = %e, "delivery deferred");
        }
    }
    Ok(())
}

async fn deliver_to(hub: &Arc<Hub>, name: &str) -> Result<()> {
    let instance = hub.store.instance(name).await?;
    if instance.state == InstanceState::Archived {
        crate::restore::restore(hub, name, false).await?;
    }
    if let Some(b) = hub.store.breaker(&format!("instance:{name}")).await? {
        tracing::debug!(instance = %name, reason = %b.reason, "instance paused by breaker; deliveries wait");
        return Ok(());
    }
    if hub.agents.get(name).await.is_some_and(|a| a.stopping()) {
        return Ok(()); // delivered to its next container once this one has exited
    }
    if let Some(agent) = hub.agents.get(name).await
        && drifted(hub, &agent).await?
    {
        // A merge or a grant changed what its container should be: restart it (once its turn is
        // over) so the delivery reaches the new one. The exit wakes the dispatcher again.
        if !agent.busy() {
            tracing::info!(instance = %name, "environment changed; restarting the container");
            agent.stop().await;
        }
        return Ok(());
    }
    let agent = ensure_awake(hub, &instance).await?;
    // A turn this loop started may not show as busy yet: queued messages wait for it too.
    let mut started = false;
    let changes = std::mem::take(&mut *agent.changes.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    if !changes.is_empty() {
        // Ahead of the rest, as a note: the agent reads it with what woke it.
        let payload = BuiltinPayload::EnvironmentUpdated(EnvironmentUpdatedPayload { changes });
        let ev = builtin(payload, Source::Hub)?.to(name);
        hub.store.append_event(&ev).await?;
        hub.store.mark_routed(&ev.id.to_string()).await?;
        let d = hub.store.enqueue_delivery_with(&ev.id.to_string(), name, None, None, true, None).await?;
        started |= deliver_one(hub, &agent, &d, started).await?;
    }
    for d in hub.store.pending_deliveries(name).await? {
        started |= deliver_one(hub, &agent, &d, started).await?;
    }
    Ok(())
}

/// A message the user queued: held (pending, cancellable) until the agent's turn ends.
fn is_queued_message(ev: &Event) -> Result<bool> {
    if ev.source != Source::User || ev.kind != botler_core::event::kinds::MESSAGE {
        return Ok(false);
    }
    let m: MessagePayload =
        serde_json::from_value(ev.payload.clone()).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    Ok(m.mode == Some(SendMode::Queue))
}

/// The text pi receives for one delivery. The botler extension recognises the prefix, drops
/// duplicate ids, and renders the header. `previous` is when the instance's last delivery
/// arrived: the header notes a long gap since then.
pub fn envelope(
    d: &Delivery,
    ev: &Event,
    tz: chrono_tz::Tz,
    now: DateTime<Utc>,
    previous: Option<DateTime<Utc>>,
) -> Result<String> {
    let body = DeliveryEnvelope {
        id: d.id.clone(),
        event: ev.id.to_string(),
        kind: ev.kind.clone(),
        from: ev.source.key(),
        provenance: ev.provenance,
        correlation: ev.correlation.unwrap_or(ev.id).to_string(),
        body: catalog::render(ev, d.prompt.as_deref()),
        quiet: d.quiet,
        at: botler_core::clock::format_local(now, tz),
        gap: botler_core::clock::gap_note(now, previous),
    };
    let json = serde_json::to_string(&body).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    Ok(format!("botler:delivery {json}"))
}

/// Hand one delivery to pi. Returns whether it started a turn; a queued message is left pending
/// while one runs (`turn_started`: by this dispatch, maybe not reported busy yet).
async fn deliver_one(hub: &Hub, agent: &LiveAgent, d: &Delivery, turn_started: bool) -> Result<bool> {
    let ev = hub.store.event(&d.event_id).await?;
    let busy = agent.pi.is_busy().await?;
    if (busy || turn_started) && is_queued_message(&ev)? {
        tracing::debug!(delivery = %d.id, "queued message waits for the turn to end");
        return Ok(false);
    }
    let previous = hub.store.last_delivered(&d.instance).await?.and_then(|p| p.delivered_at);
    let text = envelope(d, &ev, hub.applied().await.defs.timezone, Utc::now(), previous)?;
    let streaming = if busy {
        // The user and urgent events interrupt at the next turn boundary; everything else waits.
        Some(if ev.urgent || ev.source == Source::User { Streaming::Steer } else { Streaming::FollowUp })
    } else {
        None
    };
    hub.store.mark_delivery_attempt(&d.id).await?;
    let _delivering = agent.delivering.lock().await;
    match agent.pi.prompt(&text, &[], streaming).await {
        Ok(disposition) => {
            hub.store.mark_delivered(&d.id).await?;
            // Busy is driven only by pi's own agent_start/agent_settled events: setting it here
            // would race with a settle that arrives before this response is processed.
            tracing::debug!(delivery = %d.id, ?disposition, "delivered");
            agent.touch();
            Ok(!busy && !d.quiet)
        }
        Err(PiError::Rejected { error, .. }) if d.attempts + 1 >= MAX_ATTEMPTS => {
            tracing::error!(delivery = %d.id, %error, "pi keeps rejecting delivery; dropping it");
            hub.store.drop_delivery(&d.id, &format!("rejected by pi: {error}")).await?;
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

async fn set_phase(hub: &Hub, instance: &str, step: WakeStep) {
    hub.agents.phases.lock().await.insert(instance.to_string(), WakePhase::Waking { step });
    broadcast_state(hub, instance, AgentStatus::Waking { step });
}

/// Return the live agent for `instance`, starting its container if needed. Progress and the
/// reason for a failure are visible through [`Agents::phase`].
pub async fn ensure_awake(hub: &Arc<Hub>, instance: &Instance) -> Result<Arc<LiveAgent>> {
    if let Some(a) = hub.agents.get(&instance.name).await {
        return Ok(a);
    }
    match wake(hub, instance).await {
        Ok(a) => {
            hub.agents.phases.lock().await.remove(&instance.name);
            Ok(a)
        }
        Err(e) => {
            let msg = format!("could not start: {e}");
            hub.agents.phases.lock().await.insert(instance.name.clone(), WakePhase::Failed { error: msg.clone() });
            if let Err(err) = hub.store.set_instance_state(&instance.name, InstanceState::Dormant, None).await {
                tracing::error!(instance = %instance.name, error = %err, "could not reset instance state");
            }
            broadcast_state(hub, &instance.name, AgentStatus::Failed { error: msg });
            Err(e)
        }
    }
}

async fn wake(hub: &Arc<Hub>, instance: &Instance) -> Result<Arc<LiveAgent>> {
    let _guard = hub.agents.waking.lock().await;
    if let Some(a) = hub.agents.get(&instance.name).await {
        return Ok(a);
    }
    if let Some((_, until)) = hub.agents.crashes.lock().await.get(&instance.name)
        && *until > Utc::now()
    {
        return Err(HubError::Backoff { instance: instance.name.clone(), until: *until });
    }
    let applied = hub.applied().await;
    let def = applied
        .defs
        .agents
        .get(&instance.definition)
        .ok_or_else(|| HubError::Undefined { kind: "agent", name: instance.definition.clone() })?;
    let limit = def.spec.limits.get("wake").copied().unwrap_or_else(|| botler_core::limits::default_limit("wake"));
    if let Err(e) = hub.rate_limit(&format!("agent:{}", instance.name), "wake", limit).await {
        let reason = format!("woken too often ({e})");
        if hub.store.trip_breaker(&format!("instance:{}", instance.name), "wake", &reason).await? {
            alarm(hub, &format!("Agent `{}` was {reason} and is paused until you clear it.", instance.name)).await?;
        }
        return Err(e);
    }

    hub.store.set_instance_state(&instance.name, InstanceState::Starting, None).await?;
    set_phase(hub, &instance.name, WakeStep::BuildingImage).await;
    let image = images::ensure_agent_image(hub, def.spec.env.image(), &applied.commit).await?;
    set_phase(hub, &instance.name, WakeStep::PreparingWorkspace).await;
    let branch = prepare_workspace(hub, instance, def).await?;
    prepare_home(hub, instance, def).await?;
    // pi resumes a session in the working directory recorded in its header, not its own.
    let session = hub.paths.sessions(&instance.name).join(&instance.session_file);
    if crate::sessions::retarget_cwd(&session, HOME).await? {
        tracing::info!(instance = %instance.name, cwd = HOME, "moved the session's working directory");
    }
    set_phase(hub, &instance.name, WakeStep::StartingContainer).await;
    let wake = uuid::Uuid::now_v7().simple().to_string();
    // The definition's environment plus what the chat holds, on every wake.
    let agent_env = crate::agent_env::compute(hub, instance, def, &applied, Some(image.clone())).await?;
    let checked = check_key(hub, &applied, &instance.name).await?;
    let changes = remember_env(hub, &instance.name, &agent_env).await?;
    let env = agent_env.env.clone();
    let spec = container_spec(hub, instance, def, &env, &applied, image, branch.as_deref(), &wake).await?;
    tracing::info!(instance = %instance.name, %wake, "waking agent");
    hub.agents.begin_wake(&instance.name, &wake).await;
    let attached = match hub.containers.run_attached(&spec).await {
        Ok(a) => a,
        Err(e) => {
            hub.agents.wakes.lock().await.remove(&instance.name);
            return Err(e.into());
        }
    };
    let (pi, events) = PiClient::connect(attached.stdin, attached.stdout, hub.config.pi_timeout);
    let agent = Arc::new(LiveAgent {
        instance: instance.name.clone(),
        wake,
        container: spec.name.clone(),
        pi,
        env: agent_env,
        checked: std::sync::Mutex::new(checked),
        changes: std::sync::Mutex::new(changes),
        busy: AtomicBool::new(false),
        last_active: std::sync::Mutex::new(Utc::now()),
        stopping: AtomicBool::new(false),
        turn: std::sync::Mutex::default(),
        error: std::sync::Mutex::default(),
        delivering: Mutex::default(),
        kill: Mutex::new(Some(attached.kill)),
    });
    hub.agents.live.lock().await.insert(instance.name.clone(), agent.clone());
    hub.store.set_instance_state(&instance.name, InstanceState::Running, Some(&spec.name)).await?;
    broadcast_state(hub, &instance.name, AgentStatus::Running);
    tokio::spawn(pump(hub.clone(), agent.clone(), events));
    tokio::spawn(watch_exit(hub.clone(), agent.clone(), attached.exited, attached.stderr));
    Ok(agent)
}

/// kv owner of each instance's last container environment (to tell it what changed).
const ENV_KV: &str = "hub:agent-env";
/// A burst of merges within this long restarts an idle agent once.
pub const RESTART_DEBOUNCE_SECS: i64 = 10;

/// Store `env` as `instance`'s current environment; returns what changed since the previous one.
async fn remember_env(hub: &Hub, instance: &str, env: &AgentEnv) -> Result<Vec<String>> {
    let json = |e: serde_json::Error| HubError::Io(std::io::Error::other(e));
    let previous: Option<AgentEnv> = match hub.store.kv_get(ENV_KV, instance).await? {
        // A shape from an older hub is no basis for a comparison.
        Some(v) => serde_json::from_str(&v).ok(),
        None => None,
    };
    hub.store.kv_set(ENV_KV, instance, &serde_json::to_string(env).map_err(json)?).await?;
    Ok(previous.map(|p| crate::agent_env::changes(&p, env)).unwrap_or_default())
}

/// What `agent`'s environment was computed for: the applied commit and its grants.
async fn check_key(hub: &Hub, applied: &crate::hub::Applied, instance: &str) -> Result<String> {
    let (_, grants) = crate::grants::environment(hub, instance).await?;
    Ok(format!("{} {}", applied.commit, grants.join(",")))
}

/// Whether `agent`'s container no longer matches what it would get now (after a merge or a
/// grant). Cheap when nothing changed since the last check.
async fn drifted(hub: &Hub, agent: &LiveAgent) -> Result<bool> {
    let applied = hub.applied().await;
    let key = check_key(hub, &applied, &agent.instance).await?;
    let lock = || agent.checked.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *lock() == key {
        return Ok(false);
    }
    let instance = hub.store.instance(&agent.instance).await?;
    // A removed definition keeps running agents as they are until they sleep.
    let Some(def) = applied.defs.agents.get(&instance.definition) else { return Ok(false) };
    let now = crate::agent_env::compute(hub, &instance, def, &applied, None).await?;
    if now == agent.env {
        *lock() = key;
        return Ok(false);
    }
    Ok(true)
}

/// After a merge (or a grant): build the images running agents now need, then restart those whose
/// environment changed, each once its turn is over and no further merge came within
/// [`RESTART_DEBOUNCE_SECS`]. They resume the same session with the same home; always-on agents
/// are started again right away, others when their next message arrives.
pub async fn refresh_environments(hub: &Hub, now: DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    let since = {
        let mut seen = hub.agents.applied_at.lock().await;
        match &*seen {
            Some((commit, at)) if *commit == applied.commit => *at,
            _ => {
                *seen = Some((applied.commit.clone(), now));
                now
            }
        }
    };
    for agent in hub.agents.all().await {
        if !drifted(hub, &agent).await? {
            continue;
        }
        let instance = hub.store.instance(&agent.instance).await?;
        if let Some(def) = applied.defs.agents.get(&instance.definition)
            && let Err(e) = images::ensure_agent_image(hub, def.spec.env.image(), &applied.commit).await
        {
            tracing::error!(instance = %agent.instance, error = %e, "could not build the new image; restart waits");
            continue;
        }
        if agent.busy() || now - since < Duration::seconds(RESTART_DEBOUNCE_SECS) {
            continue;
        }
        tracing::info!(instance = %agent.instance, "environment changed; restarting between turns");
        agent.stop().await;
    }
    Ok(())
}

/// The host directory mounted as an instance's home: its own, or the shared one its definition
/// names. Created on first use and never deleted (sleeping and archiving keep it).
pub fn home_dir(hub: &Hub, instance: &Instance, env: &Environment) -> std::path::PathBuf {
    match &env.home {
        Some(name) => hub.paths.shared_home(name),
        None => hub.paths.home(&instance.name),
    }
}

async fn prepare_home(hub: &Hub, instance: &Instance, def: &AgentDef) -> Result<()> {
    crate::storage::unpack_home(hub, &instance.name).await?;
    let home = home_dir(hub, instance, &def.spec.env);
    tokio::fs::create_dir_all(&home).await?;
    // Before homes, `workspace = "volume"` agents had a private directory at `/workspace`. The
    // home replaces it; its contents move into the home once so nothing is lost.
    let legacy = hub.paths.workspace(&instance.name);
    if def.spec.env.workspace() != Workspace::Monorepo && legacy.exists() {
        let mut to = home.join("workspace");
        if to.exists() {
            to = home.join(format!("workspace-{}", instance.name));
        }
        tracing::info!(instance = %instance.name, from = %legacy.display(), to = %to.display(), "moving the old private workspace into the home");
        tokio::fs::rename(&legacy, &to).await?;
    }
    Ok(())
}

/// Clone the monorepo into a new workspace (never touching an existing one: agents own it).
async fn prepare_workspace(hub: &Hub, instance: &Instance, def: &AgentDef) -> Result<Option<String>> {
    let dir = hub.paths.workspace(&instance.name);
    match def.spec.env.workspace() {
        Workspace::None | Workspace::Volume => Ok(None),
        Workspace::Monorepo => {
            let branch = branch_for(&instance.name);
            if !dir.join(".git").exists() {
                let tmp = dir.with_extension("cloning");
                if tmp.exists() {
                    tokio::fs::remove_dir_all(&tmp).await?;
                }
                git_fresh(&[
                    "clone",
                    "--quiet",
                    &crate::config::path_str(hub.repo.git_dir()),
                    &crate::config::path_str(&tmp),
                ])
                .await?;
                let origin = format!("{}/git/monorepo.git", hub.config.container_url);
                git_fresh(&["-C", &crate::config::path_str(&tmp), "remote", "set-url", "origin", &origin]).await?;
                git_fresh(&["-C", &crate::config::path_str(&tmp), "checkout", "--quiet", "-b", &branch]).await?;
                tokio::fs::rename(&tmp, &dir).await?;
            }
            Ok(Some(branch))
        }
    }
}

/// git on directories the hub just created itself (never on agent-owned checkouts).
async fn git_fresh(args: &[&str]) -> Result<()> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .await?;
    if !out.status.success() {
        return Err(HubError::Repo(botler_repo::Error::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        }));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn container_spec(
    hub: &Hub,
    instance: &Instance,
    def: &AgentDef,
    env: &Environment,
    applied: &crate::hub::Applied,
    image: String,
    branch: Option<&str>,
    wake: &str,
) -> Result<ContainerSpec> {
    let (_route_name, route) = applied
        .defs
        .route_for(&def.spec)
        .ok_or_else(|| HubError::Undefined { kind: "model route", name: def.name.clone() })?;

    // The forwarder relays 127.0.0.1:7420 to the hub's socket, then runs pi.
    let mut command = vec![
        "node".to_string(),
        FORWARDER.into(),
        "pi".into(),
        "--mode".into(),
        "rpc".into(),
        "--no-approve".into(),
        "--session".into(),
        format!("/botler/sessions/{}", instance.session_file),
        "--model".into(),
        format!("{}/{}", route.provider, route.model),
        "--append-system-prompt".into(),
        "/botler/agent-dir/botler.md".into(),
        "-e".into(),
        "/botler/extension/index.ts".into(),
    ];
    if let Some(t) = &route.thinking {
        command.extend(["--thinking".into(), t.clone()]);
    }
    // Agent-authored pi resources come from the applied main commit, never from the agent's own
    // branch, so an agent cannot load unreviewed code into itself.
    let pi_res = crate::images::pi_resources(hub, &applied.commit).await?;
    if let Some(pi_res) = &pi_res {
        for (flag, sub) in [("--skill", "skills"), ("--prompt-template", "prompts")] {
            if has_entries(&pi_res.join(sub)) {
                command.extend([flag.into(), format!("/botler/pi/{sub}")]);
            }
        }
        for ext in list_files(&pi_res.join("extensions"), "ts") {
            command.extend(["-e".into(), format!("/botler/pi/extensions/{ext}")]);
        }
    }

    let route_key: Vec<String> = route.api_key_secret.iter().cloned().collect();
    let what = Runnable::Agent { instance, wake };
    let mut spec = environment::container_spec(hub, env, what, image, command, &route_key).await?;
    let vars = &mut spec.env;
    vars.insert("PI_OFFLINE".into(), "1".into());
    vars.insert("BOTLER_INSTANCE".into(), instance.name.clone());
    // Lets git in the workspace push to the hub without storing the token in .git/config.
    vars.insert("GIT_CONFIG_COUNT".into(), "2".into());
    vars.insert("GIT_CONFIG_KEY_0".into(), format!("http.{}/git/.extraHeader", hub.config.container_url));
    vars.insert("GIT_CONFIG_VALUE_0".into(), format!("Authorization: Bearer {}", vars["BOTLER_TOKEN"]));
    // Clones made before the hub moved behind the socket have the old TCP origin.
    vars.insert("GIT_CONFIG_KEY_1".into(), format!("url.{}/.insteadOf", hub.config.container_url));
    vars.insert("GIT_CONFIG_VALUE_1".into(), format!("http://host.containers.internal:{}/", hub.config.listen.port()));

    // Rendered pi config: a tmpfs agent dir with read-only files on top (pi needs to write locks).
    let dir = hub.paths.agent_dir(&instance.name);
    tokio::fs::create_dir_all(&dir).await?;
    // pi skips an invalid models.json silently (and `null` values make it invalid), so only
    // fields that are set are written.
    let mut provider = serde_json::Map::new();
    if let Some(s) = &route.api_key_secret {
        provider.insert("apiKey".into(), json!(format!("${s}")));
    }
    if let Some(u) = &route.base_url {
        provider.insert("baseUrl".into(), json!(u));
    }
    let mut model = json!({ "maxTokens": route.max_tokens });
    if let Some(input) = &route.input {
        model["input"] = json!(input);
    }
    provider.insert("modelOverrides".into(), json!({ route.model.clone(): model }));
    let models = json!({ "providers": { route.provider.clone(): provider } });
    let mut servers = serde_json::Map::new();
    servers.insert(
        "botler".into(),
        json!({
            "url": format!("{}/mcp", hub.config.container_url),
            "headers": { "Authorization": "Bearer ${BOTLER_TOKEN}" },
            "description": "The botler hub: messaging, timers, spawning agents, asking the user, proposing changes, incidents.",
            // The extension's turn-ending tools replace these.
            "toolExposure": ops::turn_ending().map(|n| (n, "hidden")).collect::<BTreeMap<_, _>>(),
        }),
    );
    servers.extend(mcp_servers(applied, env));
    // Python tools this agent is granted (`tools` globs); the extension registers them.
    let tools = applied.defs.tools_for(&def.spec.env);
    // Hub ops that end the turn; the extension registers them with `terminate: true`.
    let principal = Principal::Agent { instance: instance.name.clone(), wake: wake.into() };
    let caller = ops::Caller { principal, capabilities: def.spec.capabilities.clone(), runnable: None };
    let files: Vec<(&str, String)> = vec![
        ("models.json", to_json(&models)?),
        ("tools.json", to_json(&json!(tools))?),
        ("turn-tools.json", to_json(&ops::turn_ending_tools(&caller))?),
        ("mcp.json", to_json(&json!({ "mcpServers": servers }))?),
        // Memory indexes are read once per wake, so the prompt stays stable within a session.
        ("botler.md", prompt::render(instance, def, branch) + &crate::memory::index_section(hub, def).await?),
        ("AGENTS.md", def.prompt.clone().unwrap_or_default()),
    ];
    let mounts = &mut spec.mounts;
    mounts.push(Mount::Tmpfs { dst: "/botler/agent-dir".into() });
    for (name, body) in &files {
        let host = dir.join(name);
        tokio::fs::write(&host, body).await?;
        mounts.push(Mount::Bind { src: host, dst: format!("/botler/agent-dir/{name}"), read_only: true });
    }
    mounts.extend(crate::memory::mounts(hub, def).await?);
    let sessions = hub.paths.sessions(&instance.name);
    tokio::fs::create_dir_all(&sessions).await?;
    mounts.push(Mount::Bind { src: sessions, dst: "/botler/sessions".into(), read_only: false });
    mounts.push(Mount::Bind {
        src: hub.paths.assets().join("extension"),
        dst: "/botler/extension".into(),
        read_only: true,
    });
    let inbox = hub.paths.inbox(&instance.name);
    tokio::fs::create_dir_all(&inbox).await?;
    mounts.push(Mount::Bind { src: inbox, dst: crate::uploads::INBOX.into(), read_only: true });
    mounts.push(Mount::Bind {
        src: hub.paths.assets().join("forward"),
        dst: "/botler/forward".into(),
        read_only: true,
    });
    if let Some(src) = pi_res {
        mounts.push(Mount::Bind { src, dst: "/botler/pi".into(), read_only: true });
    }
    Ok(spec)
}

/// `mcp.json` entries besides the hub: granted services, reached directly on the services network
/// (they get no botler token), and stdio servers pi starts inside the container.
fn mcp_servers(applied: &crate::hub::Applied, env: &Environment) -> serde_json::Map<String, serde_json::Value> {
    let mut servers = serde_json::Map::new();
    for svc in &env.mcp {
        // Validation guarantees a service with a port.
        let Some(port) = applied.defs.jobs.get(svc).and_then(|j| j.spec.env.port) else { continue };
        let url = format!("http://{svc}:{port}/mcp"); // the service's alias on the network
        servers.insert(svc.clone(), json!({ "url": url }));
    }
    for (name, s) in &env.mcp_stdio {
        let Some((command, args)) = s.command.split_first() else { continue };
        servers.insert(name.clone(), json!({ "command": command, "args": args, "env": s.env }));
    }
    servers
}

fn to_json(v: &serde_json::Value) -> Result<String> {
    serde_json::to_string_pretty(v).map_err(|e| HubError::Io(std::io::Error::other(e)))
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut rd| rd.any(|e| e.is_ok_and(|e| e.file_name() != "README.md")))
}

fn list_files(dir: &Path, ext: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<String> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(&format!(".{ext}")))
        .collect();
    out.sort();
    out
}

/// Consume pi's events: live view, busy tracking, usage and budgets.
async fn pump(hub: Arc<Hub>, agent: Arc<LiveAgent>, mut events: mpsc::Receiver<PiEvent>) {
    while let Some(ev) = events.recv().await {
        if hub.live.send(Live::Pi { instance: agent.instance.clone(), event: ev.raw.clone() }).is_err() {
            tracing::trace!("no live viewers");
        }
        match &ev.kind {
            PiEventKind::AgentStart => {
                agent.busy.store(true, Ordering::SeqCst);
                agent.turn().reset();
                *agent.error() = None;
                // pi is up and working: the instance is healthy again.
                hub.agents.crashes.lock().await.remove(&agent.instance);
                if hub.agents.phases.lock().await.remove(&agent.instance).is_some() {
                    broadcast_state(&hub, &agent.instance, AgentStatus::Running);
                }
            }
            PiEventKind::AgentSettled => {
                agent.busy.store(false, Ordering::SeqCst);
                agent.touch();
                if let Err(e) = hub.store.touch_instance(&agent.instance).await {
                    tracing::error!(instance = %agent.instance, error = %e, "could not record activity");
                }
                let failed = agent.error().take();
                let ended = match failed {
                    Some(err) => crate::turns::failed(&hub, &agent, &err).await,
                    None => turn_succeeded(&hub, &agent).await,
                };
                if let Err(e) = ended {
                    tracing::error!(instance = %agent.instance, error = %e, "could not handle the end of the turn");
                }
                hub.dispatch_notify.notify_one();
            }
            PiEventKind::MessageEnd { role, usage, error } => {
                if role == "assistant" {
                    if let Some(err) = error {
                        tracing::warn!(instance = %agent.instance, %err, "model call failed");
                    }
                    agent.error().clone_from(error);
                }
                if let Some(u) = usage
                    && let Err(e) = meter(&hub, &agent, u).await
                {
                    tracing::error!(instance = %agent.instance, error = %e, "usage metering failed");
                }
            }
            PiEventKind::ToolStart { tool } => agent.turn().tool_started(tool),
            _ => {}
        }
    }
}

async fn turn_succeeded(hub: &Hub, agent: &LiveAgent) -> Result<()> {
    crate::turns::succeeded(hub, &agent.instance).await?;
    notify_parent(hub, agent).await
}

/// The delivery that started the agent's current turn. A delivery still being recorded may be
/// it: wait for it. pi's response to the prompt is read by the client's reader task, not the
/// pump, so this cannot deadlock, and the prompt is bounded by the pi timeout.
pub(crate) async fn turn_start(hub: &Hub, agent: &LiveAgent) -> Result<Option<Delivery>> {
    let _delivering = agent.delivering.lock().await;
    Ok(hub.store.last_delivered(&agent.instance).await?)
}

/// Tell the agent that spawned this one that it ended its turn, with its final answer, unless
/// the last thing it did was messaging the parent (which already has the news). Routed like a
/// message from the child: it counts for the pair breaker, and an acknowledgement ("Done,
/// thanks.") arrives as a note that does not wake the parent. Chained to the delivery that
/// started the turn so the hop limit still bounds runaway chains.
async fn notify_parent(hub: &Hub, agent: &LiveAgent) -> Result<()> {
    let instance = hub.store.instance(&agent.instance).await?;
    let Some(parent) = instance.parent.clone() else { return Ok(()) };
    if agent.turn().ended_by_messaging(&parent) {
        tracing::debug!(instance = %agent.instance, %parent, "turn ended by messaging the parent; no notice");
        return Ok(());
    }
    let last_reply = match agent.pi.command(json!({ "type": "get_last_assistant_text" })).await {
        Ok(d) => d.get("text").and_then(|t| t.as_str()).unwrap_or("").chars().take(4000).collect::<String>(),
        Err(e) => {
            tracing::warn!(instance = %agent.instance, error = %e, "could not read last reply");
            String::new()
        }
    };
    let idle = BuiltinPayload::AgentIdle(AgentIdlePayload {
        instance: instance.name.clone(),
        title: instance.title.clone(),
        slug: instance.slug.clone(),
        last_reply,
    });
    let mut ev = builtin(idle, Source::Agent(instance.name.clone()))?.to(parent);
    if let Some(d) = turn_start(hub, agent).await? {
        let started = hub.store.event(&d.event_id).await?;
        ev = ev.in_reply_to(&started);
    }
    hub.emit(ev).await?;
    Ok(())
}

/// Record usage and stop the agent when it exceeds its budget (stop and report, never truncate).
async fn meter(hub: &Hub, agent: &LiveAgent, u: &botler_runtime::Usage) -> Result<()> {
    let now = Utc::now();
    hub.store.record_usage(&agent.instance, now, &u.model, u.total_tokens, u.cost_usd).await?;
    let instance = hub.store.instance(&agent.instance).await?;
    let applied = hub.applied().await;
    let budget: Budget = applied
        .defs
        .agents
        .get(&instance.definition)
        .and_then(|d| d.spec.budget.clone())
        .unwrap_or_else(|| applied.defs.instance.defaults.budget.clone());
    let day_start = now
        .with_timezone(&applied.defs.timezone)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(applied.defs.timezone).earliest())
        .map_or(now - Duration::hours(24), |t| t.with_timezone(&Utc));
    let today = hub.store.usage_since(Some(&agent.instance), day_start).await?;
    let over = match (budget.usd_per_day, budget.tokens_per_day) {
        (Some(usd), _) if today.cost_usd > usd => {
            Some(format!("spent ${:.2} today (budget ${usd:.2})", today.cost_usd))
        }
        (_, Some(tok)) if today.tokens > i64::try_from(tok).unwrap_or(i64::MAX) => {
            Some(format!("used {} tokens today (budget {tok})", today.tokens))
        }
        _ => None,
    };
    if let Some(reason) = over
        && hub.store.trip_breaker(&format!("instance:{}", agent.instance), "budget", &reason).await?
    {
        tracing::warn!(instance = %agent.instance, %reason, "budget exceeded; stopping agent");
        if let Err(e) = agent.pi.abort().await {
            tracing::warn!(instance = %agent.instance, error = %e, "abort after budget failed");
        }
        alarm(
            hub,
            &format!("Agent `{}` {reason} and was stopped. Clear its breaker to let it continue.", agent.instance),
        )
        .await?;
    }
    Ok(())
}

async fn watch_exit(
    hub: Arc<Hub>,
    agent: Arc<LiveAgent>,
    exited: oneshot::Receiver<Option<i32>>,
    stderr: botler_runtime::StderrTail,
) {
    let code = match exited.await {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!(instance = %agent.instance, "exit status lost");
            None
        }
    };
    let deliberate = agent.stopping.load(Ordering::SeqCst);
    // Idle or crashed, every instance only goes to sleep: it is woken by its next message with
    // its session and workspace intact. Ending a chat is an explicit act (archiving). Recorded
    // while it is still listed as live (and so not woken again), so that no longer being live
    // always means its sleep is recorded.
    let state = InstanceState::Dormant;
    if let Err(e) = hub.store.set_instance_state(&agent.instance, state, None).await {
        tracing::error!(instance = %agent.instance, error = %e, "could not record agent exit");
    }
    {
        let mut live = hub.agents.live.lock().await;
        if live.get(&agent.instance).is_some_and(|a| Arc::ptr_eq(a, &agent)) {
            live.remove(&agent.instance);
        }
    }
    {
        // Revoke this session's token.
        let mut wakes = hub.agents.wakes.lock().await;
        if wakes.get(&agent.instance).is_some_and(|w| *w == agent.wake) {
            wakes.remove(&agent.instance);
        }
    }
    // A scheduled retry or a failed turn stays visible while it sleeps.
    let status = hub.agents.phase(&agent.instance).await.map_or(AgentStatus::Dormant, AgentStatus::from);
    broadcast_state(&hub, &agent.instance, status);
    if !deliberate {
        let tail = stderr.text();
        tracing::warn!(instance = %agent.instance, ?code, stderr = %tail, "agent container exited unexpectedly");
        if let Err(e) = record_crash(&hub, &agent.instance, code, tail).await {
            tracing::error!(instance = %agent.instance, error = %e, "could not record agent crash");
        }
    }
    // Anything still queued wakes it again (subject to the wake limit).
    hub.dispatch_notify.notify_one();
}

/// Consecutive crashes before an incident is opened.
const CRASH_INCIDENT_AFTER: u32 = 3;
const MAX_BACKOFF_SECS: u64 = 300;

/// Back off exponentially after an unexpected exit; open an incident if it keeps happening.
async fn record_crash(hub: &Hub, instance: &str, code: Option<i32>, stderr: String) -> Result<()> {
    let count = {
        let mut crashes = hub.agents.crashes.lock().await;
        let entry = crashes.entry(instance.to_string()).or_insert((0, Utc::now()));
        entry.0 += 1;
        let secs = 2u64.saturating_pow(entry.0).min(MAX_BACKOFF_SECS);
        entry.1 = Utc::now() + Duration::seconds(i64::try_from(secs).unwrap_or(300));
        entry.0
    };
    if count == CRASH_INCIDENT_AFTER {
        crate::failures::report(
            hub,
            crate::failures::Failure {
                runnable: format!("agent:{instance}"),
                kind: "start",
                exit_code: code.map(i64::from),
                run_id: None,
                logs_tail: stderr,
                commit: Some(hub.applied().await.commit.clone()),
                event_id: None,
            },
        )
        .await?;
    }
    Ok(())
}

/// How long an idle agent stays up when all it waits for is the user (or a proposal). The answer
/// arrives as an event and wakes it with its session and home, so waiting asleep loses nothing.
const WAITING_GRACE_SECS: i64 = 120;

/// Whether everything `instance` still waits for is outside it: open questions, credential
/// requests or approvals it created, or unfinished proposals (and nothing queued for it).
async fn waiting_on_user(hub: &Hub, instance: &str) -> Result<bool> {
    let dialogs = hub.store.open_dialogs().await?.into_iter().any(|d| d.instance == instance);
    let proposals = hub.store.unfinished_proposals().await?.into_iter().any(|p| p.instance == instance);
    Ok(dialogs || proposals)
}

/// Put idle agents to sleep (they wake on their next message). Agents that only wait for the
/// user sleep after a short grace period instead of their full idle timeout.
pub async fn reap_idle(hub: &Hub, now: DateTime<Utc>) -> Result<()> {
    let applied = hub.applied().await;
    for agent in hub.agents.all().await {
        if agent.busy() {
            continue;
        }
        let instance = hub.store.instance(&agent.instance).await?;
        if applied.defs.agents.get(&instance.definition).is_some_and(|d| d.spec.always_on) {
            continue;
        }
        let idle = applied
            .defs
            .agents
            .get(&instance.definition)
            .and_then(|d| d.spec.idle_timeout)
            .unwrap_or(applied.defs.instance.defaults.idle_timeout);
        if !hub.store.pending_deliveries(&agent.instance).await?.is_empty() {
            continue;
        }
        let idle_for = now - agent.last_active();
        if idle_for > idle.as_chrono() {
            tracing::info!(instance = %agent.instance, "idle; stopping");
            agent.stop().await;
        } else if idle_for > Duration::seconds(WAITING_GRACE_SECS) && waiting_on_user(hub, &agent.instance).await? {
            tracing::info!(instance = %agent.instance, "waiting on the user; sleeping until the answer arrives");
            agent.stop().await;
        }
    }
    Ok(())
}

/// Stop an agent shortly after its current tool call returns (archiving).
pub async fn stop_after_reply(hub: &Hub, instance: &str) {
    if let Some(agent) = hub.agents.get(instance).await {
        tokio::spawn(async move {
            // Let the tool result reach the model and the turn finish.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            agent.stop().await;
        });
    }
}

/// Stop every agent (kill switch / shutdown). They come back on their next delivery.
pub async fn stop_all(hub: &Hub) {
    for agent in hub.agents.all().await {
        if let Err(e) = agent.pi.abort().await {
            tracing::debug!(instance = %agent.instance, error = %e, "abort before stop failed");
        }
        agent.stop().await;
    }
}

/// After a restart no pi connection survives: mark instances dormant and remove leftovers.
pub async fn reconcile_on_boot(hub: &Hub) -> Result<()> {
    let n = hub.store.reset_running_instances().await?;
    let leftovers = hub.containers.list(&hub.labels("agent")).await?;
    for c in leftovers {
        hub.containers.remove(&c.name).await?;
    }
    if n > 0 {
        tracing::info!(instances = n, "agents were running before restart; they wake on their next delivery");
    }
    Ok(())
}

/// Record a phase that outlives the wake (a scheduled retry, a failed turn) and show it.
pub(crate) async fn show_phase(hub: &Hub, instance: &str, phase: WakePhase) {
    hub.agents.phases.lock().await.insert(instance.to_string(), phase.clone());
    broadcast_state(hub, instance, phase.into());
}

pub(crate) fn broadcast_state(hub: &Hub, instance: &str, status: AgentStatus) {
    if hub.live.send(Live::Instance { instance: instance.into(), status }).is_err() {
        tracing::trace!("no live viewers");
    }
}

/// An urgent note to the front door (`botler`) from the hub.
pub async fn alarm(hub: &Hub, text: &str) -> Result<()> {
    let ev =
        builtin(BuiltinPayload::Message(MessagePayload { text: text.into(), files: vec![], mode: None }), Source::Hub)?
            .to(botler_core::FRONT_DOOR)
            .urgent(true);
    hub.emit(ev).await?;
    Ok(())
}

/// Keep `always_on` agents running (started at boot, restarted after a crash).
pub async fn keep_always_on(hub: &Arc<Hub>) -> Result<()> {
    if hub.paused().await? {
        return Ok(());
    }
    let applied = hub.applied().await;
    for def in applied.defs.persistent_agents().filter(|d| d.spec.always_on) {
        if hub.agents.get(&def.name).await.is_some() {
            continue;
        }
        let instance = match hub.store.find_instance(&def.name).await? {
            Some(i) => i,
            None => {
                hub.store
                    .create_instance(&botler_store::NewInstance {
                        name: def.name.clone(),
                        definition: def.name.clone(),
                        persistent: true,
                        session_file: format!("{}.jsonl", def.name),
                        branch: None,
                        parent: None,
                        depth: 0,
                        task: None,
                    })
                    .await?
            }
        };
        if hub.store.breaker(&format!("instance:{}", instance.name)).await?.is_some() {
            continue;
        }
        if let Err(e) = ensure_awake(hub, &instance).await {
            tracing::warn!(instance = %instance.name, error = %e, "always-on agent could not start");
        }
    }
    Ok(())
}

/// Run forever: deliver when woken, keep always-on agents up, reap idle agents periodically.
pub async fn run(hub: Arc<Hub>) {
    loop {
        if let Err(e) = keep_always_on(&hub).await {
            tracing::error!(error = %e, "always-on check failed");
        }
        if let Err(e) = refresh_environments(&hub, Utc::now()).await {
            tracing::error!(error = %e, "environment refresh failed");
        }
        if let Err(e) = dispatch_pending(&hub).await {
            tracing::error!(error = %e, "dispatch failed");
        }
        if let Err(e) = reap_idle(&hub, Utc::now()).await {
            tracing::error!(error = %e, "idle reaping failed");
        }
        tokio::select! {
            _ = hub.dispatch_notify.notified() => {}
            _ = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
        }
    }
}
