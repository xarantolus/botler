//! Credentials dialogs: an agent asks for secrets (a new group, a field added to or removed from
//! a group, or values re-entered); the user fills a form; the hub declares the change in
//! `secrets.toml` itself (the filled form is the approval), stores the values and tells the agent
//! what now exists. Values never reach the agent. Granting a group stays a reviewed change.

use std::collections::BTreeMap;

use botler_core::Source;
use botler_core::defs::{Definitions, SecretClass};
use botler_core::payloads::{BuiltinPayload, DialogAnsweredPayload, SecretsOutcome};
use botler_store::{Dialog, DialogKind, DialogStatus};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::hub::{Hub, HubError, load_at};

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, ts_rs::TS)]
pub struct NewField {
    /// UPPER_SNAKE_CASE, unique across all groups; the env var name unless `file` is set.
    pub name: String,
    /// What the user should paste and where to get it.
    pub description: String,
    /// Deliver as a file at this absolute path instead (e.g. `/home/agent/.ssh/id_ed25519`).
    #[serde(default)]
    pub file: Option<String>,
}

/// What an agent asks for. All lists may be combined in one request.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema, PartialEq, ts_rs::TS)]
#[ts(optional_fields)]
pub struct SecretRequest {
    /// The group (existing, or new: then `description` and `add` are required).
    pub group: String,
    /// Why you need this, shown to the user.
    pub reason: String,
    /// For a new group: what it is for.
    #[serde(default)]
    pub description: Option<String>,
    /// For a new group: `container` (default) or `hub` (never enters a container).
    #[serde(default)]
    pub class: Option<SecretClass>,
    /// Fields to add (the user enters their values).
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<NewField>>")]
    pub add: Vec<NewField>,
    /// Existing fields whose values the user should (re-)enter.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub fill: Vec<String>,
    /// Fields to remove from the group (their values are deleted).
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub remove: Vec<String>,
}

/// A secret value as the web app sends it: text (typed, pasted, or a text file read in the
/// browser), or a file's exact bytes as base64.
#[derive(Clone, Debug, Deserialize, ts_rs::TS)]
#[serde(untagged)]
pub enum SecretInput {
    Text(String),
    Bytes { base64: String },
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum InputError {
    #[error("the value of {0} is not valid base64: {1}")]
    Base64(String, String),
    #[error("{0} is an environment variable, so its value must be text; binary values can only be delivered as files")]
    BinaryEnv(String),
}

impl From<&str> for SecretInput {
    fn from(s: &str) -> Self {
        Self::Text(s.into())
    }
}

impl SecretInput {
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(t) => t.is_empty(),
            Self::Bytes { base64 } => base64.is_empty(),
        }
    }

    /// The bytes to store for secret `name`, delivered as a file when `file` is true.
    pub fn decode(&self, name: &str, file: bool) -> Result<Vec<u8>, InputError> {
        use base64::Engine;
        let bytes = match self {
            Self::Text(t) => return Ok(t.as_bytes().to_vec()),
            Self::Bytes { base64 } => base64::engine::general_purpose::STANDARD
                .decode(base64.trim())
                .map_err(|e| InputError::Base64(name.into(), e.to_string()))?,
        };
        if !file && std::str::from_utf8(&bytes).is_err() {
            return Err(InputError::BinaryEnv(name.into()));
        }
        Ok(bytes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Input(#[from] InputError),
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error("secrets.toml could not be edited: {0}")]
    Toml(String),
    #[error("missing value for {0}")]
    MissingValue(String),
}

impl From<botler_store::Error> for RequestError {
    fn from(e: botler_store::Error) -> Self {
        Self::Hub(e.into())
    }
}

impl From<botler_repo::Error> for RequestError {
    fn from(e: botler_repo::Error) -> Self {
        Self::Hub(e.into())
    }
}

fn valid_secret_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl SecretRequest {
    /// Check the request against current definitions. Run at request time and again at submit.
    pub fn validate(&self, defs: &Definitions) -> Result<(), RequestError> {
        let bad = |m: String| Err(RequestError::Invalid(m));
        if !botler_core::defs::valid_name(&self.group) {
            return bad(format!("group name {:?} must be lowercase letters, digits and dashes", self.group));
        }
        if self.reason.trim().is_empty() {
            return bad("give a reason the user can judge".into());
        }
        if self.add.is_empty() && self.fill.is_empty() && self.remove.is_empty() {
            return bad("nothing requested: use add, fill or remove".into());
        }
        let existing = defs.secret_groups.get(&self.group);
        match existing {
            None => {
                if self.description.as_deref().is_none_or(|d| d.trim().is_empty()) || self.add.is_empty() {
                    return bad(format!(
                        "group {} does not exist: give a description and the fields to add",
                        self.group
                    ));
                }
                if !self.fill.is_empty() || !self.remove.is_empty() {
                    return bad(format!(
                        "group {} does not exist yet, so nothing can be filled or removed",
                        self.group
                    ));
                }
            }
            Some(g) => {
                if self.class.is_some_and(|c| c != g.class) {
                    return bad(format!(
                        "group {} already exists with class {:?}; classes cannot change here",
                        g.name, g.class
                    ));
                }
                for n in self.fill.iter().chain(&self.remove) {
                    if !g.secrets.contains(n) {
                        return bad(format!("{n} is not in group {}", g.name));
                    }
                }
                if self.remove.len() == g.secrets.len() && self.add.is_empty() {
                    return bad(format!("removing every field would leave group {} empty", g.name));
                }
            }
        }
        let class = existing.map_or(self.class.unwrap_or(SecretClass::Container), |g| g.class);
        let mut seen = Vec::new();
        for f in &self.add {
            if !valid_secret_name(&f.name) {
                return bad(format!("secret name {:?} must be UPPER_SNAKE_CASE", f.name));
            }
            if let Some(other) = defs.secrets.get(&f.name) {
                return bad(format!("{} already exists (group {}); use `fill` to re-enter it", f.name, other.group));
            }
            if seen.contains(&f.name) {
                return bad(format!("{} is listed twice", f.name));
            }
            seen.push(f.name.clone());
            match &f.file {
                Some(_) if class == SecretClass::Hub => {
                    return bad(format!(
                        "{} is in a hub group and never enters a container, so it cannot be a file",
                        f.name
                    ));
                }
                Some(p) if !p.starts_with('/') => return bad(format!("file path of {} must be absolute", f.name)),
                _ => {}
            }
        }
        if self.fill.iter().any(|n| self.remove.contains(n)) {
            return bad("a field cannot be filled and removed at once".into());
        }
        Ok(())
    }

    /// Fields the user enters values for, with their description and delivery.
    pub fn form(&self, defs: &Definitions) -> Vec<NewField> {
        let mut out = self.add.clone();
        for n in &self.fill {
            if let Some(d) = defs.secrets.get(n) {
                out.push(NewField { name: n.clone(), description: d.description.clone(), file: d.file.clone() });
            }
        }
        out
    }

    fn changes_declarations(&self) -> bool {
        !self.add.is_empty() || !self.remove.is_empty()
    }
}

/// Apply the request's declaration changes to `secrets.toml`, keeping comments and layout.
pub fn edit_secrets_toml(text: &str, req: &SecretRequest, is_new: bool) -> Result<String, RequestError> {
    use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| RequestError::Toml(e.to_string()))?;
    if !doc.contains_key("version") {
        doc["version"] = value(1);
    }
    if !doc.contains_key("group") {
        doc["group"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let groups = doc["group"]
        .as_array_of_tables_mut()
        .ok_or_else(|| RequestError::Toml("`group` is not an array of tables".into()))?;
    let field_table = |f: &NewField| {
        let mut t = Table::new();
        t["name"] = value(f.name.clone());
        t["description"] = value(f.description.clone());
        if let Some(p) = &f.file {
            t["file"] = value(p.clone());
        }
        t
    };
    if is_new {
        let mut g = Table::new();
        g["name"] = value(req.group.clone());
        g["description"] = value(req.description.clone().unwrap_or_default());
        g["class"] = value(match req.class.unwrap_or(SecretClass::Container) {
            SecretClass::Container => "container",
            SecretClass::Hub => "hub",
        });
        let mut fields = ArrayOfTables::new();
        for f in &req.add {
            fields.push(field_table(f));
        }
        g["secret"] = Item::ArrayOfTables(fields);
        groups.push(g);
        return Ok(doc.to_string());
    }
    let g = groups
        .iter_mut()
        .find(|t| t.get("name").and_then(|n| n.as_str()) == Some(req.group.as_str()))
        .ok_or_else(|| RequestError::Toml(format!("group {} not found in secrets.toml", req.group)))?;
    if !g.contains_key("secret") {
        g["secret"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let fields = g["secret"]
        .as_array_of_tables_mut()
        .ok_or_else(|| RequestError::Toml("`secret` is not an array of tables".into()))?;
    fields.retain(|t| !t.get("name").and_then(|n| n.as_str()).is_some_and(|n| req.remove.iter().any(|r| r == n)));
    for f in &req.add {
        fields.push(field_table(f));
    }
    Ok(doc.to_string())
}

/// Create the dialog for a request. Returns the dialog (its id is the request id).
pub async fn open(hub: &Hub, instance: &str, req: &SecretRequest) -> Result<Dialog, RequestError> {
    let applied = hub.applied().await;
    req.validate(&applied.defs)?;
    let spec = serde_json::to_value(req).map_err(|e| RequestError::Toml(e.to_string()))?;
    let what =
        match (applied.defs.secret_groups.contains_key(&req.group), req.add.len(), req.fill.len(), req.remove.len()) {
            (false, _, _, _) => format!("New secrets: {}", req.group),
            (true, a, f, 0) if a + f > 0 => format!("Secrets for {}", req.group),
            (true, 0, 0, _) => format!("Remove secrets from {}", req.group),
            _ => format!("Change secrets of {}", req.group),
        };
    let d = hub
        .store
        .create_dialog(&botler_store::NewDialog {
            instance: instance.into(),
            kind: DialogKind::Credentials,
            question: what,
            detail: Some(req.reason.clone()),
            ctas: vec![],
            about: vec![format!("secret-group:{}", req.group)],
            priority: botler_store::Priority::Blocking,
            proposal_id: None,
            spec: Some(spec),
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(d)
}

/// What the user submitted for a credentials dialog.
#[derive(Debug, Deserialize, ts_rs::TS)]
pub struct Submission {
    #[serde(default)]
    #[ts(optional, as = "Option<BTreeMap<String, SecretInput>>")]
    pub values: BTreeMap<String, SecretInput>,
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub decline: bool,
}

/// Carry out a filled credentials form.
pub async fn submit(hub: &Hub, dialog_id: &str, sub: Submission) -> Result<SecretsOutcome, RequestError> {
    let d = hub.store.dialog(dialog_id).await?;
    if d.kind != DialogKind::Credentials {
        return Err(RequestError::Invalid("not a credentials request".into()));
    }
    if d.status != DialogStatus::Open {
        return Err(botler_store::Error::DialogClosed { status: d.status, reason: d.abort_reason }.into());
    }
    let req: SecretRequest = serde_json::from_value(d.spec.clone().unwrap_or_default())
        .map_err(|e| RequestError::Invalid(format!("stored request is unreadable: {e}")))?;

    if sub.decline {
        hub.store.answer_dialog(dialog_id, "declined").await?;
        let outcome = SecretsOutcome { declined: true, added: vec![], filled: vec![], removed: vec![] };
        emit_answer(hub, &d, &req, outcome.clone()).await?;
        return Ok(outcome);
    }

    // The world may have changed since the agent asked.
    let applied = hub.applied().await;
    req.validate(&applied.defs)?;
    let form = req.form(&applied.defs);
    let mut values = BTreeMap::new();
    for f in &form {
        let Some(v) = sub.values.get(&f.name).filter(|v| !v.is_empty()) else {
            return Err(RequestError::MissingValue(f.name.clone()));
        };
        values.insert(f.name.clone(), v.decode(&f.name, f.file.is_some())?);
    }

    if req.changes_declarations() {
        declare(hub, &req, &d.instance).await?;
    }
    for f in &form {
        hub.store.set_secret_bytes(&hub.secret_key, &f.name, &values[&f.name]).await?;
    }
    for n in &req.remove {
        hub.store.delete_secret(n).await?;
    }
    hub.store.answer_dialog(dialog_id, "provided").await?;
    let outcome = SecretsOutcome {
        declined: false,
        added: req.add.iter().map(|f| f.name.clone()).collect(),
        filled: req.fill.clone(),
        removed: req.remove.clone(),
    };
    emit_answer(hub, &d, &req, outcome.clone()).await?;
    // Agents waiting on a missing secret can start now.
    hub.dispatch_notify.notify_one();
    Ok(outcome)
}

/// Commit the declaration change on main and apply it. Retries once if main moved.
async fn declare(hub: &Hub, req: &SecretRequest, instance: &str) -> Result<(), RequestError> {
    for attempt in 0..2 {
        let base = hub.repo.main().await?;
        let text = hub.repo.read_file(&base, "secrets.toml").await?.unwrap_or_default();
        let is_new = !hub.applied().await.defs.secret_groups.contains_key(&req.group);
        let edited = edit_secrets_toml(&text, req, is_new)?;
        let message = format!("secrets: {} (requested by {instance}, approved by the user)", req.group);
        let head = hub.repo.commit_files(&base, &[("secrets.toml", Some(edited.as_str()))], &message).await?;
        // Validate before anything is applied: the form must never break main.
        let applied = load_at(&hub.repo, &head).await?;
        match hub.repo.advance_main(&base, &head).await {
            Ok(()) => {
                hub.apply(applied).await?;
                tracing::info!(group = %req.group, commit = %head, "secret declarations updated");
                return Ok(());
            }
            Err(botler_repo::Error::MainMoved { .. }) if attempt == 0 => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(RequestError::Invalid("main kept moving; try again".into()))
}

async fn emit_answer(hub: &Hub, d: &Dialog, req: &SecretRequest, outcome: SecretsOutcome) -> Result<(), HubError> {
    let payload = BuiltinPayload::DialogAnswered(DialogAnsweredPayload {
        dialog_id: d.id.clone(),
        question: d.question.clone(),
        answer: if outcome.declined { "declined" } else { "provided" }.into(),
        group: Some(req.group.clone()),
        outcome: Some(outcome),
        note: Some("Values are stored by the hub and never shown to agents. To use the group in a container, `request_access(\"secret:<group>\")` for this chat, or grant it in an agent or job definition (`secrets = [\"<group>\"]`) and propose the change.".into()),
    });
    let ev = crate::hub::builtin(payload, Source::User)?.to(d.instance.clone());
    hub.emit(ev).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> SecretRequest {
        SecretRequest {
            group: "gitlab".into(),
            reason: "deploy".into(),
            description: None,
            class: None,
            add: vec![NewField { name: "GITLAB_TOKEN_STATION".into(), description: "token".into(), file: None }],
            fill: vec![],
            remove: vec!["GITLAB_OLD".into()],
        }
    }

    #[test]
    fn edit_keeps_comments_and_changes_only_the_group() {
        let text = "# keep me\nversion = 1\n\n[[group]]\nname = \"gitlab\"\nclass = \"container\"\n\n[[group.secret]]\nname = \"GITLAB_OLD\"\n\n[[group.secret]]\nname = \"GITLAB_TOKEN\"\n\n[[group]]\nname = \"other\"\nclass = \"hub\"\n\n[[group.secret]]\nname = \"OTHER\"\n";
        let out = edit_secrets_toml(text, &req(), false).unwrap();
        assert!(out.starts_with("# keep me"), "{out}");
        assert!(
            !out.contains("GITLAB_OLD") && out.contains("GITLAB_TOKEN\"") && out.contains("GITLAB_TOKEN_STATION"),
            "{out}"
        );
        assert!(out.contains("name = \"OTHER\""), "{out}");
        let parsed: botler_core::defs::SecretsFile = toml::from_str(&out).unwrap();
        assert_eq!(parsed.groups[0].secrets.len(), 2);
    }

    #[test]
    fn new_group_is_appended() {
        let mut r = req();
        r.group = "station".into();
        r.description = Some("Ground station".into());
        r.remove.clear();
        r.add[0].file = Some("/home/agent/.ssh/id_ed25519".into());
        let out = edit_secrets_toml("version = 1\n", &r, true).unwrap();
        let parsed: botler_core::defs::SecretsFile = toml::from_str(&out).unwrap();
        assert_eq!(parsed.groups[0].name, "station");
        assert_eq!(parsed.groups[0].secrets[0].file.as_deref(), Some("/home/agent/.ssh/id_ed25519"));
    }
}
