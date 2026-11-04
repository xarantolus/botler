//! OpenRouter's model catalog (`GET /api/v1/models`, public): what each model takes, its output
//! and context limits and its price. Fetched with curl, cached in `models-catalog.json` and
//! refreshed daily. Offline the hub works without it: models then get pi's own knowledge, and
//! model ids are only checked for their shape.

use std::sync::Arc;
use std::time::Duration;

use botler_core::defs::{Definitions, Diagnostic, ModelChoice, ModelInput, OPENROUTER, Severity};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::hub::Hub;
use crate::pi_config;

const URL: &str = "https://openrouter.ai/api/v1/models";
const MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("fetching {URL}: {0}")]
    Fetch(String),
    #[error("OpenRouter's model list is not what the hub expects: {0}")]
    Parse(#[from] serde_json::Error),
}

/// One model as the web app and the hub use it. Prices are USD per million tokens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct CatalogModel {
    pub id: String,
    pub name: String,
    pub context_length: Option<u64>,
    /// The most output tokens one call may ask for.
    pub max_output: Option<u64>,
    pub image: bool,
    pub price_in: Option<f64>,
    pub price_out: Option<f64>,
    pub price_cache_read: Option<f64>,
    pub price_cache_write: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
pub struct Catalog {
    pub fetched_at: DateTime<Utc>,
    pub models: Vec<CatalogModel>,
}

impl Catalog {
    pub fn get(&self, id: &str) -> Option<&CatalogModel> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Up to three ids closest to `id` (edit distance), for "did you mean".
    pub fn closest(&self, id: &str) -> Vec<&str> {
        let mut ranked: Vec<(usize, &str)> = self.models.iter().map(|m| (distance(id, &m.id), m.id.as_str())).collect();
        ranked.sort();
        ranked.into_iter().take(3).map(|(_, m)| m).collect()
    }

    /// Why `id` is not a usable OpenRouter model, if it is not in the catalog.
    pub fn unknown(&self, id: &str) -> Option<String> {
        self.get(id)
            .is_none()
            .then(|| format!("{id:?} is not in OpenRouter's model list; closest: {}", self.closest(id).join(", ")))
    }

    /// Warnings for unknown model ids in use or listed in `botler.toml`.
    pub fn check(&self, defs: &Definitions) -> Vec<Diagnostic> {
        let models = &defs.instance.models;
        let mut out = Vec::new();
        let mut check = |file: String, field: &str, name: &str| {
            let m = models.resolve(name);
            if m.provider == OPENROUTER
                && let Some(why) = self.unknown(&m.id)
            {
                out.push(Diagnostic { severity: Severity::Warning, path: file, message: format!("{field}: {why}") });
            }
        };
        if let Some(d) = &models.default {
            check("botler.toml".into(), "models.default", d);
        }
        for m in &models.models {
            check("botler.toml".into(), "models.model", &m.id);
        }
        for a in defs.agents.values() {
            if let Some(m) = &a.spec.model {
                check(format!("agents/{}/agent.toml", a.name), "model", m);
            }
        }
        out
    }
}

/// The `models.json` entry pi gets for `choice`: the configured cap and input, refined by the
/// catalog (the model's own output maximum, context window and prices).
pub fn pi_model(choice: &ModelChoice, known: Option<&CatalogModel>) -> pi_config::Model {
    let cap = u64::from(choice.max_tokens);
    let input = choice.input.clone().or_else(|| {
        known.map(|k| if k.image { vec![ModelInput::Text, ModelInput::Image] } else { vec![ModelInput::Text] })
    });
    let cost = known.map(|k| pi_config::Cost {
        input: k.price_in,
        output: k.price_out,
        cache_read: k.price_cache_read,
        cache_write: k.price_cache_write,
    });
    pi_config::Model {
        max_tokens: known.and_then(|k| k.max_output).map_or(cap, |m| m.min(cap)),
        input,
        context_window: known.and_then(|k| k.context_length),
        cost: cost.filter(|c| *c != pi_config::Cost::default()),
    }
}

/// The `settings.json` pi gets: it compacts once the context passes its window minus
/// `reserveTokens`, so that is set to compact at `compact_at` (0, or an unknown window: pi's
/// default). The window itself stays the model's: pi fits every request, the summary of an
/// already larger context included, into it, so a cut window starves the summary and the chat
/// never compacts. The user's messages sent mid-run (steers) all reach the model at the next
/// pause together, not one per model call.
pub fn pi_settings(known: Option<&CatalogModel>, compact_at: u64) -> pi_config::Settings {
    let compaction = known
        .and_then(|k| k.context_length)
        .filter(|&c| compact_at > 0 && c > compact_at)
        .map(|c| pi_config::Compaction { reserve_tokens: c - compact_at });
    pi_config::Settings { steering_mode: pi_config::SteeringMode::All, compaction }
}

/// The catalog entry for `choice`, if it is an OpenRouter model the catalog knows.
pub async fn known(hub: &Hub, choice: &ModelChoice) -> Option<CatalogModel> {
    if choice.provider != OPENROUTER {
        return None;
    }
    hub.catalog.read().await.as_ref().and_then(|c| c.get(&choice.id).cloned())
}

/// The catalog for the web app's model picker.
#[derive(Debug, Serialize, TS)]
pub struct CatalogView {
    pub fetched_at: Option<DateTime<Utc>>,
    pub models: Vec<CatalogModel>,
    /// Why there is no catalog (offline), when there is none.
    pub error: Option<String>,
}

/// The cached catalog, fetched first if the hub never had one.
pub async fn view(hub: &Hub) -> CatalogView {
    let cached = hub.catalog.read().await.clone();
    let (catalog, error) = match cached {
        Some(c) => (Some(c), None),
        None => match refresh(hub).await {
            Ok(c) => (Some(c), None),
            Err(e) => (None, Some(e.to_string())),
        },
    };
    CatalogView {
        fetched_at: catalog.as_ref().map(|c| c.fetched_at),
        models: catalog.map(|c| c.models.clone()).unwrap_or_default(),
        error,
    }
}

/// [`Catalog::check`] against the current catalog; nothing while there is none (offline).
pub async fn warnings(hub: &Hub, defs: &Definitions) -> Vec<Diagnostic> {
    hub.catalog.read().await.as_ref().map(|c| c.check(defs)).unwrap_or_default()
}

/// The cached catalog from disk. A cache that does not parse is ignored (and replaced by the
/// next fetch).
pub fn load_cached(hub_root: &std::path::Path) -> Option<Arc<Catalog>> {
    let path = hub_root.join("models-catalog.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read the model catalog cache");
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(c) => Some(Arc::new(c)),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring an unreadable model catalog cache");
            None
        }
    }
}

/// Fetch the catalog now, cache it and use it.
pub async fn refresh(hub: &Hub) -> Result<Arc<Catalog>, CatalogError> {
    let out = tokio::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location", "--max-time", "30", URL])
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        return Err(CatalogError::Fetch(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let catalog = Arc::new(parse(&out.stdout, Utc::now())?);
    let path = hub.paths.root.join("models-catalog.json");
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, serde_json::to_vec(&*catalog)?).await?;
    tokio::fs::rename(&tmp, &path).await?;
    *hub.catalog.write().await = Some(catalog.clone());
    tracing::info!(models = catalog.models.len(), "model catalog refreshed");
    Ok(catalog)
}

/// Keep the catalog at most a day old.
pub async fn run(hub: Arc<Hub>) {
    loop {
        let fresh = hub
            .catalog
            .read()
            .await
            .as_ref()
            .is_some_and(|c| (Utc::now() - c.fetched_at).to_std().is_ok_and(|age| age < MAX_AGE));
        if !fresh && let Err(e) = refresh(&hub).await {
            tracing::warn!(error = %e, "could not refresh OpenRouter's model catalog; retrying in an hour");
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

#[derive(Deserialize)]
struct Raw {
    data: Vec<RawModel>,
}

#[derive(Deserialize)]
struct RawModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    architecture: RawArchitecture,
    #[serde(default)]
    pricing: RawPricing,
    #[serde(default)]
    top_provider: RawTopProvider,
}

#[derive(Default, Deserialize)]
struct RawArchitecture {
    #[serde(default)]
    input_modalities: Vec<String>,
}

#[derive(Default, Deserialize)]
struct RawPricing {
    prompt: Option<String>,
    completion: Option<String>,
    input_cache_read: Option<String>,
    input_cache_write: Option<String>,
}

#[derive(Default, Deserialize)]
struct RawTopProvider {
    max_completion_tokens: Option<u64>,
}

fn parse(body: &[u8], fetched_at: DateTime<Utc>) -> Result<Catalog, CatalogError> {
    let raw: Raw = serde_json::from_slice(body)?;
    // Prices are USD per token as decimal strings; negative ones mean "variable" (routers).
    let per_million = |p: &Option<String>| -> Result<Option<f64>, CatalogError> {
        let Some(p) = p else { return Ok(None) };
        let v: f64 = p
            .parse()
            .map_err(|_| CatalogError::Parse(serde::de::Error::custom(format!("price {p:?} is not a number"))))?;
        // Rounded to a millionth of a dollar, so 0.0000001 reads 0.1 rather than 0.09999…
        Ok((v >= 0.0).then_some((v * 1e12).round() / 1e6))
    };
    let mut models = Vec::with_capacity(raw.data.len());
    for m in raw.data {
        models.push(CatalogModel {
            image: m.architecture.input_modalities.iter().any(|i| i == "image"),
            price_in: per_million(&m.pricing.prompt)?,
            price_out: per_million(&m.pricing.completion)?,
            price_cache_read: per_million(&m.pricing.input_cache_read)?,
            price_cache_write: per_million(&m.pricing.input_cache_write)?,
            name: if m.name.is_empty() { m.id.clone() } else { m.name },
            id: m.id,
            context_length: m.context_length,
            max_output: m.top_provider.max_completion_tokens,
        });
    }
    Ok(Catalog { fetched_at, models })
}

/// Levenshtein distance, by characters.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"data":[
        {"id":"~anthropic/claude-sonnet-latest","name":"Sonnet","context_length":1000000,
         "architecture":{"input_modalities":["text","image","file"]},
         "pricing":{"prompt":"0.000002","completion":"0.00001","input_cache_read":"0.0000001"},
         "top_provider":{"max_completion_tokens":128000}},
        {"id":"openrouter/auto","name":"Auto","pricing":{"prompt":"-1","completion":"-1"}},
        {"id":"qwen/qwen3.8-flash","architecture":{"input_modalities":["text"]},"top_provider":{"max_completion_tokens":8192}}
    ]}"#;

    #[test]
    fn parses_prices_per_million_and_suggests_close_ids() {
        let c = parse(BODY.as_bytes(), Utc::now()).unwrap();
        let s = c.get("~anthropic/claude-sonnet-latest").unwrap();
        assert!(s.image && s.price_in == Some(2.0) && s.price_out == Some(10.0) && s.price_cache_write.is_none());
        assert_eq!(c.get("openrouter/auto").unwrap().price_in, None);
        assert_eq!(c.get("qwen/qwen3.8-flash").unwrap().name, "qwen/qwen3.8-flash");
        assert_eq!(c.closest("anthropic/claude-sonnet-latest")[0], "~anthropic/claude-sonnet-latest");
        assert!(c.unknown("qwen/qwen3.8-flash").is_none());
        assert!(c.unknown("qwen/qwen-flash").unwrap().contains("closest: qwen/qwen3.8-flash"));
    }

    #[test]
    fn pi_model_caps_output_and_takes_the_catalogs_input() {
        let c = parse(BODY.as_bytes(), Utc::now()).unwrap();
        let mut choice = botler_core::defs::Models::default().resolve("qwen/qwen3.8-flash");
        let m = serde_json::to_value(pi_model(&choice, c.get("qwen/qwen3.8-flash"))).unwrap();
        assert_eq!(m, serde_json::json!({ "maxTokens": 8192, "input": ["text"] }));
        choice.id = "~anthropic/claude-sonnet-latest".into();
        let m = serde_json::to_value(pi_model(&choice, c.get(&choice.id))).unwrap();
        assert_eq!(m["maxTokens"], 16384);
        assert_eq!(m["input"], serde_json::json!(["text", "image"]));
        assert_eq!(m["contextWindow"], 1000000);
        assert_eq!(m["cost"], serde_json::json!({ "input": 2.0, "output": 10.0, "cacheRead": 0.1 }));
        // Offline: only the configured cap.
        assert_eq!(serde_json::to_value(pi_model(&choice, None)).unwrap(), serde_json::json!({ "maxTokens": 16384 }));
        // Compacting at 120k of a 1M window: pi reserves the rest.
        let s = serde_json::to_value(pi_settings(c.get(&choice.id), 120_000)).unwrap();
        assert_eq!(s, serde_json::json!({ "steeringMode": "all", "compaction": { "reserveTokens": 880_000 } }));
        assert_eq!(pi_settings(c.get(&choice.id), 0).compaction, None);
        assert_eq!(pi_settings(None, 120_000).compaction, None);
    }
}
