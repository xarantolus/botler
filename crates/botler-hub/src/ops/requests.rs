//! Requests to the user: questions and secret requests, answered later as `dialog.answered`.

use std::sync::Arc;

use botler_core::defs::{Resource, ResourceKind};
use botler_store::{DialogKind, DialogStatus, NewDialog, Priority};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who, parse_when, to_json};
use crate::hub::{Hub, HubError};
use crate::secret_requests::{self, RequestError, SecretRequest};

impl From<RequestError> for OpError {
    fn from(e: RequestError) -> Self {
        match e {
            RequestError::Invalid(m) | RequestError::MissingValue(m) => Self::BadRequest(m),
            RequestError::Input(e) => Self::BadRequest(e.to_string()),
            RequestError::Hub(h) => Self::Hub(h),
            RequestError::Toml(m) => Self::Hub(HubError::Forbidden(format!("secrets.toml: {m}"))),
        }
    }
}

/// A request is open; its answer arrives later as an event.
#[derive(Debug, Serialize, TS)]
pub struct RequestOpened {
    pub request_id: String,
    pub status: String,
    pub note: String,
}

impl Op for SecretRequest {
    const NAME: &'static str = "request_secrets";
    const DESCRIPTION: &'static str = "Ask the user for secrets, asynchronously: create a group, add fields to one, re-enter values (`fill`) or remove fields. The user gets a form; you get a request id now and a `dialog.answered` event later saying what was provided (never the values). Do not continue work that needs the secrets until that event arrives. Using a group in a container still needs a grant: `request_access(\"secret:<group>\")` for this chat, or `secrets = [\"<group>\"]` in a definition, proposed as a change.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = RequestOpened;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<RequestOpened, OpError> {
        let instance = caller.agent("only agents request secrets")?;
        let d = secret_requests::open(hub, instance, &self).await?;
        Ok(RequestOpened {
            request_id: d.id,
            status: "waiting for the user".into(),
            note: "Continue with other work or stop; the answer arrives as a dialog.answered event.".into(),
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListRequests {}

#[derive(Debug, Serialize, TS)]
pub struct Requests {
    pub requests: Vec<RequestSummary>,
}

#[derive(Debug, Serialize, TS)]
pub struct RequestSummary {
    pub id: String,
    pub kind: DialogKind,
    pub question: String,
    pub created_at: DateTime<Utc>,
}

impl Op for ListRequests {
    const NAME: &'static str = "list_requests";
    const DESCRIPTION: &'static str =
        "Your outstanding requests to the user (questions, secret requests) with their ids.";
    const WHO: Who = Who::UserAndAgents;
    type Output = Requests;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Requests, OpError> {
        let instance = caller.agent("only agents have requests")?;
        let requests = hub
            .store
            .open_dialogs()
            .await?
            .into_iter()
            .filter(|d| d.instance == instance)
            .map(|d| RequestSummary { id: d.id, kind: d.kind, question: d.question, created_at: d.created_at })
            .collect();
        Ok(Requests { requests })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct CancelRequest {
    pub id: String,
    /// Why it is no longer needed.
    pub reason: String,
}

#[derive(Debug, Serialize, TS)]
pub struct RequestCancelled {
    /// Whether it was still open.
    pub cancelled: bool,
    pub id: String,
}

impl Op for CancelRequest {
    const NAME: &'static str = "cancel_request";
    const DESCRIPTION: &'static str = "Withdraw one of your outstanding requests by id when it no longer matters, e.g. the user answered it in the chat (it disappears for the user; a late answer is rejected). Withdrawing a proposal's approval request withdraws the proposal.";
    const WHO: Who = Who::UserAndAgents;
    type Output = RequestCancelled;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<RequestCancelled, OpError> {
        let instance = caller.agent("only agents have requests")?;
        let d = hub.store.dialog(&self.id).await?;
        if d.instance != instance {
            return Err(OpError::Forbidden("that request is not yours".into()));
        }
        let why = format!("cancelled by {instance}: {}", self.reason);
        let cancelled = hub.store.close_dialog(&self.id, DialogStatus::Aborted, &why).await?;
        if cancelled {
            crate::notify::dialog_closed(hub, &d);
        }
        if let Some(p) = &d.proposal_id {
            crate::transactions::withdraw(hub, p, &why).await?;
        }
        Ok(RequestCancelled { cancelled, id: self.id })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct AskUser {
    pub question: String,
    /// Allowed answers, e.g. ["yes", "no"]. Empty means free text.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub options: Vec<String>,
    /// Background the user needs to answer.
    #[serde(default)]
    pub detail: Option<String>,
    /// Objects whose change withdraws the question, e.g. `proposal:<id>`.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub about: Vec<String>,
    /// `blocking` (you cannot continue; notifies the user), `normal`, or `info`.
    #[serde(default)]
    pub priority: Option<String>,
    /// Withdraw the question after this long, e.g. `1d`.
    #[serde(default)]
    pub expires_in: Option<String>,
    /// A form instead of free text or `options`: a JSON Schema object whose `properties` are the
    /// fields (`title`, `description`, `default`, `required`). Strings (with `enum`, or `format`
    /// date / date-time / time / file, or `"x-multiline": true`), numbers, integers, booleans,
    /// and arrays of `enum` strings (multi-select) or of files. Never ask for secrets in a form;
    /// use `request_secrets`.
    #[serde(default)]
    #[ts(optional, type = "Record<string, unknown>")]
    pub form: Option<serde_json::Value>,
}

/// Answer a question (or a visit request) and tell its chat.
pub async fn answer_question(
    hub: &Hub,
    id: &str,
    answer: String,
    values: Option<serde_json::Value>,
) -> Result<botler_store::Dialog, HubError> {
    let d = hub.store.answer_dialog(id, &answer).await?;
    let payload = botler_core::payloads::BuiltinPayload::DialogAnswered(botler_core::payloads::DialogAnsweredPayload {
        dialog_id: d.id.clone(),
        question: d.question.clone(),
        answer,
        group: None,
        outcome: None,
        values,
        note: None,
    });
    hub.emit(crate::hub::builtin(payload, botler_core::Source::User)?.to(d.instance.clone())).await?;
    Ok(d)
}

/// What a question dialog's `spec` holds.
#[derive(Debug, Serialize, Deserialize)]
pub struct QuestionSpec {
    pub form: serde_json::Value,
}

#[derive(Debug, Serialize, TS)]
pub struct Asked {
    pub dialog_id: String,
    pub note: String,
}

impl Op for AskUser {
    const NAME: &'static str = "ask_user";
    const DESCRIPTION: &'static str = "Ask the user a question and get the answer later as a `dialog.answered` event. `options` are the allowed answers; `form` (a JSON Schema object) asks for several typed fields at once (choices, multi-select, numbers, dates, files) and the answer's `values` match it; `about` lists objects (e.g. `proposal:<id>`) whose change withdraws the question.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = Asked;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Asked, OpError> {
        let instance = caller.agent("only agents ask the user")?;
        let priority = match self.priority.as_deref().unwrap_or("blocking") {
            "blocking" => Priority::Blocking,
            "normal" => Priority::Normal,
            "info" => Priority::Info,
            other => return Err(OpError::BadRequest(format!("unknown priority {other}"))),
        };
        let expires_at = self.expires_in.as_deref().map(|d| parse_when(None, Some(d), Utc::now())).transpose()?;
        let spec = match self.form {
            Some(_) if !self.options.is_empty() => {
                return Err(OpError::BadRequest(
                    "give either `options` or a `form`, not both; a form can hold an `enum` field".into(),
                ));
            }
            Some(form) => {
                crate::forms::check_schema(&form).map_err(|e| OpError::BadRequest(format!("form: {e}")))?;
                Some(to_json(&QuestionSpec { form })?)
            }
            None => None,
        };
        let d = hub
            .store
            .create_dialog(&NewDialog {
                instance: instance.into(),
                kind: DialogKind::Question,
                question: self.question,
                detail: self.detail,
                ctas: self.options,
                about: self.about,
                priority,
                proposal_id: None,
                spec,
                expires_at,
            })
            .await?;
        crate::notify::dialog_opened(hub, &d).await;
        Ok(Asked {
            dialog_id: d.id,
            note: "The answer arrives later as a dialog.answered event; continue with other work or stop.".into(),
        })
    }
}

/// The answers of a visit request.
pub const VISIT_DONE: &str = "done";
pub const VISIT_CANCEL: &str = "cancel";

/// What a visit request asks the user to open (stored as the dialog's `spec`). The web app builds
/// the link from the host name it is browsing on, `host_port` and `path`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct VisitSpec {
    /// The service whose published port this is.
    pub service: String,
    pub host_port: u16,
    /// Starts with `/`.
    pub path: String,
    pub title: String,
    pub instructions: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct RequestVisit {
    /// A host port published by a service this chat may use.
    pub host_port: u16,
    /// Path to open, e.g. `/login`. Defaults to `/`.
    #[serde(default)]
    pub path: Option<String>,
    /// What the visit is for, e.g. "Log in to Home Assistant".
    pub title: String,
    /// What the user should do there, step by step.
    pub instructions: String,
    /// Withdraw the request after this long, e.g. `1d`.
    #[serde(default)]
    pub expires_in: Option<String>,
}

/// The service publishing `host_port`, if `instance` may use it (its definition's `mcp`, or a
/// grant).
async fn visit_service(hub: &Hub, instance: &str, host_port: u16) -> Result<String, OpError> {
    let applied = hub.applied().await;
    let Some(job) = applied.defs.jobs.values().find(|j| j.spec.env.publish.iter().any(|p| p.host_port == host_port))
    else {
        return Err(OpError::BadRequest(format!("no service publishes host port {host_port}")));
    };
    let r = Resource::new(ResourceKind::Service, job.name.clone());
    if !crate::grants::holds(hub, instance, &r, None).await? {
        return Err(OpError::Forbidden(format!(
            "port {host_port} belongs to service `{}`, which this chat may not use; `request_access(\"{r}\")` first",
            job.name
        )));
    }
    Ok(job.name.clone())
}

impl Op for RequestVisit {
    const NAME: &'static str = "request_visit";
    const DESCRIPTION: &'static str = "Ask the user to open a page of a service in their browser (e.g. to log in) and tell you when done. `host_port` must be a port published by a service this chat may use. The answer (`done` or `cancel`) arrives as a `dialog.answered` event.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = Asked;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Asked, OpError> {
        let instance = caller.agent("only agents request visits")?;
        let path = self.path.unwrap_or_else(|| "/".into());
        if !path.starts_with('/') || path.chars().any(char::is_whitespace) {
            return Err(OpError::BadRequest(format!("path {path:?} must start with `/` and contain no spaces")));
        }
        if self.title.trim().is_empty() || self.instructions.trim().is_empty() {
            return Err(OpError::BadRequest(
                "say what the visit is for (`title`) and what to do (`instructions`)".into(),
            ));
        }
        let service = visit_service(hub, instance, self.host_port).await?;
        let expires_at = self.expires_in.as_deref().map(|d| parse_when(None, Some(d), Utc::now())).transpose()?;
        let spec = VisitSpec {
            service,
            host_port: self.host_port,
            path,
            title: self.title.clone(),
            instructions: self.instructions.clone(),
        };
        let d = hub
            .store
            .create_dialog(&NewDialog {
                instance: instance.into(),
                kind: DialogKind::Visit,
                question: self.title,
                detail: Some(self.instructions),
                ctas: vec![VISIT_DONE.into(), VISIT_CANCEL.into()],
                about: vec![],
                priority: Priority::Blocking,
                proposal_id: None,
                spec: Some(serde_json::to_value(&spec).map_err(|e| HubError::Io(std::io::Error::other(e)))?),
                expires_at,
            })
            .await?;
        crate::notify::dialog_opened(hub, &d).await;
        Ok(Asked {
            dialog_id: d.id,
            note: "The user's answer (`done` or `cancel`) arrives as a dialog.answered event; stop until then.".into(),
        })
    }
}
