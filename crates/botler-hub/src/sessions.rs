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
        /// When it was delivered, in the instance's local time (from the delivery header).
        local_time: Option<String>,
        /// After a long silence, e.g. `first message in 3 days`.
        gap: Option<String>,
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
    /// A note that was added without starting a turn (e.g. an acknowledgement from another
    /// agent); the agent reads it with whatever wakes it next.
    Note {
        id: String,
        at: Option<String>,
        text: String,
        from: String,
        provenance: String,
        event_kind: String,
        delivery: String,
        event: String,
        local_time: Option<String>,
        gap: Option<String>,
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

/// A delivery header the botler pi extension wrote, and the text after it.
#[derive(Debug, PartialEq)]
struct Header {
    local_time: Option<String>,
    gap: Option<String>,
    from: String,
    provenance: String,
    kind: String,
    delivery: String,
    event: String,
    rest: String,
}

/// `[<local time> (<gap>) | from X | provenance: Y | kind | delivery D | event E]`; the time
/// (and the gap note within it) is absent in headers written before local time existed.
fn parse_header(text: &str) -> Option<Header> {
    let first = text.lines().next()?;
    let inner = first.strip_prefix('[')?.strip_suffix(']')?;
    let parts: Vec<&str> = inner.split(" | ").collect();
    let (when, [from, prov, kind, delivery, event]) = (match parts[..] {
        [from, prov, kind, delivery, event] => Some((None, [from, prov, kind, delivery, event])),
        [when, from, prov, kind, delivery, event] => Some((Some(when), [from, prov, kind, delivery, event])),
        _ => None,
    })?;
    let (local_time, gap) = match when {
        None => (None, None),
        Some(w) => match w.strip_suffix(')').and_then(|w| w.split_once(" (")) {
            Some((time, gap)) => (Some(time.to_string()), Some(gap.to_string())),
            None => (Some(w.to_string()), None),
        },
    };
    Some(Header {
        local_time,
        gap,
        from: from.strip_prefix("from ")?.into(),
        provenance: prov.strip_prefix("provenance: ")?.into(),
        kind: kind.into(),
        delivery: delivery.strip_prefix("delivery ")?.into(),
        event: event.strip_prefix("event ")?.into(),
        rest: text.split_once('\n').map_or("", |(_, r)| r).to_string(),
    })
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
    let mut header = match serde_json::from_str::<Value>(first.trim_end()) {
        Ok(h) => h,
        Err(e) => {
            // Not a session pi wrote; pi reports it on start, with more context than we have.
            tracing::warn!(path = %path.display(), error = %e, "session file does not start with a JSON header");
            return Ok(false);
        }
    };
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

/// One session entry as viewers see it, or `None` for entries that are not shown (model changes,
/// labels, ...). `message_text` is left empty: [`read`] fills it from the journal.
pub fn parse_entry(e: &Value) -> Option<Item> {
    let id = e.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let at = e.get("timestamp").and_then(Value::as_str).map(String::from);
    match e.get("type").and_then(Value::as_str) {
        Some("message") => {
            let m = e.get("message")?;
            let content = m.get("content").cloned().unwrap_or(Value::Null);
            match m.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let text = match &content {
                        Value::String(s) => s.clone(),
                        c => texts(c, "text", "text"),
                    };
                    let item = match parse_header(&text) {
                        Some(h) => Item::User {
                            id,
                            at,
                            text: clip(h.rest),
                            from: Some(h.from),
                            provenance: Some(h.provenance),
                            event_kind: Some(h.kind),
                            message_text: None,
                            delivery: Some(h.delivery),
                            event: Some(h.event),
                            local_time: h.local_time,
                            gap: h.gap,
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
                            local_time: None,
                            gap: None,
                            message_text: None,
                        },
                    };
                    Some(item)
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
                    Some(Item::Assistant {
                        id,
                        at,
                        text: clip(texts(&content, "text", "text")),
                        thinking: clip(texts(&content, "thinking", "thinking")),
                        tool_calls,
                        model: m.get("model").and_then(Value::as_str).map(String::from),
                        cost_usd: m.pointer("/usage/cost/total").and_then(Value::as_f64),
                        error: m.get("errorMessage").and_then(Value::as_str).map(String::from),
                    })
                }
                Some("toolResult") => {
                    let tool: String = m.get("toolName").and_then(Value::as_str).unwrap_or("").into();
                    let text = texts(&content, "text", "text");
                    let (dialog_id, proposal_id) = result_links(&tool, &text);
                    Some(Item::ToolResult {
                        id,
                        at,
                        tool_call_id: m.get("toolCallId").and_then(Value::as_str).map(String::from),
                        tool,
                        text: clip(text),
                        is_error: m.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        dialog_id,
                        proposal_id,
                    })
                }
                _ => None,
            }
        }
        // Notes the botler extension added without a turn (acknowledgements between agents).
        Some("custom_message") if e.get("customType").and_then(Value::as_str) == Some("botler.note") => {
            let content = e.get("content").cloned().unwrap_or(Value::Null);
            let text = match &content {
                Value::String(s) => s.clone(),
                c => texts(c, "text", "text"),
            };
            parse_header(&text).map(|h| Item::Note {
                id,
                at,
                text: clip(h.rest),
                from: h.from,
                provenance: h.provenance,
                event_kind: h.kind,
                delivery: h.delivery,
                event: h.event,
                local_time: h.local_time,
                gap: h.gap,
            })
        }
        Some("compaction") => Some(Item::Compaction {
            id,
            at,
            summary: clip(e.get("summary").and_then(Value::as_str).unwrap_or("").into()),
        }),
        _ => None,
    }
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
        let Some(mut item) = parse_entry(e) else { continue };
        if let Item::User { event: Some(ev), message_text, .. } = &mut item {
            *message_text = user_text(hub, ev).await?;
        }
        out.push(item);
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
        let h =
            parse_header("[from user | provenance: trusted | message | delivery d1 | event e1]\nhello\nworld").unwrap();
        assert_eq!(
            (h.from.as_str(), h.provenance.as_str(), h.kind.as_str(), h.delivery.as_str(), h.event.as_str()),
            ("user", "trusted", "message", "d1", "e1")
        );
        assert_eq!((h.rest.as_str(), h.local_time, h.gap), ("hello\nworld", None, None));
        assert!(parse_header("plain text").is_none());

        // With the local time, and a gap note after a long silence.
        let h = parse_header(
            "[Fri 2026-10-30 17:05 Europe/Berlin (first message in 3 days) | from user | provenance: trusted | message | delivery d | event e]\nhi",
        )
        .unwrap();
        assert_eq!(h.local_time.as_deref(), Some("Fri 2026-10-30 17:05 Europe/Berlin"));
        assert_eq!(h.gap.as_deref(), Some("first message in 3 days"));
        assert_eq!((h.from.as_str(), h.rest.as_str()), ("user", "hi"));
        let h =
            parse_header("[Fri 2026-10-30 17:05 UTC | from hub | provenance: trusted | timer | delivery d | event e]")
                .unwrap();
        assert_eq!((h.local_time.as_deref(), h.gap), (Some("Fri 2026-10-30 17:05 UTC"), None));
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
