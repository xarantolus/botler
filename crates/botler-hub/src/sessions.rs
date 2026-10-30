//! Reading pi session files for viewers: a flat list of the entries on the active branch,
//! with the delivery/event ids from botler's message headers so events link to messages.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::hub::{Hub, HubError, Result};

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    User {
        id: String,
        at: Option<String>,
        text: String,
        from: Option<String>,
        provenance: Option<String>,
        event_kind: Option<String>,
        delivery: Option<String>,
        event: Option<String>,
    },
    Assistant {
        id: String,
        at: Option<String>,
        text: String,
        thinking: String,
        tool_calls: Vec<ToolCall>,
        model: Option<String>,
        cost_usd: Option<f64>,
        error: Option<String>,
    },
    ToolResult {
        id: String,
        at: Option<String>,
        tool: String,
        text: String,
        is_error: bool,
    },
    Compaction {
        id: String,
        at: Option<String>,
        summary: String,
    },
}

#[derive(Debug, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

const MAX_TEXT: usize = 20_000;

fn clip(s: String) -> String {
    if s.len() <= MAX_TEXT {
        return s;
    }
    let mut end = MAX_TEXT;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes)", &s[..end], s.len() - end)
}

/// `[from X | provenance: Y | kind | delivery D | event E]` → parts.
fn parse_header(text: &str) -> Option<(String, String, String, String, String, String)> {
    let first = text.lines().next()?;
    let inner = first.strip_prefix('[')?.strip_suffix(']')?;
    let parts: Vec<&str> = inner.split(" | ").collect();
    let [from, prov, kind, delivery, event] = parts[..] else { return None };
    let rest = text.split_once('\n').map_or("", |(_, r)| r).to_string();
    Some((
        from.strip_prefix("from ")?.into(),
        prov.strip_prefix("provenance: ")?.into(),
        kind.into(),
        delivery.strip_prefix("delivery ")?.into(),
        event.strip_prefix("event ")?.into(),
        rest,
    ))
}

fn texts(content: &Value, kind: &str, field: &str) -> String {
    content
        .as_array()
        .map(|c| {
            c.iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some(kind))
                .filter_map(|p| p.get(field).and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Point an existing session's header at working directory `cwd`. pi resumes a session in the
/// directory its header records (and refuses to start if that directory is missing), so moving
/// agents to a new working directory means rewriting that one field. Returns whether the file
/// changed. Only called while the instance's container is not running.
pub async fn retarget_cwd(path: &std::path::Path, cwd: &str) -> Result<bool> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(HubError::Io(e)),
    };
    let mut reader = tokio::io::BufReader::new(file);
    let mut first = String::new();
    reader.read_line(&mut first).await?;
    let Ok(mut header) = serde_json::from_str::<Value>(first.trim_end()) else { return Ok(false) };
    if header.get("type").and_then(Value::as_str) != Some("session")
        || header.get("cwd").and_then(Value::as_str) == Some(cwd)
    {
        return Ok(false);
    }
    header["cwd"] = Value::String(cwd.into());
    let line = serde_json::to_string(&header).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    let tmp = path.with_extension("jsonl.retarget");
    let mut out = tokio::fs::File::create(&tmp).await?;
    out.write_all(line.as_bytes()).await?;
    out.write_all(b"\n").await?;
    tokio::io::copy(&mut reader, &mut out).await?;
    out.sync_all().await?;
    drop(out);
    tokio::fs::rename(&tmp, path).await?;
    Ok(true)
}

/// The active branch of an instance's session, oldest first.
pub async fn read(hub: &Hub, instance: &str) -> Result<Vec<Item>> {
    let inst = hub.store.instance(instance).await?;
    let path = hub.paths.sessions(&inst.name).join(&inst.session_file);
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(HubError::Io(e)),
    };
    let entries: Vec<Value> = raw.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    // Walk back from the last entry (the leaf) along parentId to get the active branch.
    let by_id: HashMap<&str, &Value> = entries.iter().filter_map(|e| Some((e.get("id")?.as_str()?, e))).collect();
    let mut branch = Vec::new();
    let mut cur = entries.iter().rev().find(|e| e.get("id").is_some());
    while let Some(e) = cur {
        branch.push(e);
        cur = e.get("parentId").and_then(Value::as_str).and_then(|p| by_id.get(p).copied());
    }
    branch.reverse();

    let mut out = Vec::new();
    for e in branch {
        let id = e.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        let at = e.get("timestamp").and_then(Value::as_str).map(String::from);
        match e.get("type").and_then(Value::as_str) {
            Some("message") => {
                let Some(m) = e.get("message") else { continue };
                let content = m.get("content").cloned().unwrap_or(Value::Null);
                match m.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = match &content {
                            Value::String(s) => s.clone(),
                            c => texts(c, "text", "text"),
                        };
                        let item = match parse_header(&text) {
                            Some((from, prov, kind, delivery, event, rest)) => Item::User {
                                id,
                                at,
                                text: clip(rest),
                                from: Some(from),
                                provenance: Some(prov),
                                event_kind: Some(kind),
                                delivery: Some(delivery),
                                event: Some(event),
                            },
                            None => Item::User {
                                id,
                                at,
                                text: clip(text),
                                from: None,
                                provenance: None,
                                event_kind: None,
                                delivery: None,
                                event: None,
                            },
                        };
                        out.push(item);
                    }
                    Some("assistant") => {
                        let tool_calls = content
                            .as_array()
                            .map(|c| {
                                c.iter()
                                    .filter(|p| p.get("type").and_then(Value::as_str) == Some("toolCall"))
                                    .map(|p| ToolCall {
                                        name: p.get("name").and_then(Value::as_str).unwrap_or("?").into(),
                                        arguments: p.get("arguments").cloned().unwrap_or(Value::Null),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        out.push(Item::Assistant {
                            id,
                            at,
                            text: clip(texts(&content, "text", "text")),
                            thinking: clip(texts(&content, "thinking", "thinking")),
                            tool_calls,
                            model: m.get("model").and_then(Value::as_str).map(String::from),
                            cost_usd: m.pointer("/usage/cost/total").and_then(Value::as_f64),
                            error: m.get("errorMessage").and_then(Value::as_str).map(String::from),
                        });
                    }
                    Some("toolResult") => out.push(Item::ToolResult {
                        id,
                        at,
                        tool: m.get("toolName").and_then(Value::as_str).unwrap_or("").into(),
                        text: clip(texts(&content, "text", "text")),
                        is_error: m.get("isError").and_then(Value::as_bool).unwrap_or(false),
                    }),
                    _ => {}
                }
            }
            Some("compaction") => out.push(Item::Compaction {
                id,
                at,
                summary: clip(e.get("summary").and_then(Value::as_str).unwrap_or("").into()),
            }),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retarget_rewrites_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.jsonl");
        let body = "{\"type\":\"message\",\"id\":\"a\",\"cwd\":\"/workspace\"}\n";
        std::fs::write(&p, format!("{{\"type\":\"session\",\"version\":3,\"cwd\":\"/workspace\"}}\n{body}")).unwrap();
        assert!(retarget_cwd(&p, "/home/agent").await.unwrap());
        let text = std::fs::read_to_string(&p).unwrap();
        let (head, rest) = text.split_once('\n').unwrap();
        assert_eq!(serde_json::from_str::<Value>(head).unwrap()["cwd"], "/home/agent");
        assert_eq!(rest, body, "entries after the header are untouched");
        assert!(!retarget_cwd(&p, "/home/agent").await.unwrap(), "idempotent");
        assert!(!retarget_cwd(&dir.path().join("missing.jsonl"), "/x").await.unwrap());
    }

    #[test]
    fn header_parsing() {
        let (from, prov, kind, d, e, rest) =
            parse_header("[from user | provenance: trusted | message | delivery d1 | event e1]\nhello\nworld").unwrap();
        assert_eq!(
            (from.as_str(), prov.as_str(), kind.as_str(), d.as_str(), e.as_str()),
            ("user", "trusted", "message", "d1", "e1")
        );
        assert_eq!(rest, "hello\nworld");
        assert!(parse_header("plain text").is_none());
    }
}
