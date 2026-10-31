//! Channels: the user talking to Botler outside the web app (Telegram, email, ...).
//!
//! A channel is a service with `channel = true`, built by agents. It speaks the outside protocol
//! and hands the hub what arrives (`channel_inbound`); the hub decides who that is. A sender is
//! never trusted until the user pairs it: they create a one-time code in the web app and send
//! `pair <code>` from the outside account. A paired sender's messages are the user's, with `via`
//! saying where they came from, and go to the chat the binding names (Botler unless switched
//! with `/chat <name>`). Everything the user should see there (the reply that ends a turn the
//! channel started, files the agent attaches, questions it asks) goes into the channel's outbox,
//! which the service polls (`channel_outbox`) and acknowledges (`channel_ack`).
//!
//! Approvals, secrets and logins are never answered over a channel; those get a notice to open
//! the web app.

use botler_core::payloads::{BuiltinPayload, MessagePayload, SendMode, Via};
use botler_core::{FRONT_DOOR, Source};
use botler_store::{ChannelBinding, Dialog, DialogKind, OutboxItem};
use chrono::{DateTime, Duration, Utc};
use rand::Rng;

use crate::hub::{Hub, Result, builtin};

/// How long a pairing code works.
pub const CODE_MINUTES: i64 = 10;
/// The longest reply text queued (channels split or trim further for their protocol).
const MAX_TEXT: usize = 8000;

/// What became of an inbound message.
#[derive(Clone, Debug, PartialEq, serde::Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Inbound {
    /// Delivered to `to` as a message from the user.
    Delivered { to: String, event_id: String },
    /// It answered an open question.
    Answered { dialog_id: String },
    /// A `pair <code>` that bound the sender; a confirmation is in the outbox.
    Paired { to: String },
    /// A command (`/chat <name>`); its reply is in the outbox.
    Command,
    /// The sender is not paired (or the code was wrong): dropped and counted. The service sends
    /// nothing back, so strangers learn nothing.
    Ignored,
}

/// A new one-time pairing code for messages to go to `target`.
pub async fn create_code(hub: &Hub, target: &str) -> Result<(String, DateTime<Utc>)> {
    // 31^8 ≈ 10^12 codes, so guessing one from outside in its lifetime is hopeless; no 0/O, 1/I/L.
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    let code: String = {
        let mut rng = rand::rng();
        (0..8).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
    };
    let expires = Utc::now() + Duration::minutes(CODE_MINUTES);
    hub.store.create_channel_code(&code, target, expires).await?;
    Ok((code, expires))
}

/// A message `sender` sent on `channel`, with `files` already stored as uploads.
pub async fn inbound(
    hub: &Hub,
    channel: &str,
    sender: &str,
    text: &str,
    files: &[String],
    reply_ref: Option<&str>,
) -> Result<Inbound> {
    let text = text.trim();
    if let Some(code) = text.strip_prefix("pair ").map(|c| c.trim().to_ascii_uppercase())
        && let Some(target) = hub.store.use_channel_code(&code, Utc::now()).await?
    {
        let b = hub.store.bind_channel(channel, sender, &target).await?;
        tracing::info!(channel, sender, target = %b.target, "channel sender paired");
        let note =
            format!("Paired. Your messages here now go to {}. Send /chat <name> to talk to another chat.", b.target);
        hub.store.queue_outbox(channel, sender, &note, &[], None).await?;
        return Ok(Inbound::Paired { to: b.target });
    }
    let Some(binding) = hub.store.channel_binding(channel, sender).await? else {
        tracing::info!(channel, sender, "message from an unpaired sender dropped");
        hub.store.record_channel_stranger(channel, sender).await?;
        return Ok(Inbound::Ignored);
    };
    if let Some(name) = text.strip_prefix("/chat").filter(|r| r.is_empty() || r.starts_with(' ')).map(str::trim) {
        let reply = switch(hub, &binding, name).await?;
        hub.store.queue_outbox(channel, sender, &reply, &[], None).await?;
        return Ok(Inbound::Command);
    }
    if let Some((id, answer)) = question_answered(hub, &binding, text, reply_ref).await? {
        crate::ops::answer_question(hub, &id, answer, None).await?;
        return Ok(Inbound::Answered { dialog_id: id });
    }
    let to = binding.target.clone();
    let files = crate::uploads::attach(hub, &to, files).await?;
    let via = Via { channel: channel.into(), sender: sender.into() };
    let payload = MessagePayload { text: text.into(), files, mode: Some(SendMode::Steer), via: Some(via) };
    let ev = hub.emit(builtin(BuiltinPayload::Message(payload), Source::User)?.to(to.clone())).await?;
    Ok(Inbound::Delivered { to, event_id: ev.id.to_string() })
}

/// `/chat <name>`: send the binding's messages to another chat (empty: say where they go).
async fn switch(hub: &Hub, b: &ChannelBinding, name: &str) -> Result<String> {
    if name.is_empty() {
        return Ok(format!(
            "Your messages go to {}. Send /chat <name> to switch, /chat {FRONT_DOOR} to go back.",
            b.target
        ));
    }
    // A chat, or an agent definition whose instance is started by the first message (Botler).
    let target = match hub.store.resolve_instance(name).await? {
        Some(i) => Some(i.name),
        None => hub.applied().await.defs.agents.contains_key(name).then(|| name.to_owned()),
    };
    match target {
        Some(t) => {
            hub.store.retarget_channel(&b.id, &t).await?;
            Ok(format!("Your messages now go to {t}."))
        }
        None => Ok(format!("No chat named {name}. Your messages still go to {}.", b.target)),
    }
}

/// The open question `text` answers, with the answer as stored: the question a reply refers to,
/// or the target chat's only open question when the text is one of its choices.
async fn question_answered(
    hub: &Hub,
    b: &ChannelBinding,
    text: &str,
    reply_ref: Option<&str>,
) -> Result<Option<(String, String)>> {
    // A choice as the agent wrote it; free text only for a question without choices.
    let answer = |d: &Dialog| match d.ctas.iter().find(|c| c.eq_ignore_ascii_case(text)) {
        Some(c) => Some(c.clone()),
        None => d.ctas.is_empty().then(|| text.to_owned()),
    };
    if let Some(r) = reply_ref
        && let Ok(item) = hub.store.outbox_item(r).await
        && item.channel == b.channel
        && let Some(id) = item.dialog_id
    {
        let d = hub.store.dialog(&id).await?;
        if answerable(&d)
            && let Some(a) = answer(&d)
        {
            return Ok(Some((d.id, a)));
        }
    }
    let open: Vec<Dialog> =
        hub.store.open_dialogs().await?.into_iter().filter(|d| d.instance == b.target && answerable(d)).collect();
    if let [d] = open.as_slice()
        && !d.ctas.is_empty()
        && let Some(a) = answer(d)
    {
        return Ok(Some((d.id.clone(), a)));
    }
    Ok(None)
}

/// Questions without a form can be answered over a channel; everything else needs the web app.
fn answerable(d: &Dialog) -> bool {
    d.kind == DialogKind::Question && d.spec.is_none() && d.status == botler_store::DialogStatus::Open
}

/// The live binding a chat's current turn came in on, if it came over a channel.
pub async fn current(hub: &Hub, instance: &str) -> Result<Option<ChannelBinding>> {
    let Some(d) = hub.store.last_delivered(instance).await? else { return Ok(None) };
    let ev = hub.store.event(&d.event_id).await?;
    let Some(via) = ev.payload.get("via").and_then(|v| serde_json::from_value::<Via>(v.clone()).ok()) else {
        return Ok(None);
    };
    Ok(hub.store.channel_binding(&via.channel, &via.sender).await?.filter(|b| b.target == instance))
}

/// A turn ended with `reply`: send it back if the turn came over a channel.
pub async fn turn_ended(hub: &Hub, instance: &str, reply: &str) -> Result<()> {
    let reply = reply.trim();
    if reply.is_empty() {
        return Ok(());
    }
    if let Some(b) = current(hub, instance).await? {
        let text: String = reply.chars().take(MAX_TEXT).collect();
        hub.store.queue_outbox(&b.channel, &b.sender, &text, &[], None).await?;
    }
    Ok(())
}

/// Files the agent attached to its reply go out too.
pub async fn attached(hub: &Hub, instance: &str, files: &[String]) -> Result<()> {
    if let Some(b) = current(hub, instance).await? {
        hub.store.queue_outbox(&b.channel, &b.sender, "", files, None).await?;
    }
    Ok(())
}

/// A chat opened a request while talking over a channel: ask there too (questions), or say that
/// the web app has something waiting.
pub async fn dialog_opened(hub: &Hub, d: &Dialog) -> Result<()> {
    let Some(b) = current(hub, &d.instance).await? else { return Ok(()) };
    let text = if answerable(d) {
        let mut t = format!("Question: {}", d.question);
        if let Some(detail) = &d.detail {
            t.push_str(&format!("\n{detail}"));
        }
        if !d.ctas.is_empty() {
            t.push_str(&format!("\nAnswer with one of: {}", d.ctas.join(" / ")));
        }
        t
    } else {
        format!("Waiting for you in the web app (Requests): {}", d.question)
    };
    hub.store.queue_outbox(&b.channel, &b.sender, &text, &[], answerable(d).then_some(d.id.as_str())).await?;
    Ok(())
}

/// What `channel` has to send.
pub async fn pending(hub: &Hub, channel: &str, limit: i64) -> Result<Vec<OutboxItem>> {
    Ok(hub.store.pending_outbox(channel, limit).await?)
}

/// The Channels section of the web app.
#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct ChannelsView {
    /// The channel services defined (`channel = true`).
    pub channels: Vec<String>,
    pub bindings: Vec<ChannelBinding>,
    /// Unpaired senders whose messages were dropped.
    pub strangers: Vec<botler_store::ChannelStranger>,
}

#[derive(Debug, serde::Deserialize, ts_rs::TS)]
#[ts(optional_fields)]
pub struct NewPairingCode {
    /// The chat paired messages go to; Botler by default.
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct PairingCode {
    pub code: String,
    pub target: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize, ts_rs::TS)]
pub struct BindingRevoked {
    pub revoked: bool,
}

pub async fn view(hub: &Hub) -> Result<ChannelsView> {
    let applied = hub.applied().await;
    let channels = applied.defs.jobs.values().filter(|j| j.spec.channel).map(|j| j.name.clone()).collect();
    Ok(ChannelsView {
        channels,
        bindings: hub.store.channel_bindings().await?,
        strangers: hub.store.channel_strangers().await?,
    })
}
