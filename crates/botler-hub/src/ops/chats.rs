//! Chats (agent instances): naming, spawning, listing, reading and archiving them.

use std::sync::Arc;

use botler_core::defs::{AgentKind, Capability};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{Caller, Op, OpError, SendMessage, Who};
use crate::hub::{Hub, HubError};
use crate::router;

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct SetChatTitle {
    pub title: String,
    /// Lowercase words joined by dashes, 3 to 60 characters.
    pub slug: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatTitled {
    pub instance: String,
    pub title: String,
    pub slug: String,
}

impl Op for SetChatTitle {
    const NAME: &'static str = "set_chat_title";
    const DESCRIPTION: &'static str = "Name your chat so the user can find it: a short title and a unique slug like `groundstation-antenna-fix` (lowercase words joined by dashes, project or topic first). Others can then message you by the slug. Do this after the first message of a new chat, and again if the topic changes.";
    const WHO: Who = Who::UserAndAgents;
    type Output = ChatTitled;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<ChatTitled, OpError> {
        let instance = caller.agent("only agents name chats")?;
        let slug_ok = (3..=60).contains(&self.slug.len())
            && botler_core::defs::valid_name(&self.slug)
            && !self.slug.ends_with('-')
            && !self.slug.contains("--");
        if !slug_ok {
            return Err(OpError::BadRequest(
                "slug must be 3-60 lowercase letters/digits in dash-separated words".into(),
            ));
        }
        let title = self.title.trim();
        if title.is_empty() || title.len() > 120 {
            return Err(OpError::BadRequest("title must be 1-120 characters".into()));
        }
        hub.store.set_instance_title(instance, title, &self.slug).await.map_err(|e| match e {
            botler_store::Error::SlugTaken(s) => OpError::BadRequest(format!("{s} is taken; pick another slug")),
            other => other.into(),
        })?;
        Ok(ChatTitled { instance: instance.into(), title: title.into(), slug: self.slug })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct SpawnAgent {
    /// Agent definition under `agents/`, e.g. `dev`.
    pub definition: String,
    pub task: String,
    /// What the new chat gets beyond its definition.
    #[serde(default)]
    pub env: Option<SpawnEnv>,
}

#[derive(Debug, Default, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct SpawnEnv {
    /// Secret groups (memory notes say which fits the work, e.g. `github-ssh`).
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub secrets: Vec<String>,
    /// Network shares, mounted read-only.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub mounts: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct AgentSpawned {
    pub instance: String,
    /// Resources it holds from the start (`secret:<group>`, `mount:<name>`).
    pub granted: Vec<String>,
    /// Access requests the user decides; the new chat is woken with each answer.
    pub requested: Vec<String>,
}

impl Op for SpawnAgent {
    const NAME: &'static str = "spawn_agent";
    const DESCRIPTION: &'static str = "Start a new instance of an agent definition with a task. It reports back to you by message; if it has not by `reply_within` (botler.toml, default 20m), you get a `reply.overdue` event. `env = {secrets: [...], mounts: [...]}` gives it secret groups and shares beyond its definition: what you hold yourself or policy allows is granted at once, the rest is asked of the user. Give the child the access its task needs; never tell it to work around missing access.";
    const WHO: Who = Who::UserAndAgents;
    type Output = AgentSpawned;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<AgentSpawned, OpError> {
        use botler_core::defs::{GrantMode, Resource, ResourceKind};
        let env = self.env.unwrap_or_default();
        let wanted: Vec<Resource> = env
            .secrets
            .iter()
            .map(|s| Resource::new(ResourceKind::Secret, s.clone()))
            .chain(env.mounts.iter().map(|m| Resource::new(ResourceKind::Mount, m.clone())))
            .collect();
        let applied = hub.applied().await;
        let def = applied
            .defs
            .agents
            .get(&self.definition)
            .ok_or_else(|| HubError::Undefined { kind: "agent", name: self.definition.clone() })?;
        let wanted: Vec<Resource> =
            wanted.into_iter().filter(|r| !botler_core::defs::env_gives(&def.spec.env, r, None)).collect();
        for r in &wanted {
            crate::grants::check(&applied.defs, def, r, None)?;
        }
        let parent = caller.instance();
        let instance = router::new_instance(hub, &self.definition, parent, Some(&self.task)).await?;
        let (mut granted, mut requested) = (Vec::new(), Vec::new());
        for r in &wanted {
            // The user decides directly; a parent passes on what it holds; policy may allow it.
            let by = match parent {
                None => Some("user".to_string()),
                Some(p) if crate::grants::holds(hub, p, r, None).await? => Some(format!("spawn:{p}")),
                Some(_) if applied.defs.grant_policy.mode(r) == GrantMode::Auto => Some("policy".into()),
                Some(_) => None,
            };
            match by {
                Some(by) => {
                    crate::grants::give(hub, &instance, r, None, &by, false).await?;
                    granted.push(r.to_string());
                }
                None => {
                    let reason = format!("{} spawned this chat for: {}", parent.unwrap_or("someone"), self.task);
                    if let crate::grants::Requested::Asked(d) =
                        crate::grants::request(hub, &instance, r, &reason, None).await?
                    {
                        requested.push(d.id);
                    }
                }
            }
        }
        router::send_task(hub, &instance, parent, Some(&self.task), None, None).await?;
        if let Some(me) = parent {
            crate::turns::expect_reply(hub, me, &instance, None, None).await?;
        }
        Ok(AgentSpawned { instance, granted, requested })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListChats {}

#[derive(Debug, Serialize, TS)]
pub struct Chats {
    pub chats: Vec<ChatSummary>,
    pub agent_definitions: Vec<AgentDefinitionSummary>,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatSummary {
    pub chat: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub definition: String,
    pub state: ChatState,
    pub task: Option<String>,
    pub last_active: DateTime<Utc>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ChatState {
    /// Working on a turn.
    Working,
    /// Awake, waiting for input.
    Idle,
    Asleep,
}

#[derive(Debug, Serialize, TS)]
pub struct AgentDefinitionSummary {
    pub definition: String,
    pub description: String,
    pub kind: AgentKind,
}

impl Op for ListChats {
    const NAME: &'static str = "list_chats";
    const DESCRIPTION: &'static str = "Every chat (agent instance), running or asleep, with its title, slug and what it was doing, plus the agent definitions you can spawn. Messaging a sleeping chat wakes it with its conversation and workspace intact.";
    const WHO: Who = Who::UserAndAgents;
    type Output = Chats;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Chats, OpError> {
        let applied = hub.applied().await;
        let agent_definitions = applied
            .defs
            .agents
            .values()
            .map(|d| AgentDefinitionSummary {
                definition: d.name.clone(),
                description: d.spec.description.clone(),
                kind: d.spec.kind,
            })
            .collect();
        let mut chats = Vec::new();
        for i in hub.store.live_instances().await? {
            let busy = hub.agents.get(&i.name).await.is_some_and(|a| a.busy());
            let state = match i.state {
                _ if busy => ChatState::Working,
                botler_store::InstanceState::Running => ChatState::Idle,
                _ => ChatState::Asleep,
            };
            chats.push(ChatSummary {
                chat: i.name,
                title: i.title,
                slug: i.slug,
                definition: i.definition,
                state,
                task: i.task,
                last_active: i.last_active,
            });
        }
        Ok(Chats { chats, agent_definitions })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ReadSession {
    pub instance: String,
    /// How many of the most recent messages (default 20, max 200).
    #[serde(default)]
    pub last: Option<u32>,
    /// An entry id (from `search`): read the messages around it instead of the latest.
    #[serde(default)]
    pub around: Option<String>,
    /// With `around`: how many messages before and after it (default 5, max 50).
    #[serde(default)]
    pub n: Option<u32>,
}

#[derive(Debug, Serialize, TS)]
pub struct SessionMessages {
    pub instance: String,
    pub messages: Vec<SessionMessage>,
}

#[derive(Debug, Serialize, TS)]
pub struct SessionMessage {
    /// The entry id, for `around`.
    pub id: String,
    /// `user`, `assistant`, `toolResult`, `note` or `compaction`.
    pub role: &'static str,
    /// At most 4000 characters.
    pub text: String,
    pub at: Option<String>,
}

impl Op for ReadSession {
    const NAME: &'static str = "read_session";
    const DESCRIPTION: &'static str = "Read the recent messages of a session: your own, or another instance's (what it has been doing) if you may. `around` (an entry id from `search`) reads the messages around that entry instead.";
    const WHO: Who = Who::UserAndAgents;
    type Output = SessionMessages;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<SessionMessages, OpError> {
        let inst = hub
            .store
            .resolve_instance(&self.instance)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {}", self.instance)))?;
        if caller.instance() != Some(inst.name.as_str()) {
            caller.require(Capability::ReadSessions)?;
        }
        let path = hub.paths.sessions(&inst.name).join(&inst.session_file);
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionMessages { instance: inst.name, messages: vec![] });
            }
            Err(e) => return Err(HubError::Io(e).into()),
        };
        // Every entry in file order: compaction keeps the history, so this includes what was compacted.
        let mut messages: Vec<SessionMessage> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|e| crate::sessions::parse_entry(&e))
            .filter_map(|item| {
                use crate::sessions::Item;
                let (id, at, role, text) = match item {
                    Item::User { id, at, text, .. } => (id, at, "user", text),
                    Item::Assistant { id, at, text, .. } => (id, at, "assistant", text),
                    Item::ToolResult { id, at, text, .. } => (id, at, "toolResult", text),
                    Item::Note { id, at, text, .. } => (id, at, "note", text),
                    Item::Compaction { id, at, summary } => (id, at, "compaction", summary),
                };
                (!text.is_empty()).then(|| SessionMessage { id, role, text: text.chars().take(4000).collect(), at })
            })
            .collect();
        let range = match &self.around {
            Some(entry) => {
                // A tool call's ref (`<entry>#<call>`) reads around its entry.
                let entry = entry.split('#').next().unwrap_or(entry);
                let i = messages.iter().position(|m| m.id == entry).ok_or_else(|| {
                    OpError::BadRequest(format!("no message {entry} in {}'s current session", inst.name))
                })?;
                let n = self.n.unwrap_or(5).min(50) as usize;
                i.saturating_sub(n)..(i + n + 1).min(messages.len())
            }
            None => messages.len().saturating_sub(self.last.unwrap_or(20).min(200) as usize)..messages.len(),
        };
        Ok(SessionMessages { instance: inst.name, messages: messages.drain(range).collect() })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ArchiveReady {
    /// Everything you did in this chat, for someone who has not read it.
    pub summary: String,
    /// Work you did not finish.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub outstanding: Vec<String>,
    /// Where your state lives: branches, files, memory notes, external systems.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub state: Vec<String>,
    /// Who should continue the outstanding work, if anyone.
    #[serde(default)]
    pub handoff: Option<Handoff>,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(rename = "ArchiveHandoff", optional_fields)]
pub struct Handoff {
    /// An existing chat (name or slug) to send the work to...
    #[serde(default)]
    pub to: Option<String>,
    /// ...or an agent definition to spawn for it.
    #[serde(default)]
    pub spawn: Option<String>,
    /// What they should do; include everything they need.
    pub message: String,
}

#[derive(Debug, Serialize, TS)]
pub struct Archived {
    pub archived: String,
    /// The chat that continues the outstanding work.
    pub handed_to: Option<String>,
    pub note: String,
}

impl Op for ArchiveReady {
    const NAME: &'static str = "archive_ready";
    const DESCRIPTION: &'static str = "Hand over before this chat is archived (only after an archive.requested event): what you did, what is outstanding, where your state lives, and optionally who should continue.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = Archived;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Archived, OpError> {
        let name = caller.agent("only agents archive")?;
        let inst = hub.store.instance(name).await?;
        if inst.archive_requested_at.is_none() {
            return Err(OpError::BadRequest("nobody asked to archive this chat; keep working".into()));
        }
        if self.summary.trim().len() < 20 {
            return Err(OpError::BadRequest("the summary must describe what you did".into()));
        }
        let mut handed_to = None;
        if let Some(h) = &self.handoff {
            handed_to = Some(match (&h.to, &h.spawn) {
                (Some(to), None) => {
                    let message = SendMessage {
                        to: to.clone(),
                        text: h.message.clone(),
                        urgent: false,
                        reply_to: None,
                        expect_reply_within: None,
                    };
                    message.call(hub, caller).await?;
                    to.clone()
                }
                (None, Some(def)) => router::spawn(hub, def, Some(name), Some(&h.message), None, None).await?,
                _ => return Err(OpError::BadRequest("handoff needs exactly one of `to` or `spawn`".into())),
            });
        }
        let report = botler_store::ArchiveRecord {
            confirmed_by_agent: true,
            forced: false,
            reason: None,
            report: Some(botler_store::HandoverReport {
                summary: self.summary,
                outstanding: self.outstanding,
                state: self.state,
                handoff: self.handoff.map(|h| botler_store::Handoff { to: h.to, spawn: h.spawn, message: h.message }),
            }),
            handed_to: handed_to.clone(),
            at: Utc::now(),
        };
        hub.store.archive_instance(name, &report).await?;
        crate::agents::stop_after_reply(hub, name).await;
        tracing::info!(instance = name, handed_to = ?handed_to, "chat archived");
        Ok(Archived {
            archived: name.into(),
            handed_to,
            note: "Archived. Stop now; you will be restored if messaged.".into(),
        })
    }
}
