//! Requests to the user: questions and secret requests, answered later as `dialog.answered`.

use std::sync::Arc;

use botler_store::{DialogKind, DialogStatus, NewDialog, Priority};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who, parse_when};
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
    const DESCRIPTION: &'static str = "Ask the user for secrets, asynchronously: create a group, add fields to one, re-enter values (`fill`) or remove fields. The user gets a form; you get a request id now and a `dialog.answered` event later saying what was provided (never the values). Do not continue work that needs the secrets until that event arrives. Using a group in a container still needs a grant (`secrets = [\"<group>\"]` in a definition, proposed as a change).";
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
    const DESCRIPTION: &'static str =
        "Withdraw one of your outstanding requests by id (it disappears for the user; a late answer is rejected).";
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
}

#[derive(Debug, Serialize, TS)]
pub struct Asked {
    pub dialog_id: String,
    pub note: String,
}

impl Op for AskUser {
    const NAME: &'static str = "ask_user";
    const DESCRIPTION: &'static str = "Ask the user a question and get the answer later as a `dialog.answered` event. `options` are the allowed answers; `about` lists objects (e.g. `proposal:<id>`) whose change withdraws the question.";
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
                spec: None,
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
