//! The web app's Settings page: models, agents and deployment defaults from `botler.toml` and
//! `agents/*/agent.toml`. The user is trusted, so a save is a commit on main authored by them,
//! with no proposal: the files are edited with toml_edit (comments and layout stay), the result
//! must load before main moves, and only what the user changed is written, onto the current main
//! (a field they did not touch keeps whatever main has meanwhile).

use std::collections::{BTreeMap, BTreeSet};

use botler_core::defs::{Defaults, Definitions, Diagnostic, ModelEntry, Push, is_model_id};
use botler_core::duration::HumanDuration;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table};
use ts_rs::TS;

use crate::hub::{Hub, HubError, Result, load_at};

const AUTHOR: &str = "the user <user@botler.invalid>";

/// Everything the Settings page shows.
#[derive(Debug, Serialize, TS)]
pub struct Settings {
    /// The main commit these were read at; edits are sent back against it.
    pub commit: String,
    pub models: ModelsEdit,
    /// Deprecated `[models.routes]` names still in `botler.toml` (they still resolve).
    pub routes: Vec<String>,
    pub agents: Vec<AgentSettings>,
    /// `[defaults]` and its JSON Schema, which the form is generated from.
    pub defaults: Value,
    pub defaults_schema: Value,
    /// `[push]` (null when off) and its JSON Schema.
    pub push: Value,
    pub push_schema: Value,
    /// Tools agents can be granted (`tools` takes globs of these names).
    pub tools: Vec<String>,
    /// Services with a port: what `mcp` can name.
    pub services: Vec<String>,
    /// Problems with the current settings, e.g. model ids OpenRouter does not list.
    pub warnings: Vec<FieldError>,
    /// Jobs and services with a `Settings` model.
    pub runnables: Vec<RunnableSettings>,
}

/// A job's or service's settings form: its model's schema and the values in its settings.toml.
#[derive(Debug, Serialize, TS)]
pub struct RunnableSettings {
    pub job: String,
    pub description: String,
    /// `oneshot`, `service` or `action`.
    pub lifetime: String,
    pub enabled: bool,
    pub schema: Value,
    /// Only what settings.toml sets; the schema's defaults apply to the rest.
    pub values: Value,
}

/// `[models]`: the default and the models we typically use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ModelsEdit {
    pub default: Option<String>,
    pub models: Vec<ModelEntry>,
}

/// The editable part of an agent definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct AgentEdit {
    /// Model id; null: `models.default`.
    pub model: Option<String>,
    pub tools: Vec<String>,
    pub mcp: Vec<String>,
    /// e.g. "30m"; null: `defaults.idle_timeout`.
    pub idle_timeout: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct AgentSettings {
    pub name: String,
    pub description: String,
    pub edit: AgentEdit,
    /// The model id it runs on (its own or the default).
    pub effective_model: Option<String>,
    /// MCP servers it starts itself (`mcp_stdio`, edited in its agent.toml).
    pub mcp_stdio: Vec<String>,
}

/// A save: only the sections and agents the user changed.
#[derive(Debug, Deserialize, TS)]
pub struct SettingsEdit {
    /// The commit the page was loaded at (`Settings::commit`).
    pub commit: String,
    #[serde(default)]
    #[ts(optional)]
    pub models: Option<ModelsEdit>,
    #[serde(default)]
    #[ts(optional)]
    pub agents: Option<BTreeMap<String, AgentEdit>>,
    #[serde(default)]
    #[ts(optional)]
    pub deployment: Option<DeploymentEdit>,
    /// Per job: all of its settings values (those equal to the default are not written).
    #[serde(default)]
    #[ts(optional)]
    pub runnables: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Deserialize, TS)]
pub struct DeploymentEdit {
    pub defaults: Value,
    /// null turns push off.
    pub push: Value,
}

/// A problem with one field (`models.default`, `models.model.2`, `agents.chat.model`,
/// `defaults.idle_timeout`, `push`), or with a whole file when the hub cannot tell which field.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Serialize, TS)]
pub struct SettingsSaved {
    pub saved: bool,
    /// The new main when saved (unchanged when nothing changed or it was refused).
    pub commit: String,
    pub errors: Vec<FieldError>,
    pub warnings: Vec<FieldError>,
}

fn agent_edit(a: &botler_core::defs::Agent) -> AgentEdit {
    AgentEdit {
        model: a.model.clone(),
        tools: a.env.tools.clone(),
        mcp: a.env.mcp.clone(),
        idle_timeout: a.idle_timeout.map(|d| d.to_string()),
    }
}

fn models_edit(defs: &Definitions) -> ModelsEdit {
    let m = &defs.instance.models;
    ModelsEdit { default: m.default.clone(), models: m.models.clone() }
}

fn to_json<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| HubError::Migration(format!("serializing settings: {e}")))
}

/// Diagnostics as field errors: a file's problems belong to its section.
fn by_file(diags: &[Diagnostic]) -> Vec<FieldError> {
    diags
        .iter()
        .map(|d| {
            let agent = d.path.strip_prefix("agents/").and_then(|p| p.strip_suffix("/agent.toml"));
            let job = d.path.strip_prefix("jobs/").and_then(|p| p.strip_suffix("/settings.toml"));
            let field = match (agent, job) {
                (Some(agent), _) => format!("agents.{agent}"),
                (_, Some(job)) => format!("runnables.{job}"),
                _ => d.path.clone(),
            };
            FieldError { field, message: d.message.clone() }
        })
        .collect()
}

pub async fn get(hub: &Hub) -> Result<Settings> {
    let applied = hub.applied().await;
    let defs = &applied.defs;
    let schema = |s: schemars::Schema| to_json(&s);
    Ok(Settings {
        commit: applied.commit.clone(),
        models: models_edit(defs),
        routes: defs.instance.models.routes.keys().cloned().collect(),
        agents: defs
            .agents
            .values()
            .map(|a| AgentSettings {
                name: a.name.clone(),
                description: a.spec.description.clone(),
                edit: agent_edit(&a.spec),
                effective_model: defs.model_for(&a.spec).map(|m| m.id),
                mcp_stdio: a.spec.env.mcp_stdio.keys().cloned().collect(),
            })
            .collect(),
        defaults: to_json(&defs.instance.defaults)?,
        defaults_schema: schema(schemars::schema_for!(Defaults))?,
        push: to_json(&defs.instance.push)?,
        push_schema: schema(schemars::schema_for!(Push))?,
        tools: defs.tools.keys().cloned().collect(),
        services: defs.services().filter(|j| j.spec.env.port.is_some()).map(|j| j.name.clone()).collect(),
        warnings: by_file(&crate::model_catalog::warnings(hub, defs).await),
        runnables: defs
            .jobs
            .values()
            .filter_map(|j| {
                let s = j.settings.as_ref()?;
                Some(RunnableSettings {
                    job: j.name.clone(),
                    description: if s.description.is_empty() {
                        j.spec.description.clone()
                    } else {
                        s.description.clone()
                    },
                    lifetime: format!("{:?}", j.spec.lifetime).to_lowercase(),
                    enabled: j.spec.enabled,
                    schema: s.schema.clone(),
                    values: s.values.clone(),
                })
            })
            .collect(),
    })
}

/// `values` as settings.toml should hold them: without the fields equal to their default.
fn non_default(schema: &Value, values: &Value) -> Value {
    let props = schema.get("properties").and_then(Value::as_object);
    let kept = values
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(k, v)| !v.is_null() && props.and_then(|p| p.get(*k)).and_then(|f| f.get("default")) != Some(*v));
    Value::Object(kept.map(|(k, v)| (k.clone(), v.clone())).collect())
}

/// Apply the user's edit as a commit on main. Field problems come back in `errors`, unsaved.
pub async fn save(hub: &Hub, edit: &SettingsEdit) -> Result<SettingsSaved> {
    let base = load_at(&hub.repo, &edit.commit).await?;
    let catalog = hub.catalog.read().await.clone();
    let mut errors = Vec::new();
    // A model id must look like one; one the user just typed must also be in OpenRouter's list.
    let check_model = |field: String, id: &str, was: Option<&str>| {
        let routes = &base.defs.instance.models.routes;
        let message = if routes.contains_key(id) {
            None
        } else if !is_model_id(id) {
            Some(format!("{id:?} is not an OpenRouter model id (vendor/model)"))
        } else if was != Some(id) {
            catalog.as_ref().and_then(|c| c.unknown(id))
        } else {
            None
        };
        message.map(|message| FieldError { field, message })
    };
    if let Some(m) = &edit.models {
        let old = models_edit(&base.defs);
        if let Some(d) = &m.default {
            errors.extend(check_model("models.default".into(), d, old.default.as_deref()));
        }
        let mut seen = BTreeSet::new();
        for (i, e) in m.models.iter().enumerate() {
            let was = old.models.iter().any(|o| o.id == e.id).then_some(e.id.as_str());
            errors.extend(check_model(format!("models.model.{i}"), &e.id, was));
            if !seen.insert(&e.id) {
                errors.push(FieldError { field: format!("models.model.{i}"), message: "listed twice".into() });
            }
        }
    }
    for (name, a) in edit.agents.iter().flatten() {
        let Some(def) = base.defs.agents.get(name) else {
            errors.push(FieldError { field: format!("agents.{name}"), message: "no such agent".into() });
            continue;
        };
        if let Some(m) = &a.model {
            errors.extend(check_model(format!("agents.{name}.model"), m, def.spec.model.as_deref()));
        }
        if let Some(t) = &a.idle_timeout
            && let Err(e) = t.parse::<HumanDuration>()
        {
            errors.push(FieldError { field: format!("agents.{name}.idle_timeout"), message: e.to_string() });
        }
    }
    let deployment = match &edit.deployment {
        Some(d) => {
            let defaults = parse_section::<Defaults>("defaults", &d.defaults, &mut errors);
            let push =
                if d.push.is_null() { Some(Value::Null) } else { parse_section::<Push>("push", &d.push, &mut errors) };
            defaults.zip(push)
        }
        None => None,
    };
    let mut runnables = Vec::new();
    for (job, values) in edit.runnables.iter().flatten() {
        let Some(s) = base.defs.jobs.get(job).and_then(|j| j.settings.as_ref()) else {
            errors.push(FieldError { field: format!("runnables.{job}"), message: "no such job with settings".into() });
            continue;
        };
        let values = non_default(&s.schema, values);
        match botler_core::event_check::compile(&s.schema) {
            Ok(v) => errors.extend(v.iter_errors(&values).take(10).map(|e| {
                let at = e.instance_path().to_string().trim_start_matches('/').replace('/', ".");
                let field = if at.is_empty() { format!("runnables.{job}") } else { format!("runnables.{job}.{at}") };
                FieldError { field, message: e.to_string() }
            })),
            Err(e) => errors.push(FieldError { field: format!("runnables.{job}"), message: e }),
        }
        runnables.push((job.clone(), s.values.clone(), values));
    }
    if !errors.is_empty() {
        return Ok(SettingsSaved { saved: false, commit: hub.repo.main().await?, errors, warnings: vec![] });
    }

    for attempt in 0..2 {
        let main = hub.repo.main().await?;
        let mut files: Vec<(String, String)> = Vec::new();
        let mut changed = Vec::new();
        if edit.models.is_some() || deployment.is_some() {
            let text = hub.repo.read_file(&main, "botler.toml").await?.unwrap_or_default();
            let mut doc = parse_doc("botler.toml", &text)?;
            if let Some(m) = &edit.models {
                edit_models(&mut doc, &models_edit(&base.defs), m, &mut changed)?;
            }
            if let Some((defaults, push)) = &deployment {
                let inst = &base.defs.instance;
                apply_json(
                    table(&mut doc, "defaults")?,
                    &to_json(&inst.defaults)?,
                    defaults,
                    "defaults.",
                    &mut changed,
                );
                let old_push = to_json(&inst.push)?;
                if push.is_null() {
                    if doc.remove("push").is_some() {
                        changed.push("push".into());
                    }
                } else {
                    apply_json(table(&mut doc, "push")?, &old_push, push, "push.", &mut changed);
                }
            }
            let new = doc.to_string();
            if new != text {
                files.push(("botler.toml".into(), new));
            }
        }
        for (name, a) in edit.agents.iter().flatten() {
            let old = to_json(&agent_edit(&base.defs.agents[name].spec))?;
            let path = format!("agents/{name}/agent.toml");
            let text = hub.repo.read_file(&main, &path).await?.unwrap_or_default();
            let mut doc = parse_doc(&path, &text)?;
            apply_json(doc.as_table_mut(), &old, &to_json(a)?, &format!("agents/{name}: "), &mut changed);
            let new = doc.to_string();
            if new != text {
                files.push((path, new));
            }
        }
        for (job, old, new) in &runnables {
            let path = botler_core::defs::settings_path(job);
            let text = hub.repo.read_file(&main, &path).await?.unwrap_or_default();
            let mut doc = parse_doc(&path, &text)?;
            apply_json(doc.as_table_mut(), old, new, &format!("{job}: "), &mut changed);
            // An emptied list is a value here (the default may not be empty), not a removal.
            for (k, v) in new.as_object().into_iter().flatten() {
                if v.as_array().is_some_and(Vec::is_empty) {
                    set(doc.as_table_mut(), k, v);
                }
            }
            let new = doc.to_string();
            if new != text {
                files.push((path, new));
            }
        }
        if files.is_empty() {
            return Ok(SettingsSaved { saved: true, commit: main, errors: vec![], warnings: vec![] });
        }
        let summary = match changed.len() {
            0..=4 => changed.join(", "),
            n => format!("{} and {} more", changed[..3].join(", "), n - 3),
        };
        let refs: Vec<(&str, Option<&str>)> = files.iter().map(|(p, t)| (p.as_str(), Some(t.as_str()))).collect();
        let message = format!("settings: {summary} (by the user)");
        let head = hub.repo.commit_files_by(&main, &refs, &message, AUTHOR).await?;
        // Validate before anything moves: the page must never break main.
        let applied = match load_at(&hub.repo, &head).await {
            Ok(a) => a,
            Err(HubError::InvalidDefinitions { diagnostics, .. }) => {
                let errors = by_file(
                    &diagnostics
                        .into_iter()
                        .filter(|d| d.severity == botler_core::defs::Severity::Error)
                        .collect::<Vec<_>>(),
                );
                return Ok(SettingsSaved { saved: false, commit: main, errors, warnings: vec![] });
            }
            Err(e) => return Err(e),
        };
        match hub.repo.advance_main(&main, &head).await {
            Ok(()) => {
                let warnings = by_file(&crate::model_catalog::warnings(hub, &applied.defs).await);
                hub.apply(applied).await?;
                tracing::info!(commit = %head, "{message}");
                crate::transactions::after_main_moved(hub).await;
                return Ok(SettingsSaved { saved: true, commit: head, errors: vec![], warnings });
            }
            Err(botler_repo::Error::MainMoved { .. }) if attempt == 0 => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(HubError::Forbidden("main kept moving; save again".into()))
}

/// `value` as a `T`, normalized (e.g. `"900s"` becomes `"15m"`); a problem is reported on the
/// first key that alone fails to parse, else on the whole section.
fn parse_section<T: Serialize + serde::de::DeserializeOwned>(
    section: &str,
    value: &Value,
    errors: &mut Vec<FieldError>,
) -> Option<Value> {
    match serde_json::from_value::<T>(value.clone()) {
        Ok(v) => serde_json::to_value(v).ok(),
        Err(whole) => {
            let field = value.as_object().into_iter().flatten().find_map(|(k, v)| {
                let alone = serde_json::json!({ k.clone(): v.clone() });
                serde_json::from_value::<T>(alone)
                    .err()
                    .filter(|e| !e.to_string().contains("missing field"))
                    .map(|e| (k, e))
            });
            errors.push(match field {
                Some((k, e)) => FieldError { field: format!("{section}.{k}"), message: e.to_string() },
                None => FieldError { field: section.into(), message: whole.to_string() },
            });
            None
        }
    }
}

fn parse_doc(path: &str, text: &str) -> Result<DocumentMut> {
    text.parse().map_err(|e: toml_edit::TomlError| HubError::Forbidden(format!("{path} does not parse: {e}")))
}

/// The table at `key`, created when missing.
fn table<'a>(t: &'a mut Table, key: &str) -> Result<&'a mut Table> {
    if !t.contains_key(key) {
        t.insert(key, Item::Table(Table::new()));
    }
    t[key].as_table_mut().ok_or_else(|| HubError::Forbidden(format!("`{key}` in botler.toml is not a table")))
}

/// `[models]`: the default, and `[[models.model]]` matched by id (removed, changed in place or
/// appended), so entries the user did not touch keep their comments.
fn edit_models(doc: &mut DocumentMut, old: &ModelsEdit, new: &ModelsEdit, changed: &mut Vec<String>) -> Result<()> {
    if old == new {
        return Ok(());
    }
    let models = table(doc, "models")?;
    if old.default != new.default {
        match &new.default {
            Some(d) => set(models, "default", &Value::String(d.clone())),
            None => drop(models.remove("default")),
        }
        changed.push("models.default".into());
    }
    if old.models == new.models {
        return Ok(());
    }
    if !models.contains_key("model") {
        models.insert("model", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let list = models["model"]
        .as_array_of_tables_mut()
        .ok_or_else(|| HubError::Forbidden("`models.model` in botler.toml is not a list of tables".into()))?;
    let id_of = |t: &Table| t.get("id").and_then(|i| i.as_str()).map(str::to_string);
    list.retain(|t| {
        id_of(t).is_none_or(|id| new.models.iter().any(|m| m.id == id) || !old.models.iter().any(|m| m.id == id))
    });
    for m in &new.models {
        let was = old.models.iter().find(|o| o.id == m.id);
        if was == Some(m) {
            continue;
        }
        let old_json = was.map(to_json).transpose()?.unwrap_or(Value::Null);
        let mut keys = Vec::new();
        let at = list.iter().position(|t| id_of(t).as_deref() == Some(m.id.as_str()));
        match at {
            Some(i) => {
                if let Some(t) = list.get_mut(i) {
                    apply_json(t, &old_json, &to_json(m)?, "", &mut keys);
                }
            }
            None => {
                let mut t = Table::new();
                apply_json(&mut t, &Value::Null, &to_json(m)?, "", &mut keys);
                list.push(t);
            }
        }
        changed.push(format!("models.model {}", m.id));
    }
    for o in &old.models {
        if !new.models.iter().any(|m| m.id == o.id) {
            changed.push(format!("models.model {} removed", o.id));
        }
    }
    Ok(())
}

/// Write the keys where `new` differs from `old` into `t` (null or an empty list removes a key;
/// objects are tables). Names of changed keys go to `changed`, prefixed.
fn apply_json(t: &mut Table, old: &Value, new: &Value, prefix: &str, changed: &mut Vec<String>) {
    let empty = serde_json::Map::new();
    let (o, n) = (old.as_object().unwrap_or(&empty), new.as_object().unwrap_or(&empty));
    let keys: BTreeSet<&String> = o.keys().chain(n.keys()).collect();
    for k in keys {
        let (ov, nv) = (o.get(k).unwrap_or(&Value::Null), n.get(k).unwrap_or(&Value::Null));
        if ov == nv {
            continue;
        }
        match nv {
            Value::Object(_) => {
                if !t.get(k).is_some_and(Item::is_table) {
                    t.insert(k, Item::Table(Table::new()));
                }
                if let Some(sub) = t[k.as_str()].as_table_mut() {
                    apply_json(sub, ov, nv, &format!("{prefix}{k}."), changed);
                }
                continue;
            }
            Value::Null => drop(t.remove(k)),
            Value::Array(a) if a.is_empty() => drop(t.remove(k)),
            v => set(t, k, v),
        }
        changed.push(format!("{prefix}{k}"));
    }
}

/// Set `key` to `v`, keeping a trailing comment on the old value.
fn set(t: &mut Table, key: &str, v: &Value) {
    let mut value = to_toml(v);
    if let Some(old) = t.get(key).and_then(Item::as_value) {
        *value.decor_mut() = old.decor().clone();
    }
    t.insert(key, Item::Value(value));
}

fn to_toml(v: &Value) -> toml_edit::Value {
    match v {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or_default().into(),
        },
        Value::Array(a) => a.iter().map(to_toml).collect::<toml_edit::Array>().into(),
        Value::Object(o) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in o {
                t.insert(k, to_toml(v));
            }
            t.into()
        }
        Value::String(s) => s.as_str().into(),
        Value::Null => "".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changed_keys_are_written_and_comments_stay() {
        let mut doc: DocumentMut =
            "# top\n[defaults]\nidle_timeout = \"15m\" # short\n\n[defaults.budget]\nusd_per_day = 1.0\n"
                .parse()
                .unwrap();
        let old = serde_json::json!({ "idle_timeout": "15m", "job_timeout": "10m", "budget": { "usd_per_day": 1.0 } });
        let new = serde_json::json!({ "idle_timeout": "20m", "job_timeout": "10m", "budget": { "usd_per_day": null, "tokens_per_day": 5000 } });
        let mut changed = vec![];
        apply_json(table(&mut doc, "defaults").unwrap(), &old, &new, "defaults.", &mut changed);
        assert_eq!(changed, ["defaults.budget.tokens_per_day", "defaults.budget.usd_per_day", "defaults.idle_timeout"]);
        assert_eq!(
            doc.to_string(),
            "# top\n[defaults]\nidle_timeout = \"20m\" # short\n\n[defaults.budget]\ntokens_per_day = 5000\n"
        );
    }

    #[test]
    fn model_entries_are_matched_by_id() {
        let text = "[models]\ndefault = \"a/x\"\n\n# keep\n[[models.model]]\nid = \"a/x\"\nnote = \"cheap\" # yes\n\n[[models.model]]\nid = \"b/y\"\n";
        let mut doc: DocumentMut = text.parse().unwrap();
        let entry = |id: &str, note: &str| ModelEntry {
            id: id.into(),
            note: note.into(),
            max_tokens: None,
            input: None,
            thinking: None,
        };
        let old = ModelsEdit { default: Some("a/x".into()), models: vec![entry("a/x", "cheap"), entry("b/y", "")] };
        let new = ModelsEdit { default: Some("c/z".into()), models: vec![entry("a/x", "cheap"), entry("c/z", "new")] };
        let mut changed = vec![];
        edit_models(&mut doc, &old, &new, &mut changed).unwrap();
        assert_eq!(changed, ["models.default", "models.model c/z", "models.model b/y removed"]);
        let out = doc.to_string();
        assert!(out.contains("# keep\n[[models.model]]\nid = \"a/x\"\nnote = \"cheap\" # yes\n"), "{out}");
        assert!(
            out.contains("default = \"c/z\"") && !out.contains("b/y") && out.contains("id = \"c/z\"\nnote = \"new\""),
            "{out}"
        );
    }

    #[test]
    fn a_bad_field_is_named() {
        let mut errors = vec![];
        let v = serde_json::json!({ "idle_timeout": "soon", "job_timeout": "10m" });
        assert!(parse_section::<Defaults>("defaults", &v, &mut errors).is_none());
        assert_eq!(errors[0].field, "defaults.idle_timeout");
        let ok = parse_section::<Defaults>("defaults", &serde_json::json!({ "idle_timeout": "900s" }), &mut errors);
        assert_eq!(ok.unwrap()["idle_timeout"], "15m");
    }
}
