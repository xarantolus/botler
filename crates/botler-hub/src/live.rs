//! `GET /api/v1/live`: what changes, pushed to the web app over a WebSocket.
//!
//! Browsers cannot set headers on a WebSocket, and a token in the URL ends up in proxy logs, so
//! the client authenticates with its first message, `{"type": "auth", "token": "..."}`, within
//! [`AUTH_TIMEOUT`]. The server answers `{"kind": "ready"}` (from then on every change arrives)
//! or `{"kind": "error"}` and closes. `{"type": "watch", "instance": "..."}` selects the one chat
//! whose pi events (streaming text, tool calls) the client wants; everything else is broadcast.
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

/// How long a new connection may take to authenticate.
pub const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// Keep-alive pings, so idle connections survive proxies.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Close code for a failed authentication (application range 4000–4999).
pub const CLOSE_UNAUTHORIZED: u16 = 4401;

/// Client to server.
#[derive(Debug, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Must be the first message.
    Auth { token: String },
    /// Receive pi events for this chat (or none).
    Watch { instance: Option<String> },
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
    /// A dialog opened.
    Dialog { notice: DialogNotice },
    /// A proposal moved (stage, status, decision).
    Proposal { proposal: Box<Proposal> },
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

pub async fn socket(State(hub): State<Arc<Hub>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(e) = serve(hub, socket).await {
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
async fn authenticate(hub: &Hub, socket: &mut WebSocket) -> Result<Result<(), String>, LiveError> {
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
    Ok(match hub.tokens.verify(&token, Utc::now()) {
        Ok(Principal::User) => Ok(()),
        Ok(_) => Err("the live stream is for the user".into()),
        Err(e) => Err(e.to_string()),
    })
}

async fn serve(hub: Arc<Hub>, mut socket: WebSocket) -> Result<(), LiveError> {
    if let Err(why) = authenticate(&hub, &mut socket).await? {
        return reject(&mut socket, why).await;
    }
    // Subscribe before saying ready, so nothing emitted after `ready` is missed.
    let mut rx = hub.live.subscribe();
    send(&mut socket, &ServerMessage::Ready).await?;
    let mut watched: Option<String> = None;
    let mut ping = tokio::time::interval(PING_EVERY);
    ping.tick().await;
    loop {
        tokio::select! {
            change = rx.recv() => {
                let msg = match change {
                    Ok(Live::Event { event }) => ServerMessage::Event { event },
                    Ok(Live::Instance { instance, status }) => ServerMessage::Instance { instance, status },
                    Ok(Live::Dialog { notice }) => ServerMessage::Dialog { notice },
                    Ok(Live::Proposal { proposal }) => ServerMessage::Proposal { proposal },
                    Ok(Live::Pi { instance, event }) => {
                        if watched.as_deref() != Some(instance.as_str()) {
                            continue;
                        }
                        ServerMessage::Pi { instance, event }
                    }
                    Err(RecvError::Lagged(missed)) => ServerMessage::Lagged { missed },
                    Err(RecvError::Closed) => return Ok(()),
                };
                send(&mut socket, &msg).await?;
            }
            incoming = socket.recv() => {
                match incoming {
                    None | Some(Ok(Message::Close(_))) => return Ok(()),
                    Some(Err(e)) => return Err(e.into()),
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::Watch { instance }) => watched = instance,
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
