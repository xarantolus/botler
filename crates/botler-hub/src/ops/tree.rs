//! A chat's session as a tree (pi entries link to their parent): looking at it, continuing from
//! an earlier entry (`/botler-goto`), labelling entries (`/botler-label`), and forking a new chat
//! from an assistant entry. pi navigates only while idle, so the hub sends the commands only then.

use std::collections::HashMap;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::{Hub, HubError};
use crate::sessions::{Item, parse_entry};

/// Characters of an entry's text shown in the tree.
const PREVIEW: usize = 200;

#[derive(Debug, Serialize, TS)]
pub struct SessionTree {
    pub chat: String,
    pub file: String,
    /// The entry the conversation continues from (the last one written).
    pub leaf: Option<String>,
    /// Every entry, in file order.
    pub nodes: Vec<TreeNode>,
}

#[derive(Debug, Serialize, TS)]
pub struct TreeNode {
    pub id: String,
    pub parent: Option<String>,
    /// `user`, `assistant`, `tool_result`, `note`, `compaction`, `label`, `botler.nav`, ...
    pub kind: String,
    /// The start of its text.
    pub text: String,
    /// The label set on it, if any.
    pub label: Option<String>,
    pub at: Option<String>,
    /// On the path from the root to the leaf: what the agent currently sees.
    pub on_branch: bool,
}

/// The lines of `chat`'s current session file, parsed (lines that are not JSON are skipped, as pi
/// skips a torn last line), and the file's name.
async fn entries(hub: &Hub, chat: &str) -> Result<(String, Vec<Value>), OpError> {
    let inst = hub.store.instance(chat).await?;
    let path = hub.paths.sessions(&inst.name).join(&inst.session_file);
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(HubError::Io(e).into()),
    };
    Ok((inst.session_file, text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()))
}

fn str_of<'a>(e: &'a Value, key: &str) -> Option<&'a str> {
    e.get(key).and_then(Value::as_str)
}

/// `chat`'s session tree.
pub async fn tree(hub: &Hub, chat: &str) -> Result<SessionTree, OpError> {
    let (file, entries) = entries(hub, chat).await?;
    let mut labels: HashMap<String, Option<String>> = HashMap::new();
    for e in &entries {
        if str_of(e, "type") == Some("label")
            && let Some(target) = str_of(e, "targetId")
        {
            labels.insert(target.into(), str_of(e, "label").map(String::from));
        }
    }
    // The header (`type = "session"`) carries the session's id, not an entry's.
    let with_id: Vec<&Value> =
        entries.iter().filter(|e| str_of(e, "id").is_some() && str_of(e, "type") != Some("session")).collect();
    let leaf = with_id.last().and_then(|e| str_of(e, "id")).map(String::from);
    let parents: HashMap<&str, &str> =
        with_id.iter().filter_map(|e| Some((str_of(e, "id")?, str_of(e, "parentId")?))).collect();
    let mut branch = std::collections::HashSet::new();
    let mut cur = leaf.as_deref();
    while let Some(id) = cur {
        branch.insert(id.to_string());
        cur = parents.get(id).copied();
    }
    let nodes = with_id
        .into_iter()
        .map(|e| {
            let id = str_of(e, "id").unwrap_or_default().to_string();
            let (kind, text) = match parse_entry(e) {
                Some(Item::User { text, .. }) => ("user".to_string(), text),
                Some(Item::Assistant { text, .. }) => ("assistant".into(), text),
                Some(Item::ToolResult { tool, .. }) => ("tool_result".into(), tool),
                Some(Item::Note { text, .. }) => ("note".into(), text),
                Some(Item::Compaction { summary, .. }) => ("compaction".into(), summary),
                Some(Item::SessionStarted { summary, .. }) => ("handover".into(), summary),
                None => {
                    let kind = str_of(e, "customType").or(str_of(e, "type")).unwrap_or("entry");
                    (kind.to_string(), str_of(e, "label").unwrap_or_default().to_string())
                }
            };
            TreeNode {
                parent: str_of(e, "parentId").map(String::from),
                kind,
                text: text.chars().take(PREVIEW).collect(),
                label: labels.get(&id).cloned().flatten(),
                at: str_of(e, "timestamp").map(String::from),
                on_branch: branch.contains(&id),
                id,
            }
        })
        .collect();
    Ok(SessionTree { chat: chat.into(), file, leaf, nodes })
}

async fn chat_name(hub: &Hub, chat: &str) -> Result<String, OpError> {
    Ok(hub
        .store
        .resolve_instance(chat)
        .await?
        .ok_or_else(|| OpError::BadRequest(format!("no chat named {chat}")))?
        .name)
}

/// Send `/command args` to `chat`'s pi, waking it if needed; refused while it is working.
async fn command(hub: &Arc<Hub>, chat: &str, command: &str) -> Result<(), OpError> {
    let inst = hub.store.instance(chat).await?;
    let agent = crate::agents::ensure_awake(hub, &inst).await?;
    if agent.busy() || agent.pi.is_busy().await.map_err(HubError::from)? {
        return Err(OpError::BadRequest(format!("{chat} is working; try again when its turn is over")));
    }
    agent.pi.prompt(command, &[], None).await.map_err(HubError::from)?;
    Ok(())
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GetSessionTree {
    pub chat: String,
}

impl Op for GetSessionTree {
    const NAME: &'static str = "session_tree";
    const DESCRIPTION: &'static str =
        "A chat's current session as a tree of entries (ids, parents, labels, the active branch).";
    const WHO: Who = Who::User;
    type Output = SessionTree;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<SessionTree, OpError> {
        tree(hub, &chat_name(hub, &self.chat).await?).await
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct GotoEntry {
    pub chat: String,
    /// The entry to continue from: later entries stay in the tree on another branch.
    pub entry: String,
}

#[derive(Debug, Serialize, TS)]
pub struct EntryDone {
    pub chat: String,
    pub entry: String,
}

impl Op for GotoEntry {
    const NAME: &'static str = "goto_entry";
    const DESCRIPTION: &'static str = "Rewind a chat (or switch branch): it continues from this entry of its session tree. Only while it is not working.";
    const WHO: Who = Who::User;
    type Output = EntryDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<EntryDone, OpError> {
        let chat = chat_name(hub, &self.chat).await?;
        node(hub, &chat, &self.entry).await?;
        command(hub, &chat, &format!("/botler-goto {}", self.entry)).await?;
        Ok(EntryDone { chat, entry: self.entry })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct LabelEntry {
    pub chat: String,
    pub entry: String,
    /// The label; none removes it.
    #[serde(default)]
    pub label: Option<String>,
}

impl Op for LabelEntry {
    const NAME: &'static str = "label_entry";
    const DESCRIPTION: &'static str =
        "Label an entry of a chat's session tree (none: remove the label). Only while it is not working.";
    const WHO: Who = Who::User;
    type Output = EntryDone;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<EntryDone, OpError> {
        let chat = chat_name(hub, &self.chat).await?;
        node(hub, &chat, &self.entry).await?;
        let label = self.label.as_deref().map(str::trim).unwrap_or_default();
        command(hub, &chat, format!("/botler-label {} {label}", self.entry).trim_end()).await?;
        Ok(EntryDone { chat, entry: self.entry })
    }
}

/// The node `entry` of `chat`'s tree.
async fn node(hub: &Hub, chat: &str, entry: &str) -> Result<TreeNode, OpError> {
    tree(hub, chat)
        .await?
        .nodes
        .into_iter()
        .find(|n| n.id == entry)
        .ok_or_else(|| OpError::BadRequest(format!("{chat}'s session has no entry {entry}")))
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ForkChat {
    pub chat: String,
    /// An assistant entry: the new chat holds the conversation up to and including it.
    pub entry: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatForked {
    /// The new chat.
    pub instance: String,
    pub from: String,
    pub entry: String,
}

impl Op for ForkChat {
    const NAME: &'static str = "fork_chat";
    const DESCRIPTION: &'static str =
        "Start a new chat of the same agent definition from a chat's conversation up to one of its assistant entries.";
    const WHO: Who = Who::User;
    type Output = ChatForked;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<ChatForked, OpError> {
        let from = chat_name(hub, &self.chat).await?;
        if node(hub, &from, &self.entry).await?.kind != "assistant" {
            return Err(OpError::BadRequest(
                "fork at an assistant entry, so the new chat starts on a complete turn".into(),
            ));
        }
        let (_, entries) = entries(hub, &from).await?;
        // The header, then the entry and its ancestors in file order.
        let parents: HashMap<&str, &str> =
            entries.iter().filter_map(|e| Some((str_of(e, "id")?, str_of(e, "parentId")?))).collect();
        let mut keep = std::collections::HashSet::new();
        let mut cur = Some(self.entry.as_str());
        while let Some(id) = cur {
            keep.insert(id);
            cur = parents.get(id).copied();
        }
        let mut text = String::new();
        for e in &entries {
            let line = if str_of(e, "type") == Some("session") {
                // A session of its own.
                let mut header = e.clone();
                if header.get("id").is_some() {
                    header["id"] = Value::String(uuid::Uuid::now_v7().to_string());
                }
                header
            } else if str_of(e, "id").is_some_and(|id| keep.contains(id)) {
                e.clone()
            } else {
                continue;
            };
            text.push_str(&line.to_string());
            text.push('\n');
        }
        let def = hub.store.instance(&from).await?.definition;
        let instance = crate::router::new_instance(hub, &def, None, None).await?;
        let inst = hub.store.instance(&instance).await?;
        let dir = hub.paths.sessions(&instance);
        tokio::fs::create_dir_all(&dir).await.map_err(HubError::Io)?;
        tokio::fs::write(dir.join(&inst.session_file), text).await.map_err(HubError::Io)?;
        tracing::info!(%from, entry = %self.entry, %instance, "forked a chat");
        Ok(ChatForked { instance, from, entry: self.entry })
    }
}
