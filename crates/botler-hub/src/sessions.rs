//! Reading pi session files for viewers: a flat list of the entries on the active branch,
//! with the delivery/event ids from botler's message headers so events link to messages.

use botler_core::payloads::BuiltinPayload;
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
        /// For a message (from the user or an agent): the files attached to it.
        files: Vec<crate::uploads::Uploaded>,
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
        /// pi's timestamp of the message (ms): the live stream's `message_start` carries the same,
        /// so the web app hands its streaming copy over to this entry.
        message_ts: Option<i64>,
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
        /// Images the tool returned (a screenshot, a picture it read), without their bytes.
        images: Vec<ImageRef>,
        /// Files the agent attached to its reply while this call ran (`attach_files`, also from
        /// inside a script): the chat shows them with the reply.
        files: Vec<crate::uploads::Uploaded>,
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
    /// The chat continues here from a handover: the report it wrote in the previous session.
    SessionStarted {
        id: String,
        at: Option<String>,
        /// The previous session file (`?file=` reads it).
        previous: String,
        summary: String,
        outstanding: Vec<String>,
        state: Vec<String>,
    },
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::User { id, .. }
            | Item::Assistant { id, .. }
            | Item::ToolResult { id, .. }
            | Item::Note { id, .. }
            | Item::Compaction { id, .. }
            | Item::SessionStarted { id, .. } => id,
        }
    }
}

/// An image block of a session entry, served by
/// `GET /api/v1/sessions/<instance>/images/<entry>/<index>?file=<session file>`: the view never
/// carries the bytes, so a refresh stays small.
#[derive(Debug, PartialEq, Serialize, ts_rs::TS)]
pub struct ImageRef {
    /// Its place in the entry's content.
    pub index: usize,
    pub mime: String,
}

/// The image blocks of `content` that the web app shows (raster types only).
fn image_refs(content: &Value) -> Vec<ImageRef> {
    let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
    blocks
        .iter()
        .enumerate()
        .filter(|(_, p)| p.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|(index, p)| {
            let mime = crate::uploads::raster_image(p.get("mimeType").and_then(Value::as_str)?)?;
            Some(ImageRef { index, mime: mime.into() })
        })
        .collect()
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

/// For message `event_id`: the text the user typed (`None` for a message from anyone else) and
/// the files attached to it; `None` for any other event. Events are never deleted, but a session
/// can outlive a database it was copied from: a missing event is shown as the delivered text.
async fn message_of(hub: &Hub, event_id: &str) -> Result<Option<(Option<String>, Vec<crate::uploads::Uploaded>)>> {
    let ev = match hub.store.event(event_id).await {
        Ok(ev) => ev,
        Err(botler_store::Error::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if ev.kind != botler_core::event::kinds::MESSAGE {
        return Ok(None);
    }
    let text = match ev.source {
        botler_core::Source::User => ev.payload.get("text").and_then(Value::as_str).map(String::from),
        _ => None,
    };
    let paths = ev.payload.get("files").and_then(Value::as_array).into_iter().flatten();
    let prefix = format!("{}/", crate::uploads::INBOX);
    let mut files = Vec::new();
    for file in paths.filter_map(|f| f.as_str()?.strip_prefix(&prefix)) {
        match crate::uploads::info(hub, file).await? {
            Some(f) => files.push(f),
            None => tracing::warn!(event = event_id, file, "a message's attachment is gone from the uploads"),
        }
    }
    Ok(Some((text, files)))
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
                            files: Vec::new(),
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
                            files: Vec::new(),
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
                        message_ts: m.get("timestamp").and_then(Value::as_i64),
                    })
                }
                Some("toolResult") => {
                    let tool: String = m.get("toolName").and_then(Value::as_str).unwrap_or("").into();
                    let text = texts(&content, "text", "text");
                    let (dialog_id, proposal_id) = result_links(&tool, &text);
                    let images = image_refs(&content);
                    Some(Item::ToolResult {
                        id,
                        at,
                        tool_call_id: m.get("toolCallId").and_then(Value::as_str).map(String::from),
                        tool,
                        text: clip(text),
                        is_error: m.get("isError").and_then(Value::as_bool).unwrap_or(false),
                        dialog_id,
                        proposal_id,
                        images,
                        files: Vec::new(),
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

/// An instance's session files, oldest first; the current one last (also before it exists).
pub async fn files(hub: &Hub, instance: &str) -> Result<Vec<String>> {
    let inst = hub.store.instance(instance).await?;
    let mut found = Vec::new();
    let mut rd = match tokio::fs::read_dir(hub.paths.sessions(&inst.name)).await {
        Ok(rd) => Some(rd),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(HubError::Io(e)),
    };
    while let Some(entry) = match rd.as_mut() {
        Some(rd) => rd.next_entry().await?,
        None => None,
    } {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") && name != inst.session_file {
            found.push((entry.metadata().await?.modified()?, name));
        }
    }
    found.sort();
    let mut out: Vec<String> = found.into_iter().map(|(_, n)| n).collect();
    out.push(inst.session_file);
    Ok(out)
}

/// The path of one of an instance's session files (default: the current one).
async fn session_path(hub: &Hub, instance: &str, file: Option<&str>) -> Result<std::path::PathBuf> {
    let inst = hub.store.instance(instance).await?;
    let file = match file {
        None => inst.session_file.clone(),
        Some(f) if files(hub, instance).await?.iter().any(|x| x == f) => f.to_string(),
        Some(f) => return Err(HubError::Undefined { kind: "session file", name: f.into() }),
    };
    Ok(hub.paths.sessions(&inst.name).join(&file))
}

/// The bytes and type of image block `index` of session entry `entry` ([`ImageRef`]).
pub async fn image(
    hub: &Hub,
    instance: &str,
    file: Option<&str>,
    entry: &str,
    index: usize,
) -> Result<(Vec<u8>, &'static str)> {
    use base64::Engine;
    use tokio::io::AsyncBufReadExt;
    let undefined = || HubError::Undefined { kind: "session image", name: format!("{entry}/{index}") };
    let path = session_path(hub, instance, file).await?;
    let mut lines = match tokio::fs::File::open(&path).await {
        Ok(f) => tokio::io::BufReader::new(f).lines(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(undefined()),
        Err(e) => return Err(HubError::Io(e)),
    };
    while let Some(line) = lines.next_line().await? {
        // Only the entry's own line is parsed.
        if !line.contains(entry) {
            continue;
        }
        let Ok(e) = serde_json::from_str::<Value>(&line) else { continue };
        if e.get("id").and_then(Value::as_str) != Some(entry) {
            continue;
        }
        let block = e.pointer(&format!("/message/content/{index}")).ok_or_else(undefined)?;
        let mime = block.get("mimeType").and_then(Value::as_str).and_then(crate::uploads::raster_image);
        let data = block.get("data").and_then(Value::as_str);
        let (Some(mime), Some(data), Some("image")) = (mime, data, block.get("type").and_then(Value::as_str)) else {
            return Err(undefined());
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| HubError::Io(std::io::Error::other(format!("image {entry}/{index} is not base64: {e}"))))?;
        return Ok((bytes, mime));
    }
    Err(undefined())
}

/// The active branch of one of an instance's sessions (default: the current one), oldest first.
/// The session's items, at most the `last` ones (all without), and how many come before them.
pub async fn read(hub: &Hub, instance: &str, file: Option<&str>, last: Option<usize>) -> Result<(Vec<Item>, usize)> {
    let page = read_page(hub, instance, file, Window { last, ..Window::default() }).await?;
    Ok((page.items, page.earlier))
}

/// Part of a session's active branch.
pub struct Page {
    pub items: Vec<Item>,
    /// How many items come before `items`.
    pub earlier: usize,
    /// The item asked for (`after` or `before`) is on the branch: `items` adjoin it.
    pub continues: bool,
    /// The deliveries anywhere on the branch.
    pub delivered: std::collections::HashSet<String>,
}

/// Which items of the active branch to read.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window<'a> {
    /// At most this many (all without): the latest, or those right before `before`.
    pub last: Option<usize>,
    /// All items after this one (a live chat's update carries only what is new).
    pub after: Option<&'a str>,
    /// Items before this one (scrolling back).
    pub before: Option<&'a str>,
}

/// The items of `window`; one whose `after` / `before` item is not on the branch (it moved:
/// a compaction, a goto) gets the latest `last` items with `continues` false.
pub async fn read_page(hub: &Hub, instance: &str, file: Option<&str>, window: Window<'_>) -> Result<Page> {
    let path = session_path(hub, instance, file).await?;
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Page { items: Vec::new(), earlier: 0, continues: false, delivered: Default::default() });
        }
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

    let items: Vec<Item> = branch.into_iter().filter_map(parse_entry).collect();
    let delivered = items
        .iter()
        .filter_map(|it| match it {
            Item::User { delivery: Some(d), .. } => Some(d.clone()),
            _ => None,
        })
        .collect();
    let at = |id: Option<&str>| id.and_then(|a| items.iter().position(|it| it.id() == a));
    let keep = |end: usize| window.last.map_or(0, |n| end.saturating_sub(n));
    let (earlier, end, continues) = match (at(window.after), at(window.before)) {
        (Some(i), _) => (i + 1, items.len(), true),
        (None, Some(j)) => (keep(j), j, true),
        (None, None) => (keep(items.len()), items.len(), false),
    };
    // An attachment belongs to the first tool result written after it (the call it was made in);
    // one from before an item that is no result (another session, an earlier turn) to none.
    let attached = hub.store.attachments(instance).await?;
    let mut attached = attached.iter().peekable();
    let mut attached_to: Vec<Vec<&str>> = Vec::with_capacity(items.len());
    for item in &items {
        let mut files = Vec::new();
        let at = match item {
            Item::User { at, .. }
            | Item::Assistant { at, .. }
            | Item::ToolResult { at, .. }
            | Item::Note { at, .. }
            | Item::Compaction { at, .. }
            | Item::SessionStarted { at, .. } => at.as_deref(),
        };
        // An entry without a usable time (pi's own) leaves the attachments to the next one.
        if let Some(at) = at.and_then(|a| chrono::DateTime::parse_from_rfc3339(a).ok()) {
            while let Some((_, file)) = attached.next_if(|(t, _)| *t <= at) {
                if matches!(item, Item::ToolResult { .. }) {
                    files.push(file.as_str());
                }
            }
        }
        attached_to.push(files);
    }
    // Only the items shown are completed from the events (a query each).
    let mut out = Vec::new();
    for (mut item, names) in items.into_iter().zip(attached_to).take(end).skip(earlier) {
        if let Item::ToolResult { files, .. } = &mut item {
            for name in names {
                files.extend(crate::uploads::info(hub, name).await?);
            }
        }
        if let Item::User { event: Some(ev), message_text, files, .. } = &mut item
            && let Some((text, attached)) = message_of(hub, ev).await?
        {
            *message_text = text;
            *files = attached;
        }
        if let Item::Note { id, at, event_kind, event, .. } = &item
            && event_kind == botler_core::event::kinds::SESSION_STARTED
        {
            let ev = hub.store.event(event).await?;
            if let Some(Ok(BuiltinPayload::SessionStarted(p))) = BuiltinPayload::of(&ev) {
                item = Item::SessionStarted {
                    id: id.clone(),
                    at: at.clone(),
                    previous: p.previous,
                    summary: p.summary,
                    outstanding: p.outstanding,
                    state: p.state,
                };
            }
        }
        out.push(item);
    }
    Ok(Page { items: out, earlier, continues, delivered })
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
