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

fn default_version() -> u32 {
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

/// The model routing table. Agents name a route, never a model.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Models {
    /// Route used when an agent does not name one.
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub routes: BTreeMap<String, ModelRoute>,
}

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
    /// Output token cap per model call. pi otherwise requests the model's full maximum, which
    /// spend-capped provider keys reject.
    #[serde(default = "ModelRoute::default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub thinking: Option<String>,
    /// `local` routes may never fall back to a hosted route.
    #[serde(default)]
    pub privacy: Privacy,
    /// Ordered fallbacks (route names).
    #[serde(default)]
    pub fallback: Vec<String>,
}

impl ModelRoute {
    fn default_max_tokens() -> u32 {
        16_384
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Privacy {
    #[default]
    Hosted,
    Local,
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
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            idle_timeout: Self::idle(),
            job_timeout: Self::job_timeout(),
            budget: Budget::default(),
            max_spawn_depth: Self::max_spawn_depth(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    pub url: String,
    /// Container secret holding push credentials (token or SSH key file).
    #[serde(default)]
    pub secret: Option<String>,
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
    /// Model route name from `botler.toml`.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub env: Environment,
    #[serde(default)]
    pub idle_timeout: Option<HumanDuration>,
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
    /// Command run inside the image built from this directory's Dockerfile.
    pub command: Vec<String>,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    #[default]
    Oneshot,
    /// Long-lived; supervised and restarted by the hub.
    Service,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Health {
    /// The service must call `health.ok()` at least this often.
    pub heartbeat: HumanDuration,
    /// Time after start before the first heartbeat is required.
    #[serde(default = "Health::grace")]
    pub grace: HumanDuration,
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
}
