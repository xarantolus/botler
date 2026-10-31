//! Messages and events: talking to agents, emitting facts, reading the journal.

use std::sync::Arc;

use botler_core::{Event, Provenance};
use botler_store::EventQuery;
use chrono::Utc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;
use uuid::Uuid;

use super::{Caller, Op, OpError, Who, resolve_chat};
use crate::auth::Principal;
use crate::hub::Hub;
use crate::uploads::Uploaded;

/// Store the files an agent names as uploads (only agents have a container the hub reads).
async fn agent_files(hub: &Hub, caller: &Caller, paths: &[String]) -> Result<Vec<Uploaded>, OpError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let me = caller.agent("files are paths in an agent's container; the user attaches uploads in the web app")?;
    Ok(crate::uploads::from_agent(hub, me, paths).await?)
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct SendMessage {
    /// Recipient instance name.
    pub to: String,
    pub text: String,
    /// Interrupt the recipient at its next turn instead of waiting until it is idle.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub urgent: bool,
    /// Event id this answers, to keep the conversation thread.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// You need an answer within this long, e.g. `20m`: if the recipient neither replies to you
    /// nor ends its turn by then, you get a `reply.overdue` event.
    #[serde(default)]
    pub expect_reply_within: Option<String>,
    /// Files from your container to send along (any type): paths in your home, your monorepo
    /// clone or `/inbox/files`. The recipient finds them under `/inbox/files`.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct MessageSent {
    pub event_id: Uuid,
    /// The recipient's instance name.
    pub to: String,
    /// The files sent along.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Uploaded>>")]
    pub files: Vec<Uploaded>,
    /// A trial run: recorded, not carried out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub dry_run: bool,
}

impl Op for SendMessage {
    const NAME: &'static str = "send_message";
    const DESCRIPTION: &'static str = "Send a message to an agent instance (e.g. `botler`). Replies come back to you as messages. Set `reply_to` to the event you are answering to keep the thread.";
    const WHO: Who = Who::Everyone;
    type Output = MessageSent;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<MessageSent, OpError> {
        let to = resolve_chat(hub, &self.to).await?;
        let within = match (&self.expect_reply_within, caller.instance()) {
            (None, _) => None,
            (Some(w), Some(_)) => Some(
                w.parse::<botler_core::duration::HumanDuration>()
                    .map_err(|e| OpError::BadRequest(format!("expect_reply_within {w:?}: {e}")))?,
            ),
            (Some(_), None) => {
                return Err(OpError::BadRequest("expect_reply_within is for agents waiting on an answer".into()));
            }
        };
        if caller.instance() == Some(to.as_str()) {
            return Err(OpError::BadRequest(
                "that is you: a message to yourself would only wake you again. To remember something, write it to a \
                 file in your home; to come back to it later, use set_timer."
                    .into(),
            ));
        }
        let files = agent_files(hub, caller, &self.files).await?;
        let names: Vec<String> = files.iter().map(|f| f.file.clone()).collect();
        let inbox = crate::uploads::attach(hub, &to, &names).await.map_err(crate::hub::HubError::from)?;
        // Messages between agents are trusted as messages; content an agent relays from outside is
        // marked by the event it came from (replies inherit the lower provenance of their parent).
        let mut ev = crate::hub::builtin(
            botler_core::payloads::BuiltinPayload::Message(botler_core::payloads::MessagePayload {
                text: self.text,
                files: inbox,
                mode: None,
                via: None,
            }),
            caller.source(),
        )?
        .to(to.clone())
        .urgent(self.urgent);
        if let Some(parent) = &self.reply_to {
            let parent = hub.store.event(parent).await?;
            ev = ev.in_reply_to(&parent);
        }
        if let Some(trial) = crate::trials::of_caller(hub, caller).await? {
            crate::trials::record(hub, &trial, Self::NAME, &ev).await?;
            return Ok(MessageSent { event_id: ev.id, to, files, dry_run: true });
        }
        let ev = hub.emit(ev).await?;
        if let (Some(me), Some(w)) = (caller.instance(), within) {
            crate::turns::expect_reply(hub, me, &to, Some(&ev.id.to_string()), Some(w.0)).await?;
        }
        if let Some(me) = caller.instance()
            && let Some(live) = hub.agents.get(me).await
        {
            live.record_message(&to);
        }
        Ok(MessageSent { event_id: ev.id, to, files, dry_run: false })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct AttachFiles {
    /// Paths in your container: your home (`/home/agent`, `~/...` or relative), your monorepo
    /// clone or `/inbox/files`. Any type (images, videos, audio, PDFs, archives, ...).
    pub files: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct FilesAttached {
    /// The files, as the user's chat shows them.
    pub files: Vec<Uploaded>,
}

impl Op for AttachFiles {
    const NAME: &'static str = "attach_files";
    const DESCRIPTION: &'static str = "Show files from your container to the user in your chat, where your reply is: a \
         screenshot, a picture, a video, audio, a PDF, any file (at most 100 MiB each). The paths must be in your \
         home, your monorepo clone or /inbox/files (copy a file from /tmp into your home first). To give files to \
         another chat, use send_message's `files`.";
    const WHO: Who = Who::UserAndAgents;
    type Output = FilesAttached;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<FilesAttached, OpError> {
        caller.agent("only agents attach files from their container; the user attaches uploads in the web app")?;
        if self.files.is_empty() {
            return Err(OpError::BadRequest("name at least one file".into()));
        }
        let files = agent_files(hub, caller, &self.files).await?;
        let names: Vec<String> = files.iter().map(|f| f.file.clone()).collect();
        crate::channels::attached(hub, caller.agent("")?, &names).await?;
        Ok(FilesAttached { files })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct EmitEvent {
    /// Dotted event type, e.g. `mail.received`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    #[ts(optional, as = "Option<Value>")]
    pub payload: Value,
    /// `external` (from a known outside source) or `untrusted` (arbitrary content). Defaults to
    /// `external`; events from agents and jobs are never `trusted`.
    #[serde(default)]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Serialize, TS)]
pub struct EventEmitted {
    pub event_id: Uuid,
    /// What is off about the event, if it was accepted anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning: Option<String>,
    /// A trial run: recorded, not carried out.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub dry_run: bool,
}

impl Op for EmitEvent {
    const NAME: &'static str = "emit_event";
    const DESCRIPTION: &'static str = "Emit a typed event. Triggers subscribed to its type react to it. Use for facts (`mail.received`), not for talking to someone (use send_message).";
    const WHO: Who = Who::Everyone;
    type Output = EventEmitted;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<EventEmitted, OpError> {
        if self.kind.is_empty()
            || !self.kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        {
            return Err(OpError::BadRequest("type must be lowercase dotted, e.g. mail.received".into()));
        }
        if botler_core::catalog::builtin(&self.kind).is_some() {
            return Err(OpError::Forbidden(format!("{} is a hub event type", self.kind)));
        }
        let provenance = match (caller.principal == Principal::User, self.provenance) {
            (true, p) => p.unwrap_or(Provenance::Trusted),
            // Agents and jobs can never claim trusted content.
            (false, Some(Provenance::Untrusted)) => Provenance::Untrusted,
            (false, _) => Provenance::External,
        };
        let applied = hub.applied().await;
        // A trial checks against its branch, where a new event type is declared.
        let trial = crate::trials::of_caller(hub, caller).await?;
        let branch_defs = match &trial {
            Some(run_id) => Some(crate::trials::definitions(hub, run_id).await?),
            None => None,
        };
        let defs = branch_defs.as_ref().unwrap_or(&applied.defs);
        let check = botler_core::event_check::check(defs, &self.kind, &self.payload);
        let warning = check.feedback(&self.kind);
        if !check.accepted() {
            tracing::info!(kind = %self.kind, caller = %caller.subject(), "rejected invalid event");
            return Err(OpError::InvalidEvent(warning.unwrap_or_else(|| format!("event {} rejected", self.kind))));
        }
        let ev = Event::new(self.kind, caller.source(), provenance, Utc::now()).payload(self.payload);
        if let Some(trial) = trial {
            crate::trials::record(hub, &trial, Self::NAME, &ev).await?;
            return Ok(EventEmitted { event_id: ev.id, warning, dry_run: true });
        }
        let ev = hub.emit(ev).await?;
        Ok(EventEmitted { event_id: ev.id, warning, dry_run: false })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct GetEvents {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub correlation: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl Op for GetEvents {
    const NAME: &'static str = "get_events";
    const DESCRIPTION: &'static str =
        "Read the event journal, newest first: filter by type (`mail.*`) or conversation (`correlation`).";
    const WHO: Who = Who::UserAndAgents;
    type Output = Vec<Event>;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Vec<Event>, OpError> {
        let q = EventQuery {
            kind: self.kind,
            correlation: self.correlation,
            since: None,
            limit: self.limit.unwrap_or(20).min(200),
        };
        Ok(hub.store.events(&q).await?)
    }
}
