//! The ops a channel service calls (see `channels.rs`).

use std::sync::Arc;

use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::channels::Inbound;
use crate::hub::{Hub, HubError};

/// The longest outbox wait.
const MAX_WAIT_SECONDS: u64 = 50;
/// The largest file a channel hands over or gets.
const MAX_FILE: usize = 25 * 1024 * 1024;

/// The channel service calling, or why it may not.
async fn channel_of(hub: &Hub, caller: &Caller) -> Result<String, OpError> {
    let name = caller.runnable.clone().ok_or_else(|| OpError::Forbidden("only channel services call this".into()))?;
    let applied = hub.applied().await;
    if !applied.defs.jobs.get(&name).is_some_and(|j| j.spec.channel) {
        return Err(OpError::Forbidden(format!("`{name}` is not a channel; set `channel = true` in its job.toml")));
    }
    Ok(name)
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ChannelFile {
    /// A plain file name, e.g. `photo.jpg`.
    pub name: String,
    /// The content, base64.
    pub base64: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ChannelInbound {
    /// The outside account, stable across messages (a Telegram chat id, an email address, ...).
    pub sender: String,
    pub text: String,
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<ChannelFile>>")]
    pub files: Vec<ChannelFile>,
    /// The outbox item this message replies to, if the protocol says (answers that question).
    #[serde(default)]
    pub reply_ref: Option<String>,
}

impl Op for ChannelInbound {
    const NAME: &'static str = "channel_inbound";
    const DESCRIPTION: &'static str = "Channel services: a message arrived from `sender`. The hub decides whether that is the user (paired with `pair <code>`) and where it goes; unpaired senders are ignored and you send them nothing.";
    const WHO: Who = Who::Runs;
    type Output = Inbound;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Inbound, OpError> {
        if let Some(trial) = crate::trials::of_caller(hub, caller).await? {
            let detail = serde_json::json!({ "sender": self.sender, "text": self.text, "files": self.files.len() });
            crate::trials::record(hub, &trial, Self::NAME, &detail).await?;
            return Ok(Inbound::Ignored);
        }
        let channel = channel_of(hub, caller).await?;
        if self.sender.trim().is_empty() {
            return Err(OpError::BadRequest("`sender` is empty: give the outside account's stable id".into()));
        }
        let mut uploads = Vec::new();
        for f in &self.files {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&f.base64)
                .map_err(|e| OpError::BadRequest(format!("file {}: not base64: {e}", f.name)))?;
            if bytes.len() > MAX_FILE {
                return Err(OpError::BadRequest(format!("file {} is over {MAX_FILE} bytes", f.name)));
            }
            uploads.push(crate::uploads::save(hub, &f.name, &bytes).await.map_err(HubError::from)?.file);
        }
        Ok(crate::channels::inbound(hub, &channel, &self.sender, &self.text, &uploads, self.reply_ref.as_deref())
            .await?)
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ChannelOutbox {
    /// Wait up to this many seconds (at most 50) for something to send. 0: return at once.
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

#[derive(Debug, Serialize, TS)]
pub struct OutboxFile {
    pub name: String,
    pub mime: String,
    pub base64: String,
}

#[derive(Debug, Serialize, TS)]
pub struct OutboxMessage {
    /// Acknowledge with `channel_ack(id)` once sent; pass it as `reply_ref` when the user replies to it.
    pub id: String,
    /// Who to send it to (as `channel_inbound` named them).
    pub sender: String,
    /// Plain text; may be empty when only files go out.
    pub text: String,
    pub files: Vec<OutboxFile>,
    /// A question: the user's reply answers it.
    pub question: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct Outbox {
    pub messages: Vec<OutboxMessage>,
}

impl Op for ChannelOutbox {
    const NAME: &'static str = "channel_outbox";
    const DESCRIPTION: &'static str = "Channel services: what to send, waiting up to `wait_seconds` for something (a long poll; no open port needed). Send each message, then `channel_ack(id)`; unacknowledged messages come again.";
    const WHO: Who = Who::Runs;
    type Output = Outbox;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Outbox, OpError> {
        if crate::trials::of_caller(hub, caller).await?.is_some() {
            return Ok(Outbox { messages: vec![] });
        }
        let channel = channel_of(hub, caller).await?;
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(self.wait_seconds.unwrap_or(0).min(MAX_WAIT_SECONDS));
        let items = loop {
            let items = crate::channels::pending(hub, &channel, 20).await?;
            if !items.is_empty() || tokio::time::Instant::now() >= deadline {
                break items;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        };
        let mut messages = Vec::with_capacity(items.len());
        for it in items {
            let mut files = Vec::new();
            for f in &it.files {
                let (bytes, mime, _) = crate::uploads::read(hub, f).await.map_err(HubError::from)?;
                let name = f.rsplit('/').next().unwrap_or(f).to_owned();
                files.push(OutboxFile {
                    name,
                    mime: mime.into(),
                    base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                });
            }
            messages.push(OutboxMessage {
                question: it.dialog_id.is_some(),
                id: it.id,
                sender: it.sender,
                text: it.text,
                files,
            });
        }
        Ok(Outbox { messages })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ChannelAck {
    pub id: String,
}

#[derive(Debug, Serialize, TS)]
pub struct Acked {
    /// False if it was already acknowledged (or is not this channel's).
    pub acked: bool,
}

impl Op for ChannelAck {
    const NAME: &'static str = "channel_ack";
    const DESCRIPTION: &'static str = "Channel services: an outbox message was sent; it will not come again.";
    const WHO: Who = Who::Runs;
    type Output = Acked;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Acked, OpError> {
        if crate::trials::of_caller(hub, caller).await?.is_some() {
            return Ok(Acked { acked: false });
        }
        let channel = channel_of(hub, caller).await?;
        Ok(Acked { acked: hub.store.ack_outbox(&channel, &self.id).await? })
    }
}
