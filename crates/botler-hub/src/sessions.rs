//! Reading pi session files for viewers: a flat list of the entries on the active branch,
//! with the delivery/event ids from botler's message headers so events link to messages.

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::hub::{Hub, HubError, Result};

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(rename = "SessionItem")]
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
        /// For a message the user typed: exactly their text (`text` is what the agent received,
        /// with the catalog's guidance around it).
        message_text: Option<String>,
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
        /// The tool call this answers.
        tool_call_id: Option<String>,
        text: String,
        is_error: bool,
        /// The dialog an `ask_user` / `request_secrets` call opened (shown in place).
        dialog_id: Option<String>,
        /// The proposal a `propose_change` call started (its approval is shown in place).
        proposal_id: Option<String>,
    },
    Compaction {
        id: String,
        at: Option<String>,
        summary: String,
    },
}

#[derive(Debug, Serialize, ts_rs::TS)]
pub struct ToolCall {
    pub id: Option<String>,
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

/// The dialog and proposal ids a botler tool result reports (`ask_user` → `dialog_id`,
/// `request_secrets` → `request_id`, `propose_change` → `proposal_id`). MCP tools may carry a
/// client prefix, hence the suffix match. Results that are not JSON (errors) have no links.
fn result_links(tool: &str, text: &str) -> (Option<String>, Option<String>) {
    if !["ask_user", "request_secrets", "propose_change"].iter().any(|t| tool.ends_with(t)) {
        return (None, None);
    }
    let Ok(v) = serde_json::from_str::<Value>(text.trim()) else { return (None, None) };
    let field = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    (field("dialog_id").or_else(|| field("request_id")), field("proposal_id"))
}

/// The text of the user's chat message `event_id`, or `None` for any other event. Events are
/// never deleted, but a session can outlive a database it was copied from: a missing event is
/// shown as the delivered text.
async fn user_text(hub: &Hub, event_id: &str) -> Result<Option<String>> {
    let ev = match hub.store.event(event_id).await {
        Ok(ev) => ev,
        Err(botler_store::Error::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if ev.kind != botler_core::event::kinds::MESSAGE || ev.source != botler_core::Source::User {
        return Ok(None);
    }
    Ok(ev.payload.get("text").and_then(Value::as_str).map(String::from))
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
                                message_text: user_text(hub, &event).await?,
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
                                message_text: None,
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
                                        id: p.get("id").and_then(Value::as_str).map(String::from),
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
                    Some("toolResult") => {
                        let tool: String = m.get("toolName").and_then(Value::as_str).unwrap_or("").into();
                        let text = texts(&content, "text", "text");
                        let (dialog_id, proposal_id) = result_links(&tool, &text);
                        out.push(Item::ToolResult {
                            id,
                            at,
                            tool_call_id: m.get("toolCallId").and_then(Value::as_str).map(String::from),
                            tool,
                            text: clip(text),
                            is_error: m.get("isError").and_then(Value::as_bool).unwrap_or(false),
                            dialog_id,
                            proposal_id,
                        });
                    }
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

    #[test]
    fn tool_result_links() {
        assert_eq!(
            result_links("botler_ask_user", r#"{"dialog_id": "d1", "note": "later"}"#),
            (Some("d1".into()), None)
        );
        assert_eq!(result_links("request_secrets", r#"{"request_id": "d2"}"#), (Some("d2".into()), None));
        assert_eq!(result_links("propose_change", r#"{"proposal_id": "p1"}"#), (None, Some("p1".into())));
        assert_eq!(result_links("bash", r#"{"dialog_id": "d1"}"#), (None, None));
        assert_eq!(result_links("ask_user", "error: rate limited"), (None, None));
    }
}
