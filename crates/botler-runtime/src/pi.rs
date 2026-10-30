//! Client for pi's RPC mode (`pi --mode rpc`): JSON lines over stdio.
//!
//! Commands carry an `id` and get exactly one `response`. A rejected command
//! (`success: false`) produces no further events, so it is surfaced immediately as
//! [`PiError::Rejected`] rather than waited on.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum PiError {
    #[error("pi rejected {command}: {error}")]
    Rejected { command: String, error: String },
    #[error("pi process closed its output")]
    Closed,
    #[error("pi did not answer {command} within {timeout:?}")]
    Timeout { command: String, timeout: Duration },
    #[error("io talking to pi: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected response to {command}: {detail}")]
    Protocol { command: String, detail: String },
}

pub type Result<T> = std::result::Result<T, PiError>;

/// What pi did with a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// A run started.
    Started,
    /// Queued behind the current run (steer / follow-up).
    Queued,
    /// Consumed by an extension (e.g. a duplicate delivery).
    Handled,
}

/// How a prompt is delivered when the agent is busy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Streaming {
    /// After the current turn's tool calls.
    #[serde(rename = "steer")]
    Steer,
    /// After the current run finishes.
    #[serde(rename = "followUp")]
    FollowUp,
}

/// Model usage from one assistant message.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Usage {
    pub model: String,
    pub total_tokens: i64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PiEventKind {
    AgentStart,
    /// pi will not continue on its own: the instance is idle.
    AgentSettled,
    MessageEnd {
        role: String,
        usage: Option<Usage>,
        error: Option<String>,
    },
    ToolStart {
        tool: String,
    },
    ToolEnd {
        tool: String,
        is_error: bool,
    },
    Other {
        r#type: String,
    },
}

/// One event from pi. `raw` is forwarded verbatim to live viewers.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PiEvent {
    pub kind: PiEventKind,
    pub raw: Value,
}

impl PiEvent {
    pub fn parse(raw: Value) -> Self {
        let t = raw.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        let kind = match t.as_str() {
            "agent_start" => PiEventKind::AgentStart,
            "agent_settled" => PiEventKind::AgentSettled,
            "message_end" => {
                let msg = raw.get("message").cloned().unwrap_or(Value::Null);
                let role = msg.get("role").and_then(Value::as_str).unwrap_or("").to_string();
                let usage = msg.get("usage").filter(|_| role == "assistant").map(|u| Usage {
                    model: msg.get("model").and_then(Value::as_str).unwrap_or("").to_string(),
                    total_tokens: u.get("totalTokens").and_then(Value::as_i64).unwrap_or(0),
                    cost_usd: u.pointer("/cost/total").and_then(Value::as_f64).unwrap_or(0.0),
                });
                let error = msg.get("errorMessage").and_then(Value::as_str).map(String::from);
                PiEventKind::MessageEnd { role, usage, error }
            }
            "tool_execution_start" => {
                PiEventKind::ToolStart { tool: raw.get("toolName").and_then(Value::as_str).unwrap_or("").to_string() }
            }
            "tool_execution_end" => PiEventKind::ToolEnd {
                tool: raw.get("toolName").and_then(Value::as_str).unwrap_or("").to_string(),
                is_error: raw.get("isError").and_then(Value::as_bool).unwrap_or(false),
            },
            _ => PiEventKind::Other { r#type: t },
        };
        Self { kind, raw }
    }
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

/// A connected pi process. Cheap to clone; all clones share one connection.
#[derive(Clone)]
pub struct PiClient {
    writer: Arc<Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
    /// Set by the reader when pi's output ends; checked after registering a waiter so a command
    /// sent after the close fails immediately instead of timing out.
    closed: Arc<AtomicBool>,
    timeout: Duration,
}

impl std::fmt::Debug for PiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PiClient").finish_non_exhaustive()
    }
}

impl PiClient {
    /// Connect over a pi process's stdin/stdout. Events arrive on the returned receiver, which
    /// closes when pi's output ends.
    pub fn connect(
        stdin: impl AsyncWrite + Send + Unpin + 'static,
        stdout: impl AsyncRead + Send + Unpin + 'static,
        timeout: Duration,
    ) -> (Self, mpsc::Receiver<PiEvent>) {
        let pending: Pending = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel(1024);
        tokio::spawn(read_loop(stdout, pending.clone(), closed.clone(), tx));
        let client =
            Self { writer: Arc::new(Mutex::new(Box::new(stdin))), pending, next_id: Arc::default(), closed, timeout };
        (client, rx)
    }

    /// Send a command and wait for its response's `data`.
    pub async fn command(&self, mut cmd: Value) -> Result<Value> {
        let command = cmd.get("type").and_then(Value::as_str).unwrap_or("?").to_string();
        let id = format!("hub-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        cmd["id"] = json!(id);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        if self.closed.load(Ordering::SeqCst) {
            self.pending.lock().await.remove(&id);
            return Err(PiError::Closed);
        }
        let mut line = serde_json::to_vec(&cmd)
            .map_err(|e| PiError::Protocol { command: command.clone(), detail: e.to_string() })?;
        line.push(b'\n');
        let written = async {
            let mut w = self.writer.lock().await;
            w.write_all(&line).await?;
            w.flush().await
        }
        .await;
        if let Err(e) = written {
            self.pending.lock().await.remove(&id);
            return Err(e.into());
        }
        let resp = match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(v)) => v,
            Ok(Err(_)) => return Err(PiError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                return Err(PiError::Timeout { command, timeout: self.timeout });
            }
        };
        if resp.get("success").and_then(Value::as_bool) != Some(true) {
            let error = resp.get("error").and_then(Value::as_str).unwrap_or("no error message").to_string();
            return Err(PiError::Rejected { command, error });
        }
        Ok(resp.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Prompt the agent. `streaming` must be set when the agent may be busy.
    pub async fn prompt(&self, message: &str, images: &[Value], streaming: Option<Streaming>) -> Result<Disposition> {
        let mut cmd = json!({ "type": "prompt", "message": message });
        if !images.is_empty() {
            cmd["images"] = json!(images);
        }
        if let Some(s) = streaming {
            cmd["streamingBehavior"] = json!(s);
        }
        let data = self.command(cmd).await?;
        match data.get("disposition").and_then(Value::as_str) {
            Some("started") => Ok(Disposition::Started),
            Some("queued") => Ok(Disposition::Queued),
            Some("handled") => Ok(Disposition::Handled),
            other => Err(PiError::Protocol { command: "prompt".into(), detail: format!("disposition {other:?}") }),
        }
    }

    pub async fn abort(&self) -> Result<()> {
        self.command(json!({ "type": "abort" })).await.map(|_| ())
    }

    pub async fn state(&self) -> Result<Value> {
        self.command(json!({ "type": "get_state" })).await
    }

    /// Whether pi is currently running (streaming or compacting).
    pub async fn is_busy(&self) -> Result<bool> {
        let s = self.state().await?;
        let flag = |k: &str| s.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(flag("isStreaming") || flag("isCompacting"))
    }
}

async fn read_loop(
    stdout: impl AsyncRead + Unpin,
    pending: Pending,
    closed: Arc<AtomicBool>,
    events: mpsc::Sender<PiEvent>,
) {
    let mut lines = BufReader::new(stdout).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "reading pi output failed");
                break;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, line = %line.chars().take(200).collect::<String>(), "non-JSON line from pi");
                continue;
            }
        };
        if value.get("type").and_then(Value::as_str) == Some("response")
            && let Some(id) = value.get("id").and_then(Value::as_str).map(String::from)
            && let Some(tx) = pending.lock().await.remove(&id)
        {
            if tx.send(value).is_err() {
                tracing::debug!(id, "response arrived after the caller gave up");
            }
            continue;
        }
        if events.send(PiEvent::parse(value)).await.is_err() {
            // Nobody listens anymore; keep draining so responses still resolve.
            tracing::debug!("pi event receiver dropped");
        }
    }
    // Wake every waiter: their responses will never come.
    closed.store(true, Ordering::SeqCst);
    pending.lock().await.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted fake pi: answers commands from a closure, emits extra lines.
    fn fake(respond: impl Fn(&Value) -> Vec<Value> + Send + 'static) -> (PiClient, mpsc::Receiver<PiEvent>) {
        let (client_w, mut fake_r) = tokio::io::duplex(1 << 16);
        let (mut fake_w, client_r) = tokio::io::duplex(1 << 16);
        tokio::spawn(async move {
            let mut lines = BufReader::new(&mut fake_r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let cmd: Value = serde_json::from_str(&l).unwrap();
                for out in respond(&cmd) {
                    fake_w.write_all(format!("{out}\n").as_bytes()).await.unwrap();
                }
            }
        });
        PiClient::connect(client_w, client_r, Duration::from_secs(2))
    }

    #[tokio::test]
    async fn prompt_started_and_events_flow() {
        let (pi, mut ev) = fake(|cmd| {
            vec![
                json!({"type": "response", "id": cmd["id"], "command": "prompt", "success": true, "data": {"disposition": "started"}}),
                json!({"type": "agent_start"}),
                json!({"type": "message_end", "message": {"role": "assistant", "model": "m", "usage": {"totalTokens": 10, "cost": {"total": 0.5}}}}),
                json!({"type": "agent_settled"}),
            ]
        });
        assert_eq!(pi.prompt("hi", &[], None).await.unwrap(), Disposition::Started);
        assert_eq!(ev.recv().await.unwrap().kind, PiEventKind::AgentStart);
        match ev.recv().await.unwrap().kind {
            PiEventKind::MessageEnd { usage: Some(u), .. } => assert_eq!((u.total_tokens, u.cost_usd), (10, 0.5)),
            other => panic!("{other:?}"),
        }
        assert_eq!(ev.recv().await.unwrap().kind, PiEventKind::AgentSettled);
    }

    #[tokio::test]
    async fn rejection_fails_fast() {
        let (pi, _ev) = fake(|cmd| {
            vec![json!({"type": "response", "id": cmd["id"], "command": "prompt", "success": false, "error": "EROFS"})]
        });
        match pi.prompt("hi", &[], None).await {
            Err(PiError::Rejected { error, .. }) => assert_eq!(error, "EROFS"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn closed_process_fails_waiters() {
        let (client_w, _keep) = tokio::io::duplex(64);
        let (fake_w, client_r) = tokio::io::duplex(64);
        drop(fake_w);
        let (pi, _ev) = PiClient::connect(client_w, client_r, Duration::from_secs(2));
        assert!(matches!(pi.state().await, Err(PiError::Closed) | Err(PiError::Io(_))));
    }

    #[tokio::test]
    async fn silent_pi_times_out() {
        let (pi, _ev) = fake(|_| vec![]);
        let pi = PiClient { timeout: Duration::from_millis(50), ..pi };
        assert!(matches!(pi.abort().await, Err(PiError::Timeout { .. })));
    }
}
