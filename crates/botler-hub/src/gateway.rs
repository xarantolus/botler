//! The model gateway: jobs and services call models through the hub.
//!
//! `POST /v1/chat/completions` (OpenAI's shape) on the hub's socket, with the run's token. The
//! runnable never holds a provider key. The hub checks the model is one its job.toml lists
//! (`models`), stops at its budget (`budget`, else `defaults.budget`), calls the provider with the
//! key it holds, and meters every call under `job:<name>` (the Usage view shows it). Streaming is
//! not offered: jobs want the whole answer.

use botler_core::defs::{Budget, Definitions, ModelChoice};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::hub::{Hub, HubError};
use crate::ops::Caller;

/// Where the gateway listens (on the hub's socket and port, like the API).
pub const PATH: &str = "/v1/chat/completions";
const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
const TIMEOUT_SECONDS: u32 = 300;
/// A probe or a key lookup is short: it must not hold up whoever waits on it.
const QUICK_SECONDS: u32 = 30;

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Budget(String),
    #[error("the model provider could not be reached: {0}")]
    Upstream(String),
    #[error("model calls wait until {provider} accepts them again: {reason}")]
    Held { provider: String, reason: String },
    #[error(transparent)]
    Hub(#[from] HubError),
}

/// The usage key a runnable's calls are metered under.
pub fn usage_key(job: &str) -> String {
    format!("job:{job}")
}

/// The model id `requested` stands for, if the job may call it.
fn allowed(defs: &Definitions, models: &[String], requested: &str) -> Option<String> {
    let default = defs.instance.models.default.as_deref();
    let wanted = if requested == "default" { default? } else { requested };
    let listed = models.iter().any(|m| m == wanted || (m == "default" && default == Some(wanted)));
    listed.then(|| wanted.to_owned())
}

fn day_start(defs: &Definitions, now: DateTime<Utc>) -> DateTime<Utc> {
    now.with_timezone(&defs.timezone)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(defs.timezone).earliest())
        .map_or(now - Duration::hours(24), |t| t.with_timezone(&Utc))
}

/// Answer one chat completion request from `caller` (a job or service run).
pub async fn chat(hub: &Hub, caller: &Caller, mut body: Value) -> Result<(u16, Value), GatewayError> {
    let job = caller.runnable.clone().ok_or_else(|| {
        GatewayError::Forbidden("the model gateway is for jobs and services; agents have their own model".into())
    })?;
    // A trial runs its branch's definition.
    let defs = match crate::trials::of_caller(hub, caller).await? {
        Some(run) => crate::trials::definitions(hub, &run).await?,
        None => hub.applied().await.defs.clone(),
    };
    let def = defs.jobs.get(&job).ok_or_else(|| GatewayError::Forbidden(format!("job {job} is not defined")))?;
    let Some(obj) = body.as_object_mut() else {
        return Err(GatewayError::BadRequest("the body is a JSON object with `messages`".into()));
    };
    if obj.get("stream").and_then(Value::as_bool) == Some(true) {
        return Err(GatewayError::BadRequest("streaming is not offered; leave out `stream`".into()));
    }
    let requested = obj.get("model").and_then(Value::as_str).unwrap_or("default").to_owned();
    let Some(model) = allowed(&defs, &def.spec.models, &requested) else {
        let listed = if def.spec.models.is_empty() { "none".into() } else { def.spec.models.join(", ") };
        return Err(GatewayError::Forbidden(format!(
            "job {job} may not call model {requested:?}; its job.toml lists `models = [{listed}]` (adding one asks the user)"
        )));
    };

    let budget: Budget = def.spec.budget.clone().unwrap_or_else(|| defs.instance.defaults.budget.clone());
    let key = usage_key(&job);
    let now = Utc::now();
    let today = hub.store.usage_since(Some(&key), day_start(&defs, now)).await.map_err(HubError::from)?;
    if let Some(usd) = budget.usd_per_day
        && today.cost_usd >= usd
    {
        return Err(GatewayError::Budget(format!(
            "job {job} spent ${:.2} on models today (budget ${usd:.2})",
            today.cost_usd
        )));
    }
    if let Some(tok) = budget.tokens_per_day
        && today.tokens >= i64::try_from(tok).unwrap_or(i64::MAX)
    {
        return Err(GatewayError::Budget(format!("job {job} used {} model tokens today (budget {tok})", today.tokens)));
    }

    let choice = defs.instance.models.resolve(&model);
    obj.insert("model".into(), json!(choice.id));
    if choice.provider == botler_core::defs::OPENROUTER {
        // OpenRouter reports the cost in the response.
        obj.insert("usage".into(), json!({ "include": true }));
        // Unless the job picks providers itself: the cheapest, as for agents.
        if !obj.contains_key("provider") {
            obj.insert("provider".into(), json!(crate::pi_config::OpenRouterRouting::CHEAPEST));
        }
    }
    if let Some(b) = hub.store.breaker(&crate::holds::subject(&choice.provider)).await.map_err(HubError::from)? {
        return Err(GatewayError::Held { provider: choice.provider.clone(), reason: b.reason });
    }
    let api_key = api_key(hub, &choice).await?;
    let (status, reply) =
        call(hub, &completions_url(&choice), api_key.as_deref(), Some(&body), TIMEOUT_SECONDS).await?;

    let usage = reply.get("usage");
    let tokens = usage.and_then(|u| u.get("total_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let cached =
        usage.and_then(|u| u.pointer("/prompt_tokens_details/cached_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let cost = usage.and_then(|u| u.get("cost")).and_then(Value::as_f64).unwrap_or(0.0);
    if tokens > 0 || cost > 0.0 {
        hub.store.record_call(&key, now, &choice.id, tokens, cached, cost).await.map_err(HubError::from)?;
    }
    tracing::info!(%job, model = %choice.id, status, tokens, cost, "model call");
    Ok((status, reply))
}

/// The hub's key for `choice`, if it uses one.
async fn api_key(hub: &Hub, choice: &ModelChoice) -> Result<Option<String>, GatewayError> {
    let Some(s) = &choice.api_key_secret else { return Ok(None) };
    let value = hub.store.secret(&hub.secret_key, s).await.map_err(HubError::from)?;
    value
        .map(Some)
        .ok_or_else(|| GatewayError::Forbidden(format!("the model key {s} has no value; set it on the Secrets page")))
}

fn completions_url(choice: &ModelChoice) -> String {
    let base = choice.base_url.as_deref().unwrap_or(OPENROUTER_URL);
    format!("{}/chat/completions", base.trim_end_matches('/'))
}

/// Whether `choice`'s provider takes a one-token call with the hub's key again: what lifts a
/// provider hold. `Err` says why not.
pub async fn probe(hub: &Hub, choice: &ModelChoice) -> Result<(), String> {
    let api_key = api_key(hub, choice).await.map_err(|e| e.to_string())?;
    let body = json!({ "model": choice.id, "max_tokens": 1, "messages": [{ "role": "user", "content": "ping" }] });
    let (status, reply) = call(hub, &completions_url(choice), api_key.as_deref(), Some(&body), QUICK_SECONDS)
        .await
        .map_err(|e| e.to_string())?;
    if status == 200 {
        return Ok(());
    }
    let msg = reply.pointer("/error/message").and_then(Value::as_str).unwrap_or("no message");
    Err(format!("{status}: {msg}"))
}

/// What OpenRouter counts for the hub's model key: what it charged, where botler's own figures
/// for agents are estimates at each model's listed price. `None` when models do not go through
/// OpenRouter, or the key is not set.
pub async fn openrouter_key(hub: &Hub) -> Result<Option<ProviderKey>, GatewayError> {
    let applied = hub.applied().await;
    // OpenRouter models use the `[models]` key and URL (routes to other providers their own).
    let models = &applied.defs.instance.models;
    if models.base_url.is_some() {
        return Ok(None);
    }
    let Some(secret) = &models.api_key_secret else { return Ok(None) };
    let Some(key) = hub.store.secret(&hub.secret_key, secret).await.map_err(HubError::from)? else { return Ok(None) };
    let (status, reply) = call(hub, &format!("{OPENROUTER_URL}/key"), Some(&key), None, QUICK_SECONDS).await?;
    if status != 200 {
        let msg = reply.pointer("/error/message").and_then(Value::as_str).unwrap_or("no message");
        return Err(GatewayError::Upstream(format!("OpenRouter answered {status}: {msg}")));
    }
    let data = reply.get("data").cloned().unwrap_or(Value::Null);
    serde_json::from_value(data).map(Some).map_err(|e| GatewayError::Upstream(format!("unexpected key info: {e}")))
}

/// OpenRouter's figures for one key (`GET /api/v1/key`), in USD.
#[derive(Debug, serde::Deserialize, serde::Serialize, ts_rs::TS)]
pub struct ProviderKey {
    pub label: String,
    /// Since the key was made.
    pub usage: f64,
    pub usage_daily: f64,
    pub usage_weekly: f64,
    pub usage_monthly: f64,
    pub limit: Option<f64>,
    pub limit_remaining: Option<f64>,
    /// How often the limit starts over (`daily`, `weekly`, `monthly`), if it does.
    pub limit_reset: Option<String>,
}

/// Send `body` to `url` with curl (the hub's HTTPS client), or GET it without one. The key goes
/// in on stdin, never on the command line.
async fn call(
    hub: &Hub,
    url: &str,
    key: Option<&str>,
    body: Option<&Value>,
    timeout: u32,
) -> Result<(u16, Value), GatewayError> {
    if url.contains(['"', '\\', '\n', '\r']) || key.is_some_and(|k| k.contains(['"', '\\', '\n', '\r'])) {
        return Err(GatewayError::BadRequest("the model URL or key contains characters curl cannot take".into()));
    }
    let mut config = format!("url = \"{url}\"\n");
    let file = match body {
        Some(body) => {
            let dir = hub.paths.root.join("tmp");
            tokio::fs::create_dir_all(&dir).await.map_err(HubError::from)?;
            let file = dir.join(format!("chat-{}.json", uuid::Uuid::now_v7().simple()));
            tokio::fs::write(&file, body.to_string()).await.map_err(HubError::from)?;
            config.push_str(&format!(
                "header = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
                file.display()
            ));
            Some(file)
        }
        None => None,
    };
    if let Some(k) = key {
        config.push_str(&format!("header = \"Authorization: Bearer {k}\"\n"));
    }
    let result = async {
        let mut child = tokio::process::Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", &timeout.to_string()])
            .args(["--write-out", "\n%{http_code}", "--config", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| GatewayError::Upstream(format!("cannot run curl: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(config.as_bytes()).await.map_err(HubError::from)?;
        }
        let out = child.wait_with_output().await.map_err(HubError::from)?;
        if !out.status.success() {
            return Err(GatewayError::Upstream(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let (json_text, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
        let status: u16 = code.trim().parse().unwrap_or(502);
        let reply = serde_json::from_str(json_text)
            .unwrap_or_else(|_| json!({ "error": { "message": json_text.chars().take(500).collect::<String>() } }));
        Ok((status, reply))
    }
    .await;
    if let Some(file) = file
        && let Err(e) = tokio::fs::remove_file(&file).await
    {
        tracing::warn!(file = %file.display(), error = %e, "could not remove a model request file");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_models() {
        let mut defs = Definitions::default();
        defs.instance.models.default = Some("qwen/qwen3".into());
        let listed = vec!["default".to_string(), "openai/gpt-5-mini".into()];
        assert_eq!(allowed(&defs, &listed, "default").as_deref(), Some("qwen/qwen3"));
        assert_eq!(allowed(&defs, &listed, "qwen/qwen3").as_deref(), Some("qwen/qwen3"));
        assert_eq!(allowed(&defs, &listed, "openai/gpt-5-mini").as_deref(), Some("openai/gpt-5-mini"));
        assert_eq!(allowed(&defs, &listed, "anthropic/claude-opus"), None);
        assert_eq!(allowed(&defs, &[], "default"), None);
    }
}
