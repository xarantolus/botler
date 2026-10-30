//! Restoring an archived chat. The world may have moved on while it was archived: its definition,
//! the resources of its grants and the event types it subscribed to can be gone. Restoring checks
//! each edge against the current definitions, marks what no longer applies as stale (kept for the
//! record, never applied), and tells the chat in a `chat.restored` note.

use botler_core::Source;
use botler_core::defs::ResourceKind;
use botler_core::event_check::known_types;
use botler_core::payloads::{BuiltinPayload, ChatRestoredPayload};
use botler_core::routing::matches_type;
use botler_store::InstanceState;

use crate::hub::{Hub, HubError, Result, builtin};

/// The generic agent a chat can be restored as when its own definition is gone.
pub const GENERIC: &str = "chat";

/// The subject messages to a chat that cannot be restored are held under.
pub fn held_subject(instance: &str) -> String {
    format!("restore:{instance}")
}

/// Restore `instance` if it is archived. Its definition must still exist, unless `as_generic`
/// (then it becomes a [`GENERIC`] chat). Returns what changed while it was archived.
pub async fn restore(hub: &Hub, instance: &str, as_generic: bool) -> Result<Vec<String>> {
    let inst = hub.store.instance(instance).await?;
    if inst.state != InstanceState::Archived {
        return Ok(vec![]);
    }
    let applied = hub.applied().await;
    let defs = &applied.defs;
    let mut changes = Vec::new();
    if !defs.agents.contains_key(&inst.definition) {
        if !as_generic || !defs.agents.contains_key(GENERIC) {
            return Err(HubError::DefinitionGone { instance: inst.name, definition: inst.definition });
        }
        hub.store.set_instance_definition(instance, GENERIC).await?;
        changes.push(format!(
            "your agent definition `{}` no longer exists; you are now a generic `{GENERIC}` agent",
            inst.definition
        ));
    }
    let def = defs.agents.get(inst.definition.as_str()).or_else(|| defs.agents.get(GENERIC));
    let with_values = hub.store.secret_names().await?;
    for g in hub.store.live_grants(instance).await?.into_iter().filter(|g| g.stale.is_none()) {
        let r = g.resource();
        let gone = match def {
            Some(def) => crate::grants::check(defs, def, &r, g.mode).err().map(|e| e.to_string()),
            None => Some("no agent definition".into()),
        };
        let missing = match defs.secret_groups.get(&r.name) {
            Some(group) if r.kind == ResourceKind::Secret => {
                let missing: Vec<&str> =
                    group.secrets.iter().filter(|s| !with_values.contains(s)).map(String::as_str).collect();
                (!missing.is_empty())
                    .then(|| format!("secret group {} has no value for {}", r.name, missing.join(", ")))
            }
            _ => None,
        };
        if let Some(reason) = gone.or(missing) {
            hub.store.mark_grant_stale(&g.id, &reason).await?;
            changes.push(format!("grant `{r}` no longer applies: {reason}"));
        }
    }
    for s in hub.store.subscriptions_of(instance).await?.into_iter().filter(|s| s.stale.is_none()) {
        if !known_types(defs).any(|k| matches_type(&s.pattern, k)) {
            let reason = "no event type matches it any more";
            hub.store.mark_subscription_stale(&s.id, reason).await?;
            changes.push(format!("subscription to `{}` no longer applies: {reason}", s.pattern));
        }
    }
    hub.store.restore_instance(instance).await?;
    // Messages that arrived while it could not be restored.
    hub.store.release_held(&held_subject(instance)).await?;
    tracing::info!(instance, changes = changes.len(), "chat restored");
    if !changes.is_empty() {
        let archived_at = inst.archive_report.map(|r| r.at);
        let note = BuiltinPayload::ChatRestored(ChatRestoredPayload { archived_at, changes: changes.clone() });
        // Queued here rather than routed later, so it is a quiet note ahead of the delivery
        // that restored the chat.
        hub.store.append_note(&builtin(note, Source::Hub)?.to(instance.to_string()), instance).await?;
    }
    Ok(changes)
}
