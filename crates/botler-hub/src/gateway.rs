//! The model gateway: jobs and services call models through the hub.
//!
//! `POST /v1/chat/completions` (OpenAI's shape) on the hub's socket, with the run's token. The
//! runnable never holds a provider key. The hub checks the model is one its job.toml lists
//! (`models`), stops at its budget (`budget`, else `defaults.budget`), calls the provider with the
//! key it holds, and meters every call under `job:<name>` (the Usage view shows it). Streaming is
//! not offered: jobs want the whole answer.

use botler_core::defs::{Budget, Definitions};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;

use crate::hub::{Hub, HubError};
use crate::ops::Caller;

/// Where the gateway listens (on the hub's socket and port, like the API).
pub const PATH: &str = "/v1/chat/completions";
pub const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";
const TIMEOUT_SECONDS: u32 = 300;

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
    }
    let api_key = match &choice.api_key_secret {
        Some(s) => Some(hub.store.secret(&hub.secret_key, s).await.map_err(HubError::from)?.ok_or_else(|| {
            GatewayError::Forbidden(format!("the model key {s} has no value; set it on the Secrets page"))
        })?),
        None => None,
    };
    let base = choice.base_url.clone().unwrap_or_else(|| OPENROUTER_URL.into());
    let (status, reply) =
        post(hub, &format!("{}/chat/completions", base.trim_end_matches('/')), api_key.as_deref(), &body).await?;

    let usage = reply.get("usage");
    let tokens = usage.and_then(|u| u.get("total_tokens")).and_then(Value::as_i64).unwrap_or(0);
    let cost = usage.and_then(|u| u.get("cost")).and_then(Value::as_f64).unwrap_or(0.0);
    if tokens > 0 || cost > 0.0 {
        hub.store.record_usage(&key, now, &choice.id, tokens, cost).await.map_err(HubError::from)?;
    }
    tracing::info!(%job, model = %choice.id, status, tokens, cost, "model call");
    Ok((status, reply))
}

/// POST `body` to `url` with curl (the hub's HTTPS client). The key goes in on stdin, never on
/// the command line.
async fn post(hub: &Hub, url: &str, key: Option<&str>, body: &Value) -> Result<(u16, Value), GatewayError> {
    if url.contains(['"', '\\', '\n', '\r']) || key.is_some_and(|k| k.contains(['"', '\\', '\n', '\r'])) {
        return Err(GatewayError::BadRequest("the model URL or key contains characters curl cannot take".into()));
    }
    let dir = hub.paths.root.join("tmp");
    tokio::fs::create_dir_all(&dir).await.map_err(HubError::from)?;
    let file = dir.join(format!("chat-{}.json", uuid::Uuid::now_v7().simple()));
    tokio::fs::write(&file, body.to_string()).await.map_err(HubError::from)?;
    let mut config = format!(
        "url = \"{url}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\n",
        file.display()
    );
    if let Some(k) = key {
        config.push_str(&format!("header = \"Authorization: Bearer {k}\"\n"));
    }
    let result = async {
        let mut child = tokio::process::Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", &TIMEOUT_SECONDS.to_string()])
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
    if let Err(e) = tokio::fs::remove_file(&file).await {
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
