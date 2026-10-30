//! Everything agents, jobs and the user can ask the hub to do. Transport-neutral: the MCP server,
//! the SDK's HTTP API and the web app all call these, so permissions, rate limits and validation
//! live in one place.

use botler_core::defs::{Capability, Target};
use botler_core::duration::HumanDuration;
use botler_core::event::kinds;
use botler_core::limits::{RateLimit, default_limit};
use botler_core::{Event, Provenance, Source};
use botler_store::{DialogKind, EventQuery, NewDialog, NewTimer, Priority};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::router;

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error("invalid arguments: {0}")]
    BadRequest(String),
    #[error("not allowed: {0}")]
    Forbidden(String),
    #[error("unknown operation {0}")]
    Unknown(String),
}

impl From<botler_store::Error> for OpError {
    fn from(e: botler_store::Error) -> Self {
        Self::Hub(e.into())
    }
}

pub type OpResult = Result<Value, OpError>;

/// Who is calling, resolved from their token.
#[derive(Clone, Debug)]
pub struct Caller {
    pub principal: Principal,
    /// For agents: their definition's capabilities.
    pub capabilities: Vec<Capability>,
    /// For runs: the runnable.
    pub runnable: Option<String>,
}

impl Caller {
    pub async fn resolve(hub: &Hub, principal: Principal) -> Result<Self, OpError> {
        let applied = hub.applied().await;
        match &principal {
            Principal::User => Ok(Self { principal, capabilities: vec![], runnable: None }),
            Principal::Agent { instance: name, wake } => {
                if !hub.agents.is_current(name, wake).await {
                    return Err(OpError::Forbidden(format!("token belongs to a stopped session of {name}")));
                }
                let inst = hub
                    .store
                    .find_instance(name)
                    .await?
                    .ok_or_else(|| OpError::Forbidden(format!("unknown instance {name}")))?;
                let capabilities =
                    applied.defs.agents.get(&inst.definition).map(|d| d.spec.capabilities.clone()).unwrap_or_default();
                Ok(Self { principal, capabilities, runnable: None })
            }
            Principal::Run { run_id } => {
                let run = hub.store.run(run_id).await?;
                if run.status.is_terminal() {
                    return Err(OpError::Forbidden(format!("run {run_id} has finished")));
                }
                Ok(Self { principal, capabilities: vec![], runnable: Some(run.runnable) })
            }
        }
    }

    pub fn source(&self) -> Source {
        match &self.principal {
            Principal::User => Source::User,
            Principal::Agent { instance, .. } => Source::Agent(instance.clone()),
            Principal::Run { .. } => Source::Runnable(self.runnable.clone().unwrap_or_default()),
        }
    }

    /// Rate-limit subject.
    fn subject(&self) -> String {
        self.source().key()
    }

    fn instance(&self) -> Option<&str> {
        match &self.principal {
            Principal::Agent { instance, .. } => Some(instance),
            _ => None,
        }
    }

    fn require(&self, cap: Capability) -> Result<(), OpError> {
        if matches!(self.principal, Principal::User) || self.capabilities.contains(&cap) {
            Ok(())
        } else {
            Err(OpError::Forbidden(format!("needs the `{}` capability", cap.as_str())))
        }
    }

    fn may(&self, cap: Capability) -> bool {
        self.require(cap).is_ok()
    }
}

/// One operation as advertised to agents.
pub struct OpSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
    /// Who sees it; agents only see tools they may call.
    pub visible: fn(&Caller) -> bool,
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|e| json!({ "type": "object", "description": format!("schema error: {e}") }))
}

fn everyone(_: &Caller) -> bool {
    true
}
fn agents(c: &Caller) -> bool {
    !matches!(c.principal, Principal::Run { .. })
}
fn spawners(c: &Caller) -> bool {
    c.may(Capability::Spawn)
}
fn session_readers(c: &Caller) -> bool {
    c.may(Capability::ReadSessions)
}
fn incident_handlers(c: &Caller) -> bool {
    c.may(Capability::Incidents)
}
fn runs(c: &Caller) -> bool {
    matches!(c.principal, Principal::Run { .. })
}

pub const OPS: &[OpSpec] = &[
    OpSpec {
        name: "send_message",
        description: "Send a message to an agent instance (e.g. `main`). Replies come back to you as messages. Set `reply_to` to the event you are answering to keep the thread.",
        schema: schema::<SendMessage>,
        visible: everyone,
    },
    OpSpec {
        name: "emit_event",
        description: "Emit a typed event. Triggers subscribed to its type react to it. Use for facts (`mail.received`), not for talking to someone (use send_message).",
        schema: schema::<EmitEvent>,
        visible: everyone,
    },
    OpSpec {
        name: "set_timer",
        description: "Wake an agent later with a prompt: at an RFC 3339 time or after a delay like \"2h\". Defaults to waking you. One-off only; recurring work belongs in a trigger file.",
        schema: schema::<SetTimer>,
        visible: everyone,
    },
    OpSpec {
        name: "cancel_timer",
        description: "Cancel a pending timer.",
        schema: schema::<CancelTimer>,
        visible: everyone,
    },
    OpSpec {
        name: "list_timers",
        description: "Pending timers you created (all timers for the user).",
        schema: schema::<Empty>,
        visible: agents,
    },
    OpSpec {
        name: "set_chat_title",
        description: "Name your chat so the user can find it: a short title and a unique slug like `groundstation-antenna-fix` (lowercase words joined by dashes, project or topic first). Others can then message you by the slug. Do this after the first message of a new chat, and again if the topic changes.",
        schema: schema::<SetChatTitle>,
        visible: agents,
    },
    OpSpec {
        name: "spawn_agent",
        description: "Start a new instance of an agent definition with a task. It reports back to you by message.",
        schema: schema::<SpawnAgent>,
        visible: spawners,
    },
    OpSpec {
        name: "list_agents",
        description: "Agent definitions and live instances with their state.",
        schema: schema::<Empty>,
        visible: agents,
    },
    OpSpec {
        name: "read_session",
        description: "Read the recent messages of another instance's session (what it has been doing).",
        schema: schema::<ReadSession>,
        visible: session_readers,
    },
    OpSpec {
        name: "get_events",
        description: "Read the event journal, newest first: filter by type (`mail.*`) or conversation (`correlation`).",
        schema: schema::<GetEvents>,
        visible: agents,
    },
    OpSpec {
        name: "ask_user",
        description: "Ask the user a question and get the answer later as a `dialog.answered` event. `options` are the allowed answers; `about` lists objects (e.g. `proposal:<id>`) whose change withdraws the question.",
        schema: schema::<AskUser>,
        visible: agents,
    },
    OpSpec { name: "get_run_logs", description: "Output of a job run.", schema: schema::<GetRunLogs>, visible: agents },
    OpSpec {
        name: "get_incident",
        description: "One incident: what failed, how often, how it was resolved.",
        schema: schema::<IdArg>,
        visible: incident_handlers,
    },
    OpSpec {
        name: "get_incident_history",
        description: "Past incidents of a job or service with their resolutions, newest first. Check this before fixing anything.",
        schema: schema::<RunnableArg>,
        visible: incident_handlers,
    },
    OpSpec {
        name: "resolve_incident",
        description: "Close an incident with what caused it and what you did (or why nothing needs doing). Required at the end of every triage.",
        schema: schema::<ResolveIncident>,
        visible: incident_handlers,
    },
    OpSpec {
        name: "state_get",
        description: "Read this job's persistent key-value state.",
        schema: schema::<StateKey>,
        visible: runs,
    },
    OpSpec {
        name: "state_set",
        description: "Write this job's persistent key-value state (cursors, dedup keys; up to 64 KiB per value).",
        schema: schema::<StateSet>,
        visible: runs,
    },
];

#[derive(Debug, Deserialize, JsonSchema)]
pub struct Empty {}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SendMessage {
    /// Recipient instance name.
    pub to: String,
    pub text: String,
    /// Interrupt the recipient at its next turn instead of waiting until it is idle.
    #[serde(default)]
    pub urgent: bool,
    /// Event id this answers, to keep the conversation thread.
    #[serde(default)]
    pub reply_to: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitEvent {
    /// Dotted event type, e.g. `mail.received`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
    /// `external` (from a known outside source) or `untrusted` (arbitrary content). Defaults to
    /// `external`; events from agents and jobs are never `trusted`.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Deserialize, JsonSchema)]
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

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CancelTimer {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetChatTitle {
    pub title: String,
    /// Lowercase words joined by dashes, 3 to 60 characters.
    pub slug: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnAgent {
    /// Agent definition under `agents/`, e.g. `dev`.
    pub definition: String,
    pub task: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadSession {
    pub instance: String,
    /// How many of the most recent messages (default 20, max 200).
    #[serde(default)]
    pub last: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetEvents {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub correlation: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskUser {
    pub question: String,
    /// Allowed answers, e.g. ["yes", "no"]. Empty means free text.
    #[serde(default)]
    pub options: Vec<String>,
    /// Background the user needs to answer.
    #[serde(default)]
    pub detail: Option<String>,
    /// Objects whose change withdraws the question, e.g. `proposal:<id>`.
    #[serde(default)]
    pub about: Vec<String>,
    /// `blocking` (you cannot continue; notifies the user), `normal`, or `info`.
    #[serde(default)]
    pub priority: Option<String>,
    /// Withdraw the question after this long, e.g. `1d`.
    #[serde(default)]
    pub expires_in: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetRunLogs {
    pub run_id: String,
    /// Last N lines (default 200).
    #[serde(default)]
    pub tail: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdArg {
    pub id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunnableArg {
    pub runnable: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ResolveIncident {
    pub id: String,
    /// Cause and what you did. Future triage reads this.
    pub resolution: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StateKey {
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct StateSet {
    pub key: String,
    pub value: String,
}

fn args<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, OpError> {
    serde_json::from_value(if v.is_null() { json!({}) } else { v }).map_err(|e| OpError::BadRequest(e.to_string()))
}

async fn limit(hub: &Hub, caller: &Caller, op: &str) -> Result<(), OpError> {
    let applied = hub.applied().await;
    let custom: Option<RateLimit> = match &caller.principal {
        Principal::Agent { instance: name, .. } => match hub.store.find_instance(name).await? {
            Some(i) => applied.defs.agents.get(&i.definition).and_then(|d| d.spec.limits.get(op).copied()),
            None => None,
        },
        Principal::Run { .. } => {
            caller.runnable.as_ref().and_then(|r| applied.defs.jobs.get(r)).and_then(|j| j.spec.limits.get(op).copied())
        }
        Principal::User => return Ok(()), // the user is not rate limited
    };
    hub.rate_limit(&caller.subject(), op, custom.unwrap_or_else(|| default_limit(op))).await.map_err(Into::into)
}

/// Run operation `name` for `caller`.
pub async fn call(hub: &Hub, caller: &Caller, name: &str, input: Value) -> OpResult {
    let spec = OPS.iter().find(|o| o.name == name).ok_or_else(|| OpError::Unknown(name.into()))?;
    if !(spec.visible)(caller) {
        return Err(OpError::Forbidden(format!("{name} is not available to you")));
    }
    limit(hub, caller, name).await?;
    match name {
        "send_message" => send_message(hub, caller, args(input)?).await,
        "emit_event" => emit_event(hub, caller, args(input)?).await,
        "set_timer" => set_timer(hub, caller, args(input)?).await,
        "cancel_timer" => cancel_timer(hub, caller, args(input)?).await,
        "list_timers" => list_timers(hub, caller).await,
        "set_chat_title" => set_chat_title(hub, caller, args(input)?).await,
        "spawn_agent" => spawn_agent(hub, caller, args(input)?).await,
        "list_agents" => list_agents(hub).await,
        "read_session" => read_session(hub, args(input)?).await,
        "get_events" => get_events(hub, args(input)?).await,
        "ask_user" => ask_user(hub, caller, args(input)?).await,
        "get_run_logs" => get_run_logs(hub, args(input)?).await,
        "get_incident" => {
            let a: IdArg = args(input)?;
            Ok(json!(hub.store.incident(&a.id).await?))
        }
        "get_incident_history" => {
            let a: RunnableArg = args(input)?;
            Ok(json!(hub.store.incidents_for(&a.runnable, 20).await?))
        }
        "resolve_incident" => {
            let a: ResolveIncident = args(input)?;
            if a.resolution.trim().len() < 10 {
                return Err(OpError::BadRequest("describe the cause and what you did".into()));
            }
            hub.store.resolve_incident(&a.id, &a.resolution).await?;
            Ok(json!({ "resolved": a.id }))
        }
        "state_get" => {
            let a: StateKey = args(input)?;
            Ok(json!({ "value": hub.store.kv_get(&caller.subject(), &a.key).await? }))
        }
        "state_set" => {
            let a: StateSet = args(input)?;
            hub.store.kv_set(&caller.subject(), &a.key, &a.value).await?;
            Ok(json!({ "ok": true }))
        }
        other => Err(OpError::Unknown(other.into())),
    }
}

async fn send_message(hub: &Hub, caller: &Caller, a: SendMessage) -> OpResult {
    let to = match hub.store.resolve_instance(&a.to).await? {
        Some(i) => i.name,
        None if hub.applied().await.defs.agents.contains_key(&a.to) => a.to.clone(),
        None => return Err(OpError::BadRequest(format!("no agent instance or chat named {}", a.to))),
    };
    // Messages between agents are trusted as messages; content an agent relays from outside is
    // marked by the event it came from (replies inherit the lower provenance of their parent).
    let mut ev = Event::new(kinds::MESSAGE, caller.source(), Provenance::Trusted, Utc::now())
        .to(to.clone())
        .urgent(a.urgent)
        .payload(json!({ "text": a.text }));
    if let Some(parent) = &a.reply_to {
        let parent = hub.store.event(parent).await?;
        ev = ev.in_reply_to(&parent);
    }
    let ev = hub.emit(ev).await?;
    Ok(json!({ "event_id": ev.id, "to": to }))
}

async fn emit_event(hub: &Hub, caller: &Caller, a: EmitEvent) -> OpResult {
    if a.kind.is_empty()
        || !a.kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    {
        return Err(OpError::BadRequest("type must be lowercase dotted, e.g. mail.received".into()));
    }
    if botler_core::catalog::builtin(&a.kind).is_some() {
        return Err(OpError::Forbidden(format!("{} is a hub event type", a.kind)));
    }
    let provenance = match (caller.principal == Principal::User, a.provenance) {
        (true, p) => p.unwrap_or(Provenance::Trusted),
        // Agents and jobs can never claim trusted content.
        (false, Some(Provenance::Untrusted)) => Provenance::Untrusted,
        (false, _) => Provenance::External,
    };
    let ev = hub.emit(Event::new(a.kind, caller.source(), provenance, Utc::now()).payload(a.payload)).await?;
    Ok(json!({ "event_id": ev.id }))
}

fn parse_when(at: Option<&str>, after: Option<&str>, now: DateTime<Utc>) -> Result<DateTime<Utc>, OpError> {
    match (at, after) {
        (Some(at), None) => DateTime::parse_from_rfc3339(at)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| OpError::BadRequest(format!("`at` must be RFC 3339: {e}"))),
        (None, Some(d)) => {
            let d: HumanDuration =
                d.parse().map_err(|e| OpError::BadRequest(format!("`in` must be a duration like 20m: {e}")))?;
            Ok(now + d.as_chrono())
        }
        _ => Err(OpError::BadRequest("give exactly one of `at` or `in`".into())),
    }
}

async fn set_timer(hub: &Hub, caller: &Caller, a: SetTimer) -> OpResult {
    let now = Utc::now();
    let fire_at = parse_when(a.at.as_deref(), a.after.as_deref(), now)?;
    if fire_at < now - chrono::Duration::minutes(1) {
        return Err(OpError::BadRequest(format!("{fire_at} is in the past")));
    }
    let target = match a.instance.or_else(|| caller.instance().map(String::from)) {
        Some(i) => {
            let i = match hub.store.resolve_instance(&i).await? {
                Some(inst) => inst.name,
                None if hub.applied().await.defs.agents.contains_key(&i) => i,
                None => return Err(OpError::BadRequest(format!("no agent instance or chat named {i}"))),
            };
            Target::Resume(i)
        }
        None => return Err(OpError::BadRequest("`instance` is required".into())),
    };
    let t = hub
        .store
        .create_timer(&NewTimer {
            fire_at,
            target,
            prompt: Some(a.prompt),
            payload: json!({}),
            created_by: caller.subject(),
        })
        .await?;
    hub.schedule_notify.notify_one();
    let tz = hub.applied().await.defs.timezone;
    Ok(
        json!({ "timer_id": t.id, "fires_at": fire_at, "local": fire_at.with_timezone(&tz).format("%a %Y-%m-%d %H:%M %Z").to_string() }),
    )
}

async fn cancel_timer(hub: &Hub, caller: &Caller, a: CancelTimer) -> OpResult {
    let t = hub.store.timer(&a.id).await?;
    if caller.principal != Principal::User && t.created_by != caller.subject() {
        return Err(OpError::Forbidden("you can only cancel your own timers".into()));
    }
    let cancelled = hub.store.set_timer_status(&a.id, botler_store::TimerStatus::Cancelled).await?;
    Ok(json!({ "cancelled": cancelled }))
}

async fn list_timers(hub: &Hub, caller: &Caller) -> OpResult {
    let mine = (caller.principal != Principal::User).then(|| caller.subject());
    Ok(json!(hub.store.pending_timers(mine.as_deref()).await?))
}

async fn set_chat_title(hub: &Hub, caller: &Caller, a: SetChatTitle) -> OpResult {
    let Some(instance) = caller.instance() else { return Err(OpError::Forbidden("only agents name chats".into())) };
    let slug_ok = (3..=60).contains(&a.slug.len())
        && botler_core::defs::valid_name(&a.slug)
        && !a.slug.ends_with('-')
        && !a.slug.contains("--");
    if !slug_ok {
        return Err(OpError::BadRequest("slug must be 3-60 lowercase letters/digits in dash-separated words".into()));
    }
    let title = a.title.trim();
    if title.is_empty() || title.len() > 120 {
        return Err(OpError::BadRequest("title must be 1-120 characters".into()));
    }
    hub.store.set_instance_title(instance, title, &a.slug).await.map_err(|e| match e {
        botler_store::Error::SlugTaken(s) => OpError::BadRequest(format!("{s} is taken; pick another slug")),
        other => other.into(),
    })?;
    Ok(json!({ "instance": instance, "title": title, "slug": a.slug }))
}

async fn spawn_agent(hub: &Hub, caller: &Caller, a: SpawnAgent) -> OpResult {
    let name = router::spawn(hub, &a.definition, caller.instance(), Some(&a.task), None, None).await?;
    Ok(json!({ "instance": name }))
}

async fn list_agents(hub: &Hub) -> OpResult {
    let applied = hub.applied().await;
    let defs: Vec<Value> = applied
        .defs
        .agents
        .values()
        .map(|d| json!({ "definition": d.name, "description": d.spec.description, "kind": d.spec.kind }))
        .collect();
    let live = hub.store.live_instances().await?;
    let mut instances = Vec::new();
    for i in live {
        let busy = hub.agents.get(&i.name).await.is_some_and(|a| a.busy());
        instances.push(json!({ "instance": i.name, "definition": i.definition, "state": i.state, "busy": busy, "task": i.task, "last_active": i.last_active }));
    }
    Ok(json!({ "definitions": defs, "instances": instances }))
}

async fn read_session(hub: &Hub, a: ReadSession) -> OpResult {
    let inst = hub.store.instance(&a.instance).await?;
    let path = hub.paths.sessions(&inst.name).join(&inst.session_file);
    let last = a.last.unwrap_or(20).min(200) as usize;
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({ "messages": [] })),
        Err(e) => return Err(HubError::Io(e).into()),
    };
    let msgs: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|e| e.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|e| {
            let m = e.get("message")?;
            let role = m.get("role")?.as_str()?;
            if role == "system" {
                return None;
            }
            let text: String = m
                .get("content")
                .and_then(Value::as_array)
                .map(|c| c.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            Some(json!({ "role": role, "text": text.chars().take(4000).collect::<String>(), "at": e.get("timestamp") }))
        })
        .collect();
    let start = msgs.len().saturating_sub(last);
    Ok(json!({ "instance": inst.name, "messages": msgs[start..] }))
}

async fn get_events(hub: &Hub, a: GetEvents) -> OpResult {
    let q = EventQuery { kind: a.kind, correlation: a.correlation, since: None, limit: a.limit.unwrap_or(20).min(200) };
    Ok(json!(hub.store.events(&q).await?))
}

async fn ask_user(hub: &Hub, caller: &Caller, a: AskUser) -> OpResult {
    let Some(instance) = caller.instance() else { return Err(OpError::Forbidden("only agents ask the user".into())) };
    let priority = match a.priority.as_deref().unwrap_or("blocking") {
        "blocking" => Priority::Blocking,
        "normal" => Priority::Normal,
        "info" => Priority::Info,
        other => return Err(OpError::BadRequest(format!("unknown priority {other}"))),
    };
    let expires_at = a.expires_in.as_deref().map(|d| parse_when(None, Some(d), Utc::now())).transpose()?;
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: instance.into(),
            kind: DialogKind::Question,
            question: a.question,
            detail: a.detail,
            ctas: a.options,
            about: a.about,
            priority,
            proposal_id: None,
            expires_at,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(
        json!({ "dialog_id": d.id, "note": "The answer arrives later as a dialog.answered event; continue with other work or stop." }),
    )
}

async fn get_run_logs(hub: &Hub, a: GetRunLogs) -> OpResult {
    let run = hub.store.run(&a.run_id).await?;
    let saved = hub.paths.run_log(&run.id);
    let logs = match tokio::fs::read_to_string(&saved).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Still running: read live.
            let name = crate::jobs::container_name(&run.id);
            hub.containers.logs(&name, 2000).await.map_err(HubError::from)?
        }
        Err(e) => return Err(HubError::Io(e).into()),
    };
    Ok(json!({ "run": run, "logs": crate::failures::tail(&logs, a.tail.unwrap_or(200)) }))
}
