//! A chat's edges: grants it holds (`request_access`, `revoke_access`) and everything it is
//! connected to (`list_connections`).

use std::sync::Arc;

use botler_core::defs::{Capability, MountMode, Resource, Target};
use botler_core::routing::Subscription;
use botler_store::{Grant, Timer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::grants::{self, Requested};
use crate::hub::Hub;

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct RequestAccess {
    /// What you need, as `<kind>:<name>`: `secret:<group>` (a secret group's values in your
    /// container), `service:<name>` (a service's MCP server), `mount:<name>` (a network share) or
    /// `event:<type>` (subscribing to a restricted event type).
    pub resource: String,
    /// Why, for the user deciding.
    pub reason: String,
    /// Mounts: `rw` to write (default `ro`).
    #[serde(default)]
    pub mode: Option<MountMode>,
}

#[derive(Debug, Serialize, TS)]
pub struct AccessRequested {
    /// `held` (you have it already), `granted` (allowed by policy) or `asked` (the user decides).
    pub status: String,
    #[ts(optional)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    pub note: String,
}

impl Op for RequestAccess {
    const NAME: &'static str = "request_access";
    const DESCRIPTION: &'static str = "Ask for a resource your definition does not give you, for this chat: `secret:<group>`, `service:<name>`, `mount:<name>` or `event:<type>`. Policy may grant it at once; otherwise the user decides (allow for this chat, always allow, or deny) and the answer wakes you as a `dialog.answered` event, with the grant applied to your container. Memory notes say which group fits which work.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = AccessRequested;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<AccessRequested, OpError> {
        let instance = caller.agent("only agents request access")?;
        let r: Resource = self.resource.parse().map_err(OpError::BadRequest)?;
        Ok(match grants::request(hub, instance, &r, &self.reason, self.mode).await? {
            Requested::Held => AccessRequested {
                status: "held".into(),
                request_id: None,
                note: format!("You already have `{r}`."),
            },
            Requested::Granted(_) => AccessRequested {
                status: "granted".into(),
                request_id: None,
                note: format!("Granted by policy. You are woken with `{r}` applied; stop now."),
            },
            Requested::Asked(d) => AccessRequested {
                status: "asked".into(),
                request_id: Some(d.id),
                note: "The user decides; the answer arrives as a dialog.answered event. Stop now or continue with other work.".into(),
            },
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct RevokeAccess {
    /// The grant id (from `list_connections`).
    pub grant: String,
}

#[derive(Debug, Serialize, TS)]
pub struct AccessRevoked {
    /// Whether it was still in force.
    pub revoked: bool,
    pub grant: Grant,
}

impl Op for RevokeAccess {
    const NAME: &'static str = "revoke_access";
    const DESCRIPTION: &'static str =
        "Give up one of your grants (the user can revoke any). It is gone from your next wake on.";
    const WHO: Who = Who::UserAndAgents;
    type Output = AccessRevoked;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<AccessRevoked, OpError> {
        let (revoked, grant) = grants::revoke(hub, &self.grant, caller.instance()).await?;
        Ok(AccessRevoked { revoked, grant })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ListConnections {
    /// The chat (name or slug); defaults to you.
    #[serde(default)]
    pub chat: Option<String>,
}

/// A chat's edges in the graph of chats, resources and events.
#[derive(Debug, Serialize, TS)]
pub struct Connections {
    pub chat: String,
    /// Archived: grants and subscriptions are suspended until it is restored.
    pub archived: bool,
    /// The chat that spawned it.
    pub parent: Option<String>,
    /// The chats it spawned.
    pub children: Vec<String>,
    /// Secret groups its definition gives it.
    pub definition_secrets: Vec<String>,
    /// What it holds beyond its definition.
    pub grants: Vec<Grant>,
    /// Services it uses (from its definition or a grant).
    pub services: Vec<String>,
    pub subscriptions: Vec<Subscription>,
    /// Pending timers it set or that wake it.
    pub timers: Vec<Timer>,
}

impl Op for ListConnections {
    const NAME: &'static str = "list_connections";
    const DESCRIPTION: &'static str = "What a chat (default: you) is connected to: its parent and children, secret groups and services from its definition, grants it holds (with ids, for `revoke_access`), event subscriptions and pending timers.";
    const WHO: Who = Who::UserAndAgents;
    type Output = Connections;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Connections, OpError> {
        let name = match (&self.chat, caller.instance()) {
            (Some(c), _) => c.clone(),
            (None, Some(me)) => me.to_string(),
            (None, None) => return Err(OpError::BadRequest("name the chat".into())),
        };
        let inst = hub
            .store
            .resolve_instance(&name)
            .await?
            .ok_or_else(|| OpError::BadRequest(format!("no chat named {name}")))?;
        if caller.instance().is_some_and(|me| me != inst.name) {
            caller.require(Capability::ReadSessions)?;
        }
        let applied = hub.applied().await;
        let def_env = applied.defs.agents.get(&inst.definition).map(|d| d.spec.env.clone()).unwrap_or_default();
        let grants = hub.store.live_grants(&inst.name).await?;
        let (granted, _) = grants::environment(hub, &inst.name).await?;
        let services = def_env.extend(&granted).0.mcp;
        let me = format!("agent:{}", inst.name);
        let timers = hub
            .store
            .pending_timers(None)
            .await?
            .into_iter()
            .filter(|t| t.created_by == me || t.target == Target::Resume(inst.name.clone()))
            .collect();
        Ok(Connections {
            archived: inst.state == botler_store::InstanceState::Archived,
            parent: inst.parent.clone(),
            children: hub.store.children(&inst.name).await?,
            definition_secrets: def_env.secrets,
            grants,
            services,
            subscriptions: hub.store.subscriptions_of(&inst.name).await?,
            timers,
            chat: inst.name,
        })
    }
}
