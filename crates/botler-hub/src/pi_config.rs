//! The files the hub writes into an agent's pi config dir. pi reads `settings.json`,
//! `models.json` and `mcp.json`; the botler extension reads `tools.json`, `turn-tools.json` and
//! `mcp-services.json`, with TypeScript types generated from these (see [`crate::bindings`]).

use std::collections::BTreeMap;

use botler_core::defs::ModelInput;
use serde::Serialize;
use serde_json::Value;

/// `settings.json`.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub steering_mode: SteeringMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Compaction>,
}

/// How pi hands the model messages sent while it works.
#[derive(Debug, PartialEq, Serialize)]
pub enum SteeringMode {
    /// All of them at the next pause, together.
    #[serde(rename = "all")]
    All,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compaction {
    pub reserve_tokens: u64,
    pub keep_recent_tokens: u64,
}

/// `models.json`. pi skips an invalid one silently (and `null` values make it invalid), so
/// only fields that are set are written.
#[derive(Debug, PartialEq, Serialize)]
pub struct Models {
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    /// `$SECRET_NAME`: pi reads it from the environment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<Compat>,
    pub model_overrides: BTreeMap<String, Model>,
}

/// What pi adds to every request to the provider.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Compat {
    pub open_router_routing: OpenRouterRouting,
}

/// How OpenRouter picks which of a model's providers serves a call (its `provider` field).
#[derive(Debug, PartialEq, Serialize)]
pub struct OpenRouterRouting {
    pub sort: &'static str,
}

impl OpenRouterRouting {
    /// The cheapest provider that is up, then the next: at times fewer tokens a second, for a
    /// fraction of the price (one model's providers differ up to 75-fold).
    pub const CHEAPEST: Self = Self { sort: "price" };
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Vec<ModelInput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

/// Prices in USD per million tokens.
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// `mcp.json`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Mcp {
    pub mcp_servers: BTreeMap<String, McpServer>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum McpServer {
    #[serde(rename_all = "camelCase")]
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        description: String,
        tool_exposure: BTreeMap<String, ToolExposure>,
    },
    /// Started by pi inside the container.
    Stdio { command: String, args: Vec<String>, env: BTreeMap<String, String> },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExposure {
    Hidden,
    /// Called as a tool of its own, not from a codemode script.
    Direct,
}

/// A hub op the extension registers as a turn-ending tool (`turn-tools.json`).
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct TurnTool {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub parameters: Value,
}

/// An MCP service the extension connects to (`mcp-services.json`, by service name).
#[derive(Debug, Serialize, ts_rs::TS)]
pub struct McpService {
    pub url: String,
}
