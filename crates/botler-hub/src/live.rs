//! `GET /api/v1/live`: what changes, pushed to the web app over a WebSocket.
//!
//! Browsers cannot set headers on a WebSocket, and a token in the URL ends up in proxy logs, so
//! the client authenticates with its first message, `{"type": "auth", "token": "..."}` (an empty
//! token for a signed-in device, whose cookie came with the upgrade from the web app's origin), within
//! [`AUTH_TIMEOUT`]. The server answers `{"kind": "ready"}` (from then on every change arrives)
//! or `{"kind": "error"}` and closes. `{"type": "watch", "instance": "...", "after": "<item id>"}`
//! selects the one chat on screen: its pi events (streaming text, tool calls) come through, and
//! whenever its session changes, `session` brings the items after the last one the client has
//! (`after` says which, so a client whose last item differs asks again from its own). Everything
//! else is broadcast.
//!
//! Messages are [`ClientMessage`] and [`ServerMessage`]; the web app's types are generated
//! from them.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use botler_core::Event;
use botler_store::Proposal;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use ts_rs::TS;

use crate::auth::Principal;
use crate::hub::{Hub, Live};
use crate::notify::DialogNotice;
use crate::phase::AgentStatus;
use crate::sessions::Window;

/// How long a new connection may take to authenticate.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep-alive pings, so idle connections survive proxies.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Changes to the watched chat within this long go out as one `session` message.
const SESSION_DELAY: Duration = Duration::from_millis(250);
/// Items a `session` message carries when it cannot continue the client's (the latest ones).
const SESSION_PAGE: usize = 50;
/// The pi events after which the session file has new entries (or the chat's state changed).
const SESSION_CHANGES: &[&str] =
    &["message_end", "tool_execution_end", "agent_start", "agent_end", "agent_settled", "compaction_end"];
/// Close code for a failed authentication (application range 4000–4999).
pub const CLOSE_UNAUTHORIZED: u16 = 4401;

/// Client to server.
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Must be the first message.
    Auth { token: String },
    /// Follow this chat (or none): its pi events, and its session's items after `after` (the
    /// last item the client has; without it, the latest page once something changes).
    Watch {
        instance: Option<String>,
        #[serde(default)]
        #[ts(optional)]
        after: Option<String>,
    },
}

/// Server to client.
#[derive(Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Authenticated; changes from now on are delivered.
    Ready,
    /// Authentication failed or the client sent something invalid.
    Error { error: String },
    /// The client fell behind and missed messages: refetch everything.
    Lagged { missed: u64 },
    /// An event was journaled.
    Event { event: Event },
    /// An agent's runtime status changed.
    Instance { instance: String, status: AgentStatus },
    /// A dialog opened or closed: refetch the requests.
    Dialog { notice: DialogNotice },
    /// A proposal moved (stage, status, decision).
    Proposal { proposal: Box<Proposal> },
    /// The watched chat's session changed: its items after `after` (`view.continues`), or its
    /// latest page (`after` null: replace what is shown).
    Session { after: Option<String>, view: Box<crate::wire::SessionView> },
    /// A raw pi RPC event of the watched chat (`message_update`, `tool_execution_end`, ...).
    Pi {
        instance: String,
        #[ts(type = "Record<string, unknown>")]
        event: serde_json::Value,
    },
}

#[derive(Debug, thiserror::Error)]
enum LiveError {
    #[error("websocket: {0}")]
    Socket(#[from] axum::Error),
    #[error("encoding a message: {0}")]
    Encode(#[from] serde_json::Error),
}

/// A pi event without the bytes of its image blocks (a screenshot in a tool result can be
/// megabytes of base64, in `tool_execution_end`, `message_end`, `turn_end`, `agent_end`, ...).
/// The web app only refreshes the session on those events, which references the images.
pub fn without_image_data(mut event: serde_json::Value) -> serde_json::Value {
    fn strip(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            serde_json::Value::Object(o) => {
                if o.get("type").and_then(serde_json::Value::as_str) == Some("image") {
                    o.remove("data");
                }
                o.values_mut().for_each(strip);
            }
            _ => {}
        }
    }
    strip(&mut event);
    event
}

pub async fn socket(State(hub): State<Arc<Hub>>, headers: axum::http::HeaderMap, ws: WebSocketUpgrade) -> Response {
    // A signed-in device's cookie counts, but only from the web app's own origin: browsers send
    // cookies on cross-origin WebSocket upgrades too, and a WebSocket has no preflight.
    let same_origin = match (headers.get("origin"), headers.get(axum::http::header::HOST)) {
        (Some(o), Some(h)) => {
            o.to_str().ok().and_then(|o| o.split_once("://")).map(|(_, rest)| rest) == h.to_str().ok()
        }
        _ => false,
    };
    let device = match crate::login::cookie_in(&headers).filter(|_| same_origin) {
        Some(c) => match crate::login::device_of(&hub, &c).await {
            Ok(d) => d.is_some(),
            Err(e) => {
                tracing::error!(error = %e, "could not check the session cookie");
                false
            }
        },
        None => false,
    };
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = serve(hub, socket, device).await {
            tracing::debug!(error = %e, "live connection ended");
        }
    })
}

async fn send(socket: &mut WebSocket, msg: &ServerMessage) -> Result<(), LiveError> {
    socket.send(Message::Text(serde_json::to_string(msg)?.into())).await?;
    Ok(())
}

async fn reject(socket: &mut WebSocket, error: String) -> Result<(), LiveError> {
    send(socket, &ServerMessage::Error { error: error.clone() }).await?;
    socket.send(Message::Close(Some(CloseFrame { code: CLOSE_UNAUTHORIZED, reason: error.into() }))).await?;
    Ok(())
}

/// Wait for the auth message; `Err` carries what to tell the client.
async fn authenticate(hub: &Hub, socket: &mut WebSocket, device: bool) -> Result<Result<(), String>, LiveError> {
    let first = match tokio::time::timeout(AUTH_TIMEOUT, socket.recv()).await {
        Err(_) => return Ok(Err("authenticate within 10 seconds".into())),
        Ok(None) => return Ok(Err("closed before authenticating".into())),
        Ok(Some(m)) => m?,
    };
    let Message::Text(text) = first else { return Ok(Err("expected an auth message".into())) };
    let token = match serde_json::from_str::<ClientMessage>(&text) {
        Ok(ClientMessage::Auth { token }) => token,
        Ok(_) => return Ok(Err("the first message must be {\"type\": \"auth\", \"token\": ...}".into())),
        Err(e) => return Ok(Err(format!("invalid message: {e}"))),
    };
    if device && token.is_empty() {
        return Ok(Ok(()));
    }
    Ok(match hub.tokens.verify(&token, Utc::now()) {
        Ok(Principal::User) => Ok(()),
        Ok(_) => Err("the live stream is for the user".into()),
        Err(e) => Err(e.to_string()),
    })
}

/// The chat a connection follows.
struct Watched {
    instance: String,
    /// The last item the client has.
    after: Option<String>,
    pi: tokio::sync::broadcast::Receiver<serde_json::Value>,
    /// When to send the session's new items.
    due: Option<tokio::time::Instant>,
}

impl Watched {
    fn changed(&mut self) {
        self.due.get_or_insert_with(|| tokio::time::Instant::now() + SESSION_DELAY);
    }
}

/// The watched chat's session after what the client has; moves `after` on.
async fn session(hub: &Hub, w: &mut Watched) -> Result<Option<ServerMessage>, crate::api::ApiError> {
    let Some(inst) = hub.store.resolve_instance(&w.instance).await.map_err(crate::hub::HubError::from)? else {
        return Ok(None);
    };
    let window = Window { last: Some(SESSION_PAGE), after: w.after.as_deref(), before: None };
    let view = crate::api::session_view(hub, inst, None, window).await?;
    let after = w.after.clone().filter(|_| view.continues);
    if let Some(last) = view.items.last() {
        w.after = Some(last.id().to_string());
    }
    Ok(Some(ServerMessage::Session { after, view: Box::new(view) }))
}

async fn next_pi(w: &mut Option<Watched>) -> Result<serde_json::Value, RecvError> {
    match w {
        Some(w) => w.pi.recv().await,
        None => std::future::pending().await,
    }
}

async fn serve(hub: Arc<Hub>, mut socket: WebSocket, device: bool) -> Result<(), LiveError> {
    if let Err(why) = authenticate(&hub, &mut socket, device).await? {
        return reject(&mut socket, why).await;
    }
    // Subscribe before saying ready, so nothing emitted after `ready` is missed.
    let mut rx = hub.live.subscribe();
    send(&mut socket, &ServerMessage::Ready).await?;
    let mut watched: Option<Watched> = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        let due = watched.as_ref().and_then(|w| w.due);
        tokio::select! {
            change = rx.recv() => {
                let msg = match change {
                    Ok(Live::Event { event }) => {
                        if let Some(w) = watched.as_mut().filter(|w| event.to.as_deref() == Some(w.instance.as_str())) {
                            w.changed();
                        }
                        ServerMessage::Event { event }
                    }
                    Ok(Live::Instance { instance, status }) => {
                        if let Some(w) = watched.as_mut().filter(|w| w.instance == instance) {
                            w.changed();
                        }
                        ServerMessage::Instance { instance, status }
                    }
                    Ok(Live::Dialog { notice }) => ServerMessage::Dialog { notice },
                    Ok(Live::Proposal { proposal }) => ServerMessage::Proposal { proposal },
                    Err(RecvError::Lagged(missed)) => ServerMessage::Lagged { missed },
                    Err(RecvError::Closed) => return Ok(()),
                };
                send(&mut socket, &msg).await?;
            }
            pi = next_pi(&mut watched) => {
                let Some(w) = watched.as_mut() else { continue };
                match pi {
                    Ok(event) => {
                        if event.get("type").and_then(serde_json::Value::as_str).is_some_and(|t| SESSION_CHANGES.contains(&t)) {
                            w.changed();
                        }
                        let instance = w.instance.clone();
                        send(&mut socket, &ServerMessage::Pi { instance, event }).await?;
                    }
                    // Streamed text was missed: the session brings what was written.
                    Err(RecvError::Lagged(_)) => w.changed(),
                    Err(RecvError::Closed) => w.pi = hub.watch_pi(&w.instance),
                }
            }
            _ = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)), if due.is_some() => {
                let Some(w) = watched.as_mut() else { continue };
                w.due = None;
                match session(&hub, w).await {
                    Ok(Some(msg)) => send(&mut socket, &msg).await?,
                    Ok(None) => {}
                    Err(e) => {
                        tracing::error!(chat = %w.instance, error = %e, "could not read the watched chat's session");
                        send(&mut socket, &ServerMessage::Error { error: format!("reading {}: {e}", w.instance) }).await?;
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Watch { instance: None, .. }) => watched = None,
                        Ok(ClientMessage::Watch { instance: Some(instance), after }) => {
                            // The same chat keeps its pi stream (nothing streamed is missed).
                            let pi = match watched.take() {
                                Some(w) if w.instance == instance => w.pi,
                                _ => hub.watch_pi(&instance),
                            };
                            // A client with items gets what came after them right away.
                            let due = after.is_some().then(tokio::time::Instant::now);
                            watched = Some(Watched { instance, after, pi, due });
                        }
                        Ok(ClientMessage::Auth { .. }) => {}
                        Err(e) => send(&mut socket, &ServerMessage::Error { error: format!("invalid message: {e}") }).await?,
                    },
                    // Pings are answered by the socket itself; binary frames mean nothing here.
                    Some(Ok(_)) => {}
                }
            }
            _ = ping.tick() => socket.send(Message::Ping(Vec::new().into())).await?,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn image_bytes_stay_out_of_the_stream() {
        let ev = json!({"type": "tool_execution_end", "result": {"content": [
            {"type": "text", "text": "Read image file [image/png]"},
            {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
        ]}});
        assert_eq!(
            super::without_image_data(ev),
            json!({"type": "tool_execution_end", "result": {"content": [
                {"type": "text", "text": "Read image file [image/png]"},
                {"type": "image", "mimeType": "image/png"},
            ]}})
        );
    }
}
