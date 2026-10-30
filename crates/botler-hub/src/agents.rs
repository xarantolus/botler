//! Agent instances: waking pi in a container, delivering queued events, metering usage, and
//! putting idle instances to sleep.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use botler_core::catalog;
use botler_core::defs::{AgentDef, Budget, Network, Workspace};
use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_runtime::{ContainerSpec, Mount, NetworkMode, PiClient, PiError, PiEvent, PiEventKind, Streaming};
use botler_store::{Delivery, Instance, InstanceState};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::auth::Principal;
use crate::hub::{Hub, HubError, Live, Result};
use crate::{images, plumbing, prompt};

/// Attempts before a delivery pi keeps rejecting is dropped.
const MAX_ATTEMPTS: i64 = 5;
/// Upper bound only: agent tokens die with their wake (see `Agents::wakes`).
const AGENT_TOKEN_TTL_HOURS: i64 = 24 * 7;

pub struct LiveAgent {
    pub instance: String,
    pub wake: String,
    pub container: String,
    pub pi: PiClient,
    busy: AtomicBool,
    last_active: std::sync::Mutex<DateTime<Utc>>,
    /// Set when the hub stops the agent on purpose (idle, kill switch), so the exit is not a crash.
    stopping: AtomicBool,
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

impl LiveAgent {
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }

    fn touch(&self) {
        *self.last_active.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Utc::now();
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
    phases: Mutex<HashMap<String, String>>,
    /// Serialises waking per hub so one instance is never started twice.
    waking: Mutex<()>,
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
    pub async fn phase(&self, instance: &str) -> Option<String> {
        self.phases.lock().await.get(instance).cloned()
    }

    /// Whether `wake` is the current session of `instance`.
    pub async fn is_current(&self, instance: &str, wake: &str) -> bool {
        self.wakes.lock().await.get(instance).is_some_and(|w| w == wake)
    }
}

pub fn container_name(instance: &str) -> String {
    format!("botler-agent-{instance}")
}

pub fn branch_for(instance: &str) -> String {
    format!("agents/{instance}/work")
}

/// Deliver everything queued, waking instances as needed.
pub async fn dispatch_pending(hub: &Arc<Hub>) -> Result<()> {
    if hub.paused().await? {
        return Ok(());
    }
    for name in hub.store.instances_with_pending().await? {
        if let Err(e) = deliver_to(hub, &name).await {
            tracing::warn!(instance = %name, error = %e, "delivery deferred");
        }
    }
    Ok(())
}

async fn deliver_to(hub: &Arc<Hub>, name: &str) -> Result<()> {
    let instance = hub.store.instance(name).await?;
    if instance.state == InstanceState::Archived {
        hub.store.restore_instance(name).await?;
    }
    if let Some(b) = hub.store.breaker(&format!("instance:{name}")).await? {
        tracing::debug!(instance = %name, reason = %b.reason, "instance paused by breaker; deliveries wait");
        return Ok(());
    }
    let agent = ensure_awake(hub, &instance).await?;
    for d in hub.store.pending_deliveries(name).await? {
        deliver_one(hub, &agent, &d).await?;
    }
    Ok(())
}

/// The text pi receives for one delivery. The botler extension recognises the prefix, drops
/// duplicate ids, and renders the header.
pub fn envelope(d: &Delivery, ev: &Event) -> String {
    let provenance = match ev.provenance {
        Provenance::Trusted => "trusted",
        Provenance::External => "external",
        Provenance::Untrusted => "untrusted",
    };
    let body = json!({
        "id": d.id,
        "event": ev.id,
        "kind": ev.kind,
        "from": ev.source.key(),
        "provenance": provenance,
        "correlation": ev.correlation.unwrap_or(ev.id),
        "body": catalog::render(ev, d.prompt.as_deref()),
    });
    format!("botler:delivery {body}")
}

async fn deliver_one(hub: &Hub, agent: &LiveAgent, d: &Delivery) -> Result<()> {
    let ev = hub.store.event(&d.event_id).await?;
    let text = envelope(d, &ev);
    let streaming = if agent.pi.is_busy().await? {
        // The user and urgent events interrupt at the next turn boundary; everything else waits.
        Some(if ev.urgent || ev.source == Source::User { Streaming::Steer } else { Streaming::FollowUp })
    } else {
        None
    };
    hub.store.mark_delivery_attempt(&d.id).await?;
    match agent.pi.prompt(&text, &[], streaming).await {
        Ok(disposition) => {
            hub.store.mark_delivered(&d.id).await?;
            // Busy is driven only by pi's own agent_start/agent_settled events: setting it here
            // would race with a settle that arrives before this response is processed.
            tracing::debug!(delivery = %d.id, ?disposition, "delivered");
            agent.touch();
            Ok(())
        }
        Err(PiError::Rejected { error, .. }) if d.attempts + 1 >= MAX_ATTEMPTS => {
            tracing::error!(delivery = %d.id, %error, "pi keeps rejecting delivery; dropping it");
            hub.store.drop_delivery(&d.id, &format!("rejected by pi: {error}")).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

async fn set_phase(hub: &Hub, instance: &str, phase: &str) {
    hub.agents.phases.lock().await.insert(instance.to_string(), phase.to_string());
    broadcast_state(hub, instance, &format!("starting: {phase}"));
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
            hub.agents.phases.lock().await.insert(instance.name.clone(), msg.clone());
            if let Err(err) = hub.store.set_instance_state(&instance.name, InstanceState::Dormant, None).await {
                tracing::error!(instance = %instance.name, error = %err, "could not reset instance state");
            }
            broadcast_state(hub, &instance.name, &msg);
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
    set_phase(hub, &instance.name, "building image").await;
    let image = images::ensure_agent_image(hub, &def.spec.image, &applied.commit).await?;
    set_phase(hub, &instance.name, "preparing workspace").await;
    let branch = prepare_workspace(hub, instance, def).await?;
    set_phase(hub, &instance.name, "starting container").await;
    let wake = uuid::Uuid::now_v7().simple().to_string();
    let spec = container_spec(hub, instance, def, &applied, image, branch.as_deref(), &wake).await?;
    tracing::info!(instance = %instance.name, %wake, "waking agent");
    hub.agents.wakes.lock().await.insert(instance.name.clone(), wake.clone());
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
        busy: AtomicBool::new(false),
        last_active: std::sync::Mutex::new(Utc::now()),
        stopping: AtomicBool::new(false),
        kill: Mutex::new(Some(attached.kill)),
    });
    hub.agents.live.lock().await.insert(instance.name.clone(), agent.clone());
    hub.store.set_instance_state(&instance.name, InstanceState::Running, Some(&spec.name)).await?;
    broadcast_state(hub, &instance.name, "running");
    tokio::spawn(pump(hub.clone(), agent.clone(), events));
    tokio::spawn(watch_exit(hub.clone(), agent.clone(), attached.exited, attached.stderr));
    Ok(agent)
}

/// Clone the monorepo into a new workspace (never touching an existing one: agents own it).
async fn prepare_workspace(hub: &Hub, instance: &Instance, def: &AgentDef) -> Result<Option<String>> {
    let dir = hub.paths.workspace(&instance.name);
    match def.spec.workspace {
        Workspace::None => Ok(None),
        Workspace::Volume => {
            tokio::fs::create_dir_all(&dir).await?;
            Ok(None)
        }
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

async fn container_spec(
    hub: &Hub,
    instance: &Instance,
    def: &AgentDef,
    applied: &crate::hub::Applied,
    image: String,
    branch: Option<&str>,
    wake: &str,
) -> Result<ContainerSpec> {
    let (_route_name, route) = applied
        .defs
        .route_for(&def.spec)
        .ok_or_else(|| HubError::Undefined { kind: "model route", name: def.name.clone() })?;

    let route_key: Vec<String> = route.api_key_secret.iter().cloned().collect();
    let p = plumbing::plumbing(
        hub,
        &Principal::Agent { instance: instance.name.clone(), wake: wake.into() },
        Duration::hours(AGENT_TOKEN_TTL_HOURS),
        &def.spec.secrets,
        &route_key,
        hub.paths.root.join("agent-secrets").join(&instance.name),
    )
    .await?;
    let mut env = p.env;
    env.insert("PI_OFFLINE".into(), "1".into());
    env.insert("BOTLER_INSTANCE".into(), instance.name.clone());
    // Lets git in the workspace push to the hub without storing the token in .git/config.
    env.insert("GIT_CONFIG_COUNT".into(), "1".into());
    env.insert("GIT_CONFIG_KEY_0".into(), format!("http.{}/git/.extraHeader", hub.config.container_url));
    env.insert("GIT_CONFIG_VALUE_0".into(), format!("Authorization: Bearer {}", env["BOTLER_TOKEN"]));

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
    provider.insert("modelOverrides".into(), json!({ route.model.clone(): { "maxTokens": route.max_tokens } }));
    let models = json!({ "providers": { route.provider.clone(): provider } });
    let mut servers = serde_json::Map::new();
    servers.insert(
        "botler".into(),
        json!({
            "url": format!("{}/mcp", hub.config.container_url),
            "headers": { "Authorization": "Bearer ${BOTLER_TOKEN}" },
            "description": "The botler hub: messaging, timers, spawning agents, asking the user, proposing changes, incidents."
        }),
    );
    for svc in &def.spec.mcp {
        servers.insert(
            svc.clone(),
            json!({
                "url": format!("{}/services/{svc}/mcp", hub.config.container_url),
                "headers": { "Authorization": "Bearer ${BOTLER_TOKEN}" }
            }),
        );
    }
    let files: Vec<(&str, String)> = vec![
        ("models.json", to_json(&models)?),
        ("mcp.json", to_json(&json!({ "mcpServers": servers }))?),
        ("botler.md", prompt::render(instance, def, branch)),
        ("AGENTS.md", def.prompt.clone().unwrap_or_default()),
    ];
    let mut mounts = p.mounts;
    mounts.push(Mount::Tmpfs { dst: "/botler/agent-dir".into() });
    for (name, body) in &files {
        let host = dir.join(name);
        tokio::fs::write(&host, body).await?;
        mounts.push(Mount::Bind { src: host, dst: format!("/botler/agent-dir/{name}"), read_only: true });
    }
    let sessions = hub.paths.sessions(&instance.name);
    tokio::fs::create_dir_all(&sessions).await?;
    mounts.push(Mount::Bind { src: sessions, dst: "/botler/sessions".into(), read_only: false });
    mounts.push(Mount::Bind {
        src: hub.paths.assets().join("extension"),
        dst: "/botler/extension".into(),
        read_only: true,
    });
    if def.spec.workspace != Workspace::None {
        mounts.push(Mount::Bind {
            src: hub.paths.workspace(&instance.name),
            dst: "/workspace".into(),
            read_only: false,
        });
    }
    // Shared scratch names are validated definition names; per-chat ones are prefixed so they
    // can never collide with a project name.
    let scratch =
        hub.paths.scratch_space(&def.spec.scratch.clone().unwrap_or_else(|| format!("chat-{}", instance.name)));
    tokio::fs::create_dir_all(&scratch).await?;
    mounts.push(Mount::Bind { src: scratch, dst: "/scratch".into(), read_only: false });

    let mut command = vec![
        "pi".to_string(),
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
    let pi_res = hub.paths.exports().join(&applied.commit).join("pi");
    if pi_res.exists() {
        mounts.push(Mount::Bind { src: pi_res.clone(), dst: "/botler/pi".into(), read_only: true });
        for (flag, sub) in [("--skill", "skills"), ("--prompt-template", "prompts")] {
            if has_entries(&pi_res.join(sub)) {
                command.extend([flag.into(), format!("/botler/pi/{sub}")]);
            }
        }
        for ext in list_files(&pi_res.join("extensions"), "ts") {
            command.extend(["-e".into(), format!("/botler/pi/extensions/{ext}")]);
        }
    }

    Ok(ContainerSpec {
        name: container_name(&instance.name),
        image,
        command,
        env,
        mounts,
        labels: BTreeMap::from([
            (crate::jobs::ROLE_LABEL.into(), "agent".into()),
            ("botler.instance".into(), instance.name.clone()),
        ]),
        network: match def.spec.network {
            Network::Default => NetworkMode::Default,
            Network::None => NetworkMode::None,
        },
        workdir: Some(if def.spec.workspace == Workspace::None { "/".into() } else { "/workspace".into() }),
        remove_on_exit: true,
        ..Default::default()
    })
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
                // pi is up and working: the instance is healthy again.
                hub.agents.crashes.lock().await.remove(&agent.instance);
            }
            PiEventKind::AgentSettled => {
                agent.busy.store(false, Ordering::SeqCst);
                agent.touch();
                if let Err(e) = hub.store.touch_instance(&agent.instance).await {
                    tracing::error!(instance = %agent.instance, error = %e, "could not record activity");
                }
                if let Err(e) = notify_parent(&hub, &agent).await {
                    tracing::error!(instance = %agent.instance, error = %e, "could not notify parent");
                }
                hub.dispatch_notify.notify_one();
            }
            PiEventKind::MessageEnd { usage: Some(u), error, .. } => {
                if let Some(err) = error {
                    tracing::warn!(instance = %agent.instance, %err, "model call failed");
                    if let Err(e) = on_provider_error(&hub, &agent, err).await {
                        tracing::error!(instance = %agent.instance, error = %e, "could not record provider error");
                    }
                }
                if let Err(e) = meter(&hub, &agent, u).await {
                    tracing::error!(instance = %agent.instance, error = %e, "usage metering failed");
                }
            }
            _ => {}
        }
    }
}

/// Auth and billing errors from the model provider will not fix themselves by retrying: pause the
/// agent and open an incident for the user. No triage is dispatched (it would use the same key).
async fn on_provider_error(hub: &Hub, agent: &LiveAgent, err: &str) -> Result<()> {
    let code = err.trim_start().split(|c: char| !c.is_ascii_digit()).next().unwrap_or("");
    let kind = match code {
        "401" | "403" => "provider auth",
        "402" => "provider billing",
        _ => return Ok(()),
    };
    let reason = format!("{kind} error: {}", err.chars().take(300).collect::<String>());
    if hub.store.trip_breaker(&format!("instance:{}", agent.instance), "provider", &reason).await? {
        let (incident, _) = hub.store.record_failure(&format!("agent:{}", agent.instance), "provider", None).await?;
        tracing::error!(instance = %agent.instance, incident = %incident.id, %reason, "agent paused: provider rejected requests");
        if hub
            .live
            .send(Live::Instance { instance: agent.instance.clone(), state: format!("paused: {reason}") })
            .is_err()
        {
            tracing::trace!("no live viewers");
        }
    }
    Ok(())
}

/// Tell the chat that spawned this one that it finished its turn, with its last reply. Chained
/// to the delivery that started the turn so the cycle breaker bounds parent/child ping-pong.
async fn notify_parent(hub: &Hub, agent: &LiveAgent) -> Result<()> {
    let instance = hub.store.instance(&agent.instance).await?;
    let Some(parent) = instance.parent.clone() else { return Ok(()) };
    let last_reply = match agent.pi.command(json!({ "type": "get_last_assistant_text" })).await {
        Ok(d) => d.get("text").and_then(|t| t.as_str()).unwrap_or("").chars().take(4000).collect::<String>(),
        Err(e) => {
            tracing::warn!(instance = %agent.instance, error = %e, "could not read last reply");
            String::new()
        }
    };
    let mut ev = Event::new(kinds::AGENT_IDLE, Source::Agent(instance.name.clone()), Provenance::Trusted, Utc::now())
        .to(parent)
        .payload(json!({ "instance": instance.name, "title": instance.title, "slug": instance.slug, "last_reply": last_reply }));
    if let Some(d) = hub.store.last_delivered(&instance.name).await? {
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
    // Idle or crashed, every instance only goes to sleep: it is woken by its next message with
    // its session and workspace intact. Ending a chat is an explicit act (archiving).
    let state = InstanceState::Dormant;
    if let Err(e) = hub.store.set_instance_state(&agent.instance, state, None).await {
        tracing::error!(instance = %agent.instance, error = %e, "could not record agent exit");
    }
    broadcast_state(&hub, &agent.instance, state.as_str());
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

/// Put idle agents to sleep (they wake on their next message).
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
        if now - agent.last_active() > idle.as_chrono()
            && hub.store.pending_deliveries(&agent.instance).await?.is_empty()
        {
            tracing::info!(instance = %agent.instance, "idle; stopping");
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
    let leftovers =
        hub.containers.list(&BTreeMap::from([(crate::jobs::ROLE_LABEL.to_string(), "agent".to_string())])).await?;
    for c in leftovers {
        hub.containers.remove(&c.name).await?;
    }
    if n > 0 {
        tracing::info!(instances = n, "agents were running before restart; they wake on their next delivery");
    }
    Ok(())
}

fn broadcast_state(hub: &Hub, instance: &str, state: &str) {
    if hub.live.send(Live::Instance { instance: instance.into(), state: state.into() }).is_err() {
        tracing::trace!("no live viewers");
    }
}

/// An urgent note to `main` from the hub.
pub async fn alarm(hub: &Hub, text: &str) -> Result<()> {
    let ev = Event::new(kinds::MESSAGE, Source::Hub, Provenance::Trusted, Utc::now())
        .to("main")
        .urgent(true)
        .payload(json!({ "text": text }));
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
