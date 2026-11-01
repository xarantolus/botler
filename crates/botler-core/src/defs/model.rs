//! The file formats of the monorepo. Each struct is one TOML file shape.
//!
//! These types are the contract between agents (who write the files) and the hub (which applies
//! them). JSON Schemas generated from them ship in the template so agents and editors can validate.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Environment;
use crate::duration::HumanDuration;
use crate::limits::RateLimit;

pub const FORMAT_VERSION: u32 = 1;

pub(crate) fn default_version() -> u32 {
    FORMAT_VERSION
}

/// `botler.toml`: instance-wide settings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    #[serde(default = "default_version")]
    pub version: u32,
    /// IANA time zone for schedules, agents' clocks and containers. Unset: the host's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default)]
    pub models: Models,
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub events: EventsPolicy,
    #[serde(default)]
    pub mirror: Option<Mirror>,
    #[serde(default)]
    pub push: Option<Push>,
}

/// How emitted events are checked against declarations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventsPolicy {
    #[serde(default)]
    pub undeclared: Undeclared,
}

/// What happens to an event type that no file declares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Undeclared {
    /// Reject it: every event type must be declared (with a schema to be validated).
    #[default]
    Reject,
    /// Accept it with a warning, for prototyping.
    Warn,
}

/// The provider of every model named by id. Deprecated routes may name another.
pub const OPENROUTER: &str = "openrouter";

/// The models agents use. An agent's `model` and `default` are any OpenRouter model id
/// (`vendor/model`, see <https://openrouter.ai/models>); `model` lists the ones we typically use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// Model id used when an agent does not name one.
    #[serde(default)]
    pub default: Option<String>,
    /// Name of the container secret holding the OpenRouter API key.
    #[serde(default)]
    pub api_key_secret: Option<String>,
    /// Another OpenAI-compatible endpoint serving the same model ids (a gateway, a test stub).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Output token cap per model call (lowered to the model's own maximum). pi otherwise requests
    /// the model's full maximum, which spend-capped provider keys reject.
    #[serde(default = "Models::default_max_tokens")]
    pub max_tokens: u32,
    /// The models we typically use, with what each is good for: suggestions for the web app and
    /// agents, not a whitelist. Also where per-model overrides go.
    #[serde(default, rename = "model", skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<ModelEntry>,
    /// Deprecated: named routes from before agents named model ids. Still resolved by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub routes: BTreeMap<String, ModelRoute>,
}

impl Models {
    fn default_max_tokens() -> u32 {
        16_384
    }

    /// What a model name (an id, or a deprecated route name) resolves to.
    pub fn resolve(&self, name: &str) -> ModelChoice {
        if let Some(r) = self.routes.get(name) {
            return ModelChoice {
                provider: r.provider.clone(),
                id: r.model.clone(),
                base_url: r.base_url.clone(),
                api_key_secret: r.api_key_secret.clone(),
                max_tokens: r.max_tokens,
                input: r.input.clone(),
                thinking: r.thinking.clone(),
            };
        }
        let entry = self.models.iter().find(|m| m.id == name);
        ModelChoice {
            provider: OPENROUTER.into(),
            id: name.into(),
            base_url: self.base_url.clone(),
            api_key_secret: self.api_key_secret.clone(),
            max_tokens: entry.and_then(|e| e.max_tokens).unwrap_or(self.max_tokens),
            input: entry.and_then(|e| e.input.clone()),
            thinking: entry.and_then(|e| e.thinking.clone()),
        }
    }

    /// The secrets used as model API keys, with what uses each (`models`, `model route x`).
    pub fn key_secrets(&self) -> Vec<(String, &str)> {
        let routes = self
            .routes
            .iter()
            .filter_map(|(r, route)| Some((format!("model route {r}"), route.api_key_secret.as_deref()?)));
        self.api_key_secret.as_deref().map(|s| ("models".to_string(), s)).into_iter().chain(routes).collect()
    }
}

impl Default for Models {
    fn default() -> Self {
        Self {
            default: None,
            api_key_secret: None,
            base_url: None,
            max_tokens: Self::default_max_tokens(),
            models: vec![],
            routes: BTreeMap::new(),
        }
    }
}

/// Whether `s` has the shape of an OpenRouter model id: `vendor/model`, no spaces.
pub fn is_model_id(s: &str) -> bool {
    s.split_once('/').is_some_and(|(v, m)| !v.is_empty() && !m.is_empty()) && !s.contains(char::is_whitespace)
}

/// A model we typically use (`[[models.model]]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// OpenRouter model id, e.g. `deepseek/deepseek-v4.1-flash`.
    pub id: String,
    /// What it is good for, in one line.
    #[serde(default)]
    pub note: String,
    /// Output token cap for this model (default: `models.max_tokens`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_tokens: Option<u32>,
    /// What the model takes, e.g. `["text", "image"]` (default: OpenRouter's catalog). Agents only
    /// see images (attachments, `read` of a picture) when it includes `image`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input: Option<Vec<ModelInput>>,
    /// pi thinking level, e.g. `low` or `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub thinking: Option<String>,
}

/// The model an agent runs on, resolved from its `model` (or the default).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ModelChoice {
    /// pi provider id: `openrouter`, unless a deprecated route names another.
    pub provider: String,
    pub id: String,
    pub base_url: Option<String>,
    pub api_key_secret: Option<String>,
    /// The configured output token cap (the hub lowers it to the model's maximum).
    pub max_tokens: u32,
    /// Configured input kinds; unset: OpenRouter's catalog, else pi's own knowledge.
    pub input: Option<Vec<ModelInput>>,
    pub thinking: Option<String>,
}

/// Deprecated: a named route (`[models.routes.<name>]`). `privacy` and `fallback` are ignored.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRoute {
    /// pi provider id, e.g. `openrouter`, `anthropic`, `ollama`.
    pub provider: String,
    pub model: String,
    /// Optional OpenAI-compatible base URL (local models, gateways).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Name of the container secret holding the API key, if any.
    #[serde(default)]
    pub api_key_secret: Option<String>,
    #[serde(default = "Models::default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub thinking: Option<String>,
    #[serde(default)]
    pub input: Option<Vec<ModelInput>>,
    #[serde(default)]
    pub privacy: Option<String>,
    #[serde(default)]
    pub fallback: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ModelInput {
    Text,
    Image,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Defaults {
    #[serde(default = "Defaults::idle")]
    pub idle_timeout: HumanDuration,
    #[serde(default = "Defaults::job_timeout")]
    pub job_timeout: HumanDuration,
    /// Default per-instance budget for agents that do not declare one.
    #[serde(default)]
    pub budget: Budget,
    /// How deep agent trees may grow: an agent at this depth cannot spawn further.
    #[serde(default = "Defaults::max_spawn_depth")]
    pub max_spawn_depth: u32,
    /// How long a spawned agent (and the reviewer) may take to answer before whoever waits on it
    /// is told (`reply.overdue`).
    #[serde(default = "Defaults::reply_within")]
    pub reply_within: HumanDuration,
    /// Archive chats dormant this long (with the agent's handover), or `"never"`. Persistent and
    /// always-on agents are never archived automatically.
    #[serde(default = "Defaults::auto_archive_after", with = "never_or_duration")]
    #[schemars(with = "String")]
    pub auto_archive_after: Option<HumanDuration>,
    /// Persistent agents roll over to a fresh session (seeded with their handover report) when
    /// their context exceeds this many tokens; 0 turns it off.
    #[serde(default = "Defaults::rollover_tokens")]
    pub rollover_tokens: u64,
    /// Agents compact their context (summarize older turns) once it holds about this many tokens,
    /// instead of near the model's full window: every call resends the context, so a long one is
    /// what costs. 0 compacts only near the model's window.
    #[serde(default = "Defaults::compact_at")]
    pub compact_at: u64,
    /// A chat whose last message is older than this is compacted before its next turn (its old
    /// context is no longer cached, so every call of that turn would pay for it in full), or
    /// `"never"`.
    #[serde(default = "Defaults::compact_after_idle", with = "never_or_duration")]
    #[schemars(with = "String")]
    pub compact_after_idle: Option<HumanDuration>,
    /// ...or when their session has been compacted this many times; 0 turns it off.
    #[serde(default = "Defaults::rollover_compactions")]
    pub rollover_compactions: u32,
    /// Before an agent that talked with the user (or did a lot of work) sleeps without having
    /// written memory, it gets one short turn to save what is worth keeping.
    #[serde(default = "Defaults::memory_checkpoint")]
    pub memory_checkpoint: bool,
}

/// `"never"` or a duration.
mod never_or_duration {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::duration::HumanDuration;

    pub fn serialize<S: Serializer>(v: &Option<HumanDuration>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(d) => d.serialize(s),
            None => s.serialize_str("never"),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<HumanDuration>, D::Error> {
        let s = String::deserialize(d)?;
        if s == "never" { Ok(None) } else { s.parse().map(Some).map_err(serde::de::Error::custom) }
    }
}

impl Defaults {
    fn idle() -> HumanDuration {
        HumanDuration::from_secs(15 * 60)
    }
    fn job_timeout() -> HumanDuration {
        HumanDuration::from_secs(10 * 60)
    }
    fn max_spawn_depth() -> u32 {
        8
    }
    fn auto_archive_after() -> Option<HumanDuration> {
        Some(HumanDuration::from_secs(30 * 24 * 3600))
    }
    fn reply_within() -> HumanDuration {
        HumanDuration::from_secs(20 * 60)
    }
    fn rollover_tokens() -> u64 {
        150_000
    }
    fn compact_at() -> u64 {
        150_000
    }
    fn compact_after_idle() -> Option<HumanDuration> {
        Some(HumanDuration::from_secs(4 * 3600))
    }
    fn rollover_compactions() -> u32 {
        3
    }
    fn memory_checkpoint() -> bool {
        true
    }
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            idle_timeout: Self::idle(),
            job_timeout: Self::job_timeout(),
            budget: Budget::default(),
            max_spawn_depth: Self::max_spawn_depth(),
            auto_archive_after: Self::auto_archive_after(),
            reply_within: Self::reply_within(),
            rollover_tokens: Self::rollover_tokens(),
            compact_at: Self::compact_at(),
            compact_after_idle: Self::compact_after_idle(),
            rollover_compactions: Self::rollover_compactions(),
            memory_checkpoint: Self::memory_checkpoint(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    /// Where main is pushed after every merge (e.g. `ssh://git@host:222/me/monorepo.git`), with
    /// the hub's own git and SSH credentials.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Push {
    /// ntfy server URL, e.g. `https://ntfy.sh` or a self-hosted instance.
    pub ntfy_url: String,
    pub topic_secret: String,
}

/// Unit-agnostic budget. Any limit that is set is enforced; unset limits are unbounded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    #[serde(default)]
    pub usd_per_day: Option<f64>,
    #[serde(default)]
    pub tokens_per_day: Option<u64>,
    #[serde(default)]
    pub tokens_per_run: Option<u64>,
    #[serde(default)]
    pub wall_clock_per_run: Option<HumanDuration>,
}

/// `agents/<name>/agent.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agent {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub description: String,
    /// `persistent` agents have exactly one long-lived instance named after the definition.
    /// `ephemeral` agents are templates: each spawn creates a new instance.
    #[serde(default)]
    pub kind: AgentKind,
    /// OpenRouter model id, e.g. `deepseek/deepseek-v4.1-flash`. Unset: `models.default`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub env: Environment,
    #[serde(default)]
    pub idle_timeout: Option<HumanDuration>,
    /// Compact at about this many tokens instead of `defaults.compact_at` (e.g. more for coding
    /// agents that need to keep a lot in view); 0: near the model's window.
    #[serde(default)]
    pub compact_at: Option<u64>,
    /// Keep this agent's pi session running at all times (persistent agents only), e.g. `botler`.
    #[serde(default)]
    pub always_on: bool,
    /// Extra hub capabilities beyond the base agent role.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub budget: Option<Budget>,
    #[serde(default)]
    pub limits: BTreeMap<String, RateLimit>,
    #[serde(default, rename = "trigger")]
    pub triggers: Vec<Trigger>,
}

/// An MCP server pi runs as a subprocess in the agent's container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StdioMcp {
    /// Executable and arguments (not a shell string).
    pub command: Vec<String>,
    /// Extra environment; values may reference container env vars as `${NAME}`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    Persistent,
    #[default]
    Ephemeral,
}

/// What an agent gets besides its home (`/home/agent`, which every agent has).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// Its own clone of the monorepo on its own branch, at `/home/agent/monorepo`.
    #[default]
    Monorepo,
    /// Only the home. Kept for existing definitions: the home is the private persistent volume.
    Volume,
    /// Only the home.
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    #[default]
    Default,
    None,
}

/// Hub capabilities that are not granted to every agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read other instances' sessions.
    ReadSessions,
    /// Spawn other agents. Every agent may spawn now (trees are bounded by `max_spawn_depth`);
    /// kept so definitions that list it still load.
    Spawn,
    /// Submit review verdicts on proposals (the reviewer).
    Review,
    /// Read incidents and run logs (triage).
    Incidents,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadSessions => "read_sessions",
            Self::Spawn => "spawn",
            Self::Review => "review",
            Self::Incidents => "incidents",
        }
    }
}

/// `jobs/<name>/job.toml`: a deterministic container (no LLM).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Job {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub lifetime: Lifetime,
    /// `false` keeps it defined but never run: a service is not started, triggers skip it.
    /// `trial_run` still runs it. Enabling a service needs the user's approval once.
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    /// Command run inside the image built from this directory's Dockerfile. With `from`, empty
    /// runs the image's own command.
    #[serde(default)]
    pub command: Vec<String>,
    /// An upstream image used as it is, instead of a Dockerfile (`docker.io/library/postgres:16`,
    /// with a tag or `@sha256:` digest; never `latest`): a service from a compose file.
    #[serde(default)]
    pub from: Option<String>,
    /// Its runs may clone the monorepo from the hub (`$BOTLER_MONOREPO_URL`, git over HTTP with
    /// the run's token, no keys) and push branches under `agents/<chat>/` for the chat whose event
    /// started the run, which then proposes them. Off by default; turning it on asks the user.
    #[serde(default)]
    pub monorepo: bool,
    /// Plain environment variables (not secrets: those are `secrets` groups), e.g. what a compose
    /// file's `environment:` sets.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout: Option<HumanDuration>,
    #[serde(flatten)]
    pub env: Environment,
    #[serde(default)]
    pub health: Option<Health>,
    #[serde(default)]
    pub restart: Restart,
    /// Where failure incidents go. Defaults to spawning the `triage` agent.
    #[serde(default)]
    pub on_failure: Option<Target>,
    #[serde(default)]
    pub limits: BTreeMap<String, RateLimit>,
    #[serde(default, rename = "trigger")]
    pub triggers: Vec<Trigger>,
    /// Declared event types this job or service emits (listed as their producer).
    #[serde(default)]
    pub emits: Vec<String>,
    /// Services: `http://<name>:<port>/mcp` is an MCP server, so a chat granted the service
    /// (`request_access("service:<name>")`) gets its tools, not only the network path to it.
    #[serde(default)]
    pub serves_mcp: bool,
    /// Services: a channel that carries the user's messages from outside (Telegram, email, ...).
    /// It calls `channel_inbound`, `channel_outbox` and `channel_ack`; senders count as the user
    /// once paired. It can speak as the user, so every change to it asks the user.
    #[serde(default)]
    pub channel: bool,
    /// Services: a web UI the user opens in the web app (Apps), served from the service's port
    /// behind botler's sign-in, on the hub's apps port (another origin than the web app).
    #[serde(default)]
    pub app: Option<AppSpec>,
    /// Models the runnable may call through the hub (`POST /v1/chat/completions` on its socket,
    /// `botler.chat`): OpenRouter ids, or `default` for `models.default`. The hub holds the key,
    /// meters every call, and stops at the budget.
    #[serde(default)]
    pub models: Vec<String>,
    /// Model spending limits (`usd_per_day`, `tokens_per_day`); unset: `defaults.budget`.
    #[serde(default)]
    pub budget: Option<Budget>,
    /// Actions: JSON Schema of the arguments object `invoke_action` must pass.
    #[serde(default)]
    pub args: Option<serde_json::Value>,
}

fn enabled_default() -> bool {
    true
}

impl Job {
    /// An enabled service: the hub keeps it running.
    pub fn is_service(&self) -> bool {
        self.enabled && self.lifetime == Lifetime::Service
    }
}

/// A service port published on the host: `<bind>:<host_port>` forwards to `port` in the container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// Port inside the container.
    pub port: u16,
    /// Port on the host (1024 or above: rootless Podman cannot bind lower ones).
    pub host_port: u16,
    /// Host address to bind: `0.0.0.0` (every interface, the default) or `127.0.0.1` (this machine
    /// only).
    #[serde(default = "Publish::default_bind")]
    pub bind: String,
}

impl Publish {
    fn default_bind() -> String {
        "0.0.0.0".into()
    }

    /// Where it is reachable, in words: "your network at 0.0.0.0:8080" or "this machine at ...".
    pub fn reach(&self) -> String {
        let loopback = self.bind.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
        let whom = if loopback { "this machine only" } else { "your network" };
        format!("{whom} at {}:{}", self.bind, self.host_port)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    #[default]
    Oneshot,
    /// Long-lived; supervised and restarted by the hub.
    Service,
    /// Run only through `invoke_action`, once per approval of its arguments (`args`). The only
    /// runnable that may hold hub-class secrets.
    Action,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// The service must call `health.ok()` at least this often (or pass `command` that often).
    pub heartbeat: HumanDuration,
    /// Time after start before the first heartbeat is required.
    #[serde(default = "Health::grace")]
    pub grace: HumanDuration,
    /// A check the hub runs inside the container instead of the service calling `health.ok()`,
    /// e.g. `["pg_isready"]` or `["curl", "-fsS", "http://localhost:8065/api/v4/system/ping"]`:
    /// each success is a heartbeat. For upstream images that know nothing of botler.
    #[serde(default)]
    pub command: Vec<String>,
}

impl Health {
    fn grace() -> HumanDuration {
        HumanDuration::from_secs(120)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Restart {
    #[serde(default = "Restart::backoff_max")]
    pub backoff_max: HumanDuration,
    /// This many exits within `crashloop_window` mark the service failed.
    #[serde(default = "Restart::crashloop_count")]
    pub crashloop_count: u32,
    #[serde(default = "Restart::crashloop_window")]
    pub crashloop_window: HumanDuration,
}

impl Restart {
    fn backoff_max() -> HumanDuration {
        HumanDuration::from_secs(600)
    }
    fn crashloop_count() -> u32 {
        5
    }
    fn crashloop_window() -> HumanDuration {
        HumanDuration::from_secs(600)
    }
}

impl Default for Restart {
    fn default() -> Self {
        Self {
            backoff_max: Self::backoff_max(),
            crashloop_count: Self::crashloop_count(),
            crashloop_window: Self::crashloop_window(),
        }
    }
}

/// A trigger: *when* something happens. Exactly one of `cron`, `every`, `on`, `webhook`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Trigger {
    /// Unique within its file.
    pub name: String,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub every: Option<HumanDuration>,
    /// Event type glob, e.g. `runnable.failed` or `mail.*`.
    #[serde(default)]
    pub on: Option<String>,
    /// Payload field equality filters for `on` triggers (dotted paths).
    #[serde(default)]
    pub filter: BTreeMap<String, serde_json::Value>,
    /// Webhook name, served at `/hooks/<name>`.
    #[serde(default)]
    pub webhook: Option<String>,
    /// Secret used to verify webhook calls.
    #[serde(default)]
    pub webhook_secret: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub missed: Missed,
    #[serde(default)]
    pub overlap: Overlap,
    /// Message text delivered to agent targets.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Defaults to the owning agent/job when declared inside one.
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default)]
    pub mode: Mode,
}

/// A target: *what* a trigger does.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Deliver into an existing persistent instance.
    Resume(String),
    /// Start a fresh instance of an agent definition.
    Spawn(String),
    /// Run a job.
    Run(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Missed {
    /// Missed fires during downtime are dropped.
    #[default]
    Skip,
    /// If at least one fire was missed, fire once on startup.
    RunOnce,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Overlap {
    /// Don't start a run while the previous one is still running.
    #[default]
    Skip,
    Queue,
    Allow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Live,
    /// Fires are recorded but nothing is started.
    Shadow,
}

/// `triggers/<file>.toml`: standalone trigger bindings.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TriggerFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, rename = "trigger")]
    pub triggers: Vec<Trigger>,
}

/// `secrets.toml`: declarations only, never values. Secrets come in groups: an agent declares a
/// group (e.g. a deploy key plus a host name), the user uploads its values, and definitions are
/// granted whole groups.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, rename = "group")]
    pub groups: Vec<SecretGroupFile>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretGroupFile {
    /// Group name, granted in `secrets = [...]` of agents and jobs.
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub class: SecretClass,
    #[serde(default, rename = "secret")]
    pub secrets: Vec<SecretField>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SecretField {
    /// Unique across all groups; the env var name when delivered as env.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Deliver as a file at this absolute path instead of an env var (container class only).
    #[serde(default)]
    pub file: Option<String>,
}

/// One secret, flattened with its group's class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretDecl {
    pub name: String,
    pub group: String,
    #[serde(default)]
    pub description: String,
    pub class: SecretClass,
    #[serde(default)]
    pub file: Option<String>,
}

/// A secret group as loaded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SecretGroup {
    pub name: String,
    pub description: String,
    pub class: SecretClass,
    /// Secret names, in declaration order.
    pub secrets: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum SecretClass {
    /// Delivered into containers that are granted it (env var or file).
    Container,
    /// Never enters a container; only used by approval-gated hub actions.
    Hub,
}

/// `events/<file>.toml`: event type declarations.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventsFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default, rename = "event")]
    pub events: Vec<EventDecl>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventDecl {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    /// Optional JSON Schema for the payload.
    #[serde(default)]
    pub schema: Option<serde_json::Value>,
    /// Subscribing at runtime needs an access grant (`event:<type>`), e.g. for raw mail contents.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restricted: bool,
}

/// `policies/capabilities.toml`: what forces human approval of a change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicy {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Path globs whose changes always need the user's approval.
    #[serde(default = "CapabilityPolicy::protected")]
    pub protected_paths: Vec<String>,
}

impl CapabilityPolicy {
    /// Paths that are protected even if `capabilities.toml` itself is edited to drop them.
    pub const ALWAYS_PROTECTED: &[&str] = &["policies/**", "gate/**", "secrets.toml", "botler.toml", "images/**"];

    fn protected() -> Vec<String> {
        Self::ALWAYS_PROTECTED.iter().map(|s| (*s).to_string()).collect()
    }
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self { version: FORMAT_VERSION, protected_paths: Self::protected() }
    }
}

/// `[app]` in a service's job.toml.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    /// Shown in the web app's navigation.
    pub title: String,
    /// What to open first, e.g. `/dashboard`. Default `/`.
    #[serde(default)]
    pub path: Option<String>,
    /// A Lucide icon name, e.g. `house`. Default `app-window`.
    #[serde(default)]
    pub icon: Option<String>,
}

/// `tools.manifest.json` / `actions.manifest.json` entries, generated by the SDK in the gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolDecl {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: serde_json::Value,
    /// Declared side effects, e.g. `network`, `fs_write`, `secret:GITLAB_TOKEN`, `outbound`.
    #[serde(default)]
    pub effects: Vec<String>,
    /// For actions: requires an approved proposal before the hub runs it.
    #[serde(default)]
    pub approval: bool,
    /// Python module path, e.g. `botler_tools_github.issues`.
    pub module: String,
    pub function: String,
    /// How the web app shows the tool's result; heuristics when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render: Option<ToolRender>,
}

impl ToolDecl {
    /// The name pi registers the tool under: model APIs accept `[A-Za-z0-9_-]{1,64}`, manifest
    /// names are dotted (`weather.forecast` → `weather_forecast`). Mirrors the pi extension.
    pub fn pi_name(&self) -> String {
        self.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).take(64).collect()
    }
}

/// A render hint for a tool's result, declared with `@tool(render=...)`. Field names are keys of
/// the result object; `items` names the key holding the list (the result itself when absent).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolRender {
    /// Link cards: the title linking to the url, with a snippet.
    Links {
        #[ts(optional)]
        items: Option<String>,
        #[ts(optional)]
        title: Option<String>,
        #[ts(optional)]
        url: Option<String>,
        #[ts(optional)]
        snippet: Option<String>,
    },
    /// A table of the list's objects with these columns (all keys when absent).
    Table {
        #[ts(optional)]
        items: Option<String>,
        #[ts(optional)]
        columns: Option<Vec<String>>,
    },
    /// Sanitized markdown: the result string, or its `field`.
    Markdown {
        #[ts(optional)]
        field: Option<String>,
    },
    /// The object's scalar fields as a key/value list.
    Keyvalue,
    /// An image: the result (a URL, path or data URI), or its `field`.
    Image {
        #[ts(optional)]
        field: Option<String>,
    },
    /// Collapsible JSON.
    Json,
}
