//! Event types and runtime subscriptions: what can happen, who produces and receives it, and a
//! chat asking to be told when it happens.

use std::collections::BTreeMap;
use std::sync::Arc;

use botler_core::catalog::BUILTIN;
use botler_core::defs::{Capability, Definitions, Lifetime, Resource, ResourceKind, TriggerKind, Undeclared};
use botler_core::event_check::{known_types, payload_fields, similar_among};
use botler_core::routing::{Subscription, matches_type};
use botler_store::NewSubscription;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;

/// Subscriptions one chat may hold.
const MAX_PER_CHAT: usize = 50;

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct Subscribe {
    /// Event type (`mail.received`) or glob (`mail.*`); see `list_event_types`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Payload fields that must have these values, e.g. `{"folder": "INBOX"}`.
    #[serde(default)]
    #[ts(optional, as = "Option<BTreeMap<String, Value>>")]
    pub filter: BTreeMap<String, Value>,
    /// `false`: deliver as a quiet note that waits for whatever wakes you next (default `true`:
    /// each event wakes you).
    #[serde(default)]
    pub wake: Option<bool>,
    /// Why you subscribed; shown with every delivery.
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct Subscribed {
    pub subscription: Subscription,
    /// The known event types it matches.
    pub matches: Vec<String>,
}

impl Op for Subscribe {
    const NAME: &'static str = "subscribe";
    const DESCRIPTION: &'static str = "Get events of a type (or glob like `mail.*`) delivered to this chat, optionally only those whose payload fields equal `filter`. They wake you even while you sleep (`wake=false`: quiet notes instead). Lasts until `unsubscribe`; find types with `list_event_types` or `search(sources=[\"event_type\"])`. Restricted types need `request_access(\"event:<type>\")` first.";
    const WHO: Who = Who::UserAndAgents;
    type Output = Subscribed;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Subscribed, OpError> {
        let instance = caller.agent("only chats subscribe")?;
        let applied = hub.applied().await;
        let defs = &applied.defs;
        let matches: Vec<String> =
            known_types(defs).filter(|k| matches_type(&self.kind, k)).map(String::from).collect();
        if matches.is_empty() {
            let similar = similar_among(known_types(defs), &self.kind);
            let hint = if similar.is_empty() {
                String::new()
            } else {
                format!(" Did you mean: {}?", similar.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "))
            };
            return Err(OpError::BadRequest(format!(
                "no event type matches `{}`.{hint} Use `list_event_types` to see them.",
                self.kind
            )));
        }
        for t in &matches {
            if let Some(fields) = payload_fields(defs, t) {
                let unknown: Vec<&String> =
                    self.filter.keys().filter(|k| !fields.contains(&head(k).to_string())).collect();
                if !unknown.is_empty() {
                    return Err(OpError::BadRequest(format!(
                        "`{t}` has no field {}; its fields are {}",
                        unknown.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", "),
                        fields.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
                    )));
                }
            }
        }
        let emitted = hub.store.emitted_kinds(&caller.source().key()).await?;
        if let Some(own) = emitted.iter().find(|k| matches_type(&self.kind, k)) {
            return Err(OpError::BadRequest(format!(
                "you emit `{own}` yourself; subscribing to your own events would loop"
            )));
        }
        for t in matches.iter().filter(|t| defs.events.get(*t).is_some_and(|e| e.restricted)) {
            let r = Resource::new(ResourceKind::Event, t.clone());
            if !crate::grants::holds(hub, instance, &r).await? {
                return Err(OpError::Forbidden(format!(
                    "`{t}` is restricted; ask for it with `request_access(\"{r}\", reason)` first"
                )));
            }
        }
        if hub.store.subscriptions_of(instance).await?.len() >= MAX_PER_CHAT {
            return Err(OpError::BadRequest(format!(
                "you have {MAX_PER_CHAT} subscriptions; unsubscribe from some first"
            )));
        }
        let subscription = hub
            .store
            .add_subscription(&NewSubscription {
                instance: instance.into(),
                pattern: self.kind,
                filter: self.filter,
                wake: self.wake.unwrap_or(true),
                note: self.note,
            })
            .await?;
        tracing::info!(instance, pattern = %subscription.pattern, id = %subscription.id, "subscribed");
        Ok(Subscribed { subscription, matches })
    }
}

/// The top-level field of a filter path (`a.b` → `a`).
fn head(path: &str) -> &str {
    path.split('.').next().unwrap_or(path)
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct Unsubscribe {
    /// The subscription id.
    pub id: String,
}

#[derive(Debug, Serialize, TS)]
pub struct Unsubscribed {
    pub removed: bool,
}

impl Op for Unsubscribe {
    const NAME: &'static str = "unsubscribe";
    const DESCRIPTION: &'static str = "Stop a subscription by id (from `subscribe` or `list_subscriptions`).";
    const WHO: Who = Who::UserAndAgents;
    type Output = Unsubscribed;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Unsubscribed, OpError> {
        let sub = hub.store.subscription(&self.id).await?;
        if caller.instance().is_some_and(|me| me != sub.instance) {
            return Err(OpError::Forbidden("that subscription is not yours".into()));
        }
        Ok(Unsubscribed { removed: hub.store.delete_subscription(&self.id).await? })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ListSubscriptions {}

#[derive(Debug, Serialize, TS)]
pub struct Subscriptions {
    pub subscriptions: Vec<Subscription>,
}

impl Op for ListSubscriptions {
    const NAME: &'static str = "list_subscriptions";
    const DESCRIPTION: &'static str = "Your event subscriptions with their ids.";
    const WHO: Who = Who::UserAndAgents;
    type Output = Subscriptions;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Subscriptions, OpError> {
        let instance = caller.agent("only chats have subscriptions; see a chat's connections")?;
        Ok(Subscriptions { subscriptions: hub.store.subscriptions_of(instance).await? })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct ListEventTypes {
    /// Words or a glob (`mail.*`) to narrow the list by type or description.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct EventTypes {
    pub types: Vec<EventTypeInfo>,
    /// What `emit_event` does with undeclared types.
    pub undeclared_policy: Undeclared,
}

#[derive(Debug, Serialize, TS)]
pub struct EventTypeInfo {
    #[serde(rename = "type")]
    pub kind: String,
    pub description: String,
    /// The hub's own type (agents and jobs cannot emit it).
    pub builtin: bool,
    /// Subscribing needs an `event:<type>` grant.
    pub restricted: bool,
    /// JSON Schema of the payload.
    pub schema: Option<Value>,
    /// Who emits it: `hub`, or the jobs and services declaring it in `emits`.
    pub producers: Vec<String>,
    /// Who receives it: definition triggers (`trigger <id> → <target>`) and chats' subscriptions.
    pub subscribers: Vec<String>,
}

/// Every event type with its producers and (when `subs` is given) subscribers.
pub fn catalog(defs: &Definitions, subs: &[Subscription]) -> Vec<EventTypeInfo> {
    let builtin = BUILTIN.iter().map(|b| {
        (b.kind.to_string(), b.summary.to_string(), true, false, botler_core::payloads::builtin_schema(b.kind))
    });
    let declared =
        defs.events.values().map(|e| (e.kind.clone(), e.description.clone(), false, e.restricted, e.schema.clone()));
    builtin
        .chain(declared)
        .map(|(kind, description, builtin, restricted, schema)| {
            let producers = if builtin {
                vec!["hub".to_string()]
            } else {
                defs.jobs
                    .values()
                    .filter(|j| j.spec.emits.contains(&kind))
                    .map(|j| {
                        let what = if j.spec.lifetime == Lifetime::Service { "service" } else { "job" };
                        format!("{what} {}", j.name)
                    })
                    .collect()
            };
            let mut subscribers: Vec<String> = defs
                .triggers
                .values()
                .filter(|t| matches!(&t.kind, TriggerKind::On { pattern } if matches_type(pattern, &kind)))
                .map(|t| format!("trigger {} → {}", t.id, botler_core::change::describe_target(&t.target)))
                .collect();
            subscribers.extend(
                subs.iter()
                    .filter(|s| matches_type(&s.pattern, &kind))
                    .map(|s| format!("chat {} (subscription {})", s.instance, s.id)),
            );
            EventTypeInfo { kind, description, builtin, restricted, schema, producers, subscribers }
        })
        .collect()
}

impl Op for ListEventTypes {
    const NAME: &'static str = "list_event_types";
    const DESCRIPTION: &'static str = "Event types (built-in and declared) with their description, payload schema, producers and current subscribers; `query` narrows by words or a glob. `emit_event` validates payloads against these; `subscribe` takes their names.";
    const WHO: Who = Who::Everyone;
    type Output = EventTypes;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<EventTypes, OpError> {
        let applied = hub.applied().await;
        // Who else subscribes is about other chats: agents see it with `read_sessions`.
        let subs = match caller.instance() {
            Some(_) if !caller.may(Capability::ReadSessions) => {
                let me = caller.instance().unwrap_or_default();
                hub.store.active_subscriptions().await?.into_iter().filter(|s| s.instance == me).collect()
            }
            _ => hub.store.active_subscriptions().await?,
        };
        let mut types = catalog(&applied.defs, &subs);
        if let Some(q) = self.query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
            let q = q.to_lowercase();
            types.retain(|t| {
                matches_type(&q, &t.kind)
                    || q.split_whitespace()
                        .all(|w| t.kind.to_lowercase().contains(w) || t.description.to_lowercase().contains(w))
            });
        }
        Ok(EventTypes { types, undeclared_policy: applied.defs.instance.events.undeclared })
    }
}
