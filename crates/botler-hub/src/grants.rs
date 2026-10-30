//! Grants: resources a chat holds beyond its definition (secret groups, mounts, services,
//! restricted event types). They come from `spawn_agent(env = ...)` or `request_access`, are
//! applied to the container on every wake, and end when revoked. `policies/grants.toml` says per
//! resource whether a chat gets it without asking; "always allow" writes that file through a hub
//! commit.

use botler_core::Source;
use botler_core::defs::{
    AgentDef, Definitions, Environment, GrantMode, Lifetime, MountMode, Network, Resource, ResourceKind,
};
use botler_core::payloads::{AccessChangedPayload, BuiltinPayload, DialogAnsweredPayload};
use botler_store::{Dialog, DialogKind, Grant, NewDialog, Priority};
use serde::{Deserialize, Serialize};

use crate::hub::{Hub, HubError, builtin, load_at};
use crate::ops::OpError;

/// The answers of an access request.
pub const ALLOW_CHAT: &str = "allow for this chat";
pub const ALLOW_ALWAYS: &str = "always allow";
pub const DENY: &str = "deny";

const POLICY_FILE: &str = "policies/grants.toml";

/// What an access request asks for (stored as the dialog's `spec`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessSpec {
    pub resource: String,
    pub reason: String,
    #[serde(default)]
    pub mode: Option<MountMode>,
}

/// Whether `def`'s chats can hold `r` (a mount in `mode`) at all. Errors say what to fix.
pub fn check(defs: &Definitions, def: &AgentDef, r: &Resource, mode: Option<MountMode>) -> Result<(), OpError> {
    let bad = |m: String| Err(OpError::BadRequest(m));
    match r.kind {
        ResourceKind::Secret => match defs.secret_groups.get(&r.name) {
            None => bad(format!("no secret group {} in secrets.toml; ask for it with `request_secrets` first", r.name)),
            Some(g) if g.class == botler_core::defs::SecretClass::Hub => {
                bad(format!("{} is a hub-class group and never enters a container", r.name))
            }
            Some(_) => Ok(()),
        },
        ResourceKind::Service => match defs.jobs.get(&r.name) {
            Some(j) if j.spec.lifetime == Lifetime::Service && j.spec.env.port.is_some() => {
                if def.spec.env.network() == Network::None {
                    return bad(format!("agent {} has no network, so it cannot reach services", def.name));
                }
                Ok(())
            }
            _ => bad(format!("{} is not a service with a `port` in jobs/", r.name)),
        },
        ResourceKind::Mount => match defs.mounts.get(&r.name) {
            None => bad(format!("no share {} in mounts/; define mounts/{}.toml first", r.name, r.name)),
            Some(m) if m.spec.read_only && mode == Some(MountMode::ReadWrite) => {
                bad(format!("share {} is read_only = true in mounts/{}.toml", r.name, r.name))
            }
            Some(_) if def.spec.env.network() == Network::None => {
                bad(format!("agent {} has no network, so it cannot reach shares", def.name))
            }
            Some(_) => Ok(()),
        },
        ResourceKind::Event => match defs.events.get(&r.name) {
            Some(e) if e.restricted => Ok(()),
            Some(_) => bad(format!("event type {} is not restricted; subscribe without a grant", r.name)),
            None => bad(format!("event type {} is not declared", r.name)),
        },
    }
}

/// The environment `instance`'s grants add to its definition's, and the ids of those grants (to
/// notice when they change).
pub async fn environment(hub: &Hub, instance: &str) -> Result<(Environment, Vec<String>), HubError> {
    let grants: Vec<Grant> = hub.store.live_grants(instance).await?.into_iter().filter(|g| g.stale.is_none()).collect();
    let held: Vec<(Resource, Option<MountMode>)> = grants.iter().map(|g| (g.resource(), g.mode)).collect();
    let env = botler_core::defs::environment_of(held.iter().map(|(r, m)| (r, *m)));
    Ok((env, grants.into_iter().map(|g| g.id).collect()))
}

/// Whether `instance` (its definition or a live grant) has `r` (a mount at least in `mode`).
pub async fn holds(hub: &Hub, instance: &str, r: &Resource, mode: Option<MountMode>) -> Result<bool, HubError> {
    let Some(inst) = hub.store.find_instance(instance).await? else { return Ok(false) };
    let applied = hub.applied().await;
    if applied.defs.agents.get(&inst.definition).is_some_and(|d| botler_core::defs::env_gives(&d.spec.env, r, mode)) {
        return Ok(true);
    }
    let wide_enough =
        |g: &Grant| r.kind != ResourceKind::Mount || g.mode.unwrap_or_default() >= mode.unwrap_or_default();
    Ok(hub.store.live_grants(instance).await?.iter().any(|g| g.stale.is_none() && g.resource() == *r && wide_enough(g)))
}

/// Record a grant and tell the holder when nobody asked it (`notify`).
pub async fn give(
    hub: &Hub,
    holder: &str,
    r: &Resource,
    mode: Option<MountMode>,
    by: &str,
    notify: bool,
) -> Result<Grant, HubError> {
    let g = hub.store.add_grant(holder, r, mode, by).await?;
    tracing::info!(holder, resource = %r, by, "granted");
    if notify {
        changed(
            hub,
            &g,
            "granted",
            format!("You now have `{r}` (policy `auto`); it is applied when you are next woken."),
        )
        .await?;
    }
    Ok(g)
}

async fn changed(hub: &Hub, g: &Grant, change: &str, note: String) -> Result<(), HubError> {
    let payload = BuiltinPayload::AccessChanged(AccessChangedPayload {
        grant_id: g.id.clone(),
        resource: g.resource().to_string(),
        change: change.into(),
        note,
    });
    hub.emit(builtin(payload, Source::Hub)?.to(g.holder.clone())).await?;
    Ok(())
}

/// What a request for access came to.
pub enum Requested {
    /// The chat has it already.
    Held,
    /// Granted by an `auto` policy.
    Granted(Grant),
    /// The user decides.
    Asked(Dialog),
}

/// `instance` asks for `r`: granted at once under an `auto` policy, otherwise an access request
/// to the user.
pub async fn request(
    hub: &Hub,
    instance: &str,
    r: &Resource,
    reason: &str,
    mode: Option<MountMode>,
) -> Result<Requested, OpError> {
    if reason.trim().is_empty() {
        return Err(OpError::BadRequest("give a reason the user can judge".into()));
    }
    if holds(hub, instance, r, mode).await? {
        return Ok(Requested::Held);
    }
    let inst = hub.store.instance(instance).await?;
    let applied = hub.applied().await;
    let def = applied
        .defs
        .agents
        .get(&inst.definition)
        .ok_or_else(|| HubError::Undefined { kind: "agent", name: inst.definition.clone() })?;
    check(&applied.defs, def, r, mode)?;
    if applied.defs.grant_policy.mode(r) == GrantMode::Auto && mode != Some(MountMode::ReadWrite) {
        return Ok(Requested::Granted(give(hub, instance, r, mode, "policy", true).await?));
    }
    let spec = AccessSpec { resource: r.to_string(), reason: reason.into(), mode };
    let who = inst.title.as_deref().unwrap_or(instance);
    let d = hub
        .store
        .create_dialog(&NewDialog {
            instance: instance.into(),
            kind: DialogKind::Access,
            question: format!("Give `{r}` to {who}?"),
            detail: Some(format!(
                "{reason}\n\n{}. *Always allow* lets every chat get it without asking (written to `{POLICY_FILE}`).",
                describe(r, mode)
            )),
            ctas: vec![ALLOW_CHAT.into(), ALLOW_ALWAYS.into(), DENY.into()],
            about: vec![],
            priority: Priority::Blocking,
            proposal_id: None,
            spec: Some(serde_json::to_value(&spec).map_err(|e| HubError::Io(std::io::Error::other(e)))?),
            expires_at: None,
        })
        .await?;
    crate::notify::dialog_opened(hub, &d).await;
    Ok(Requested::Asked(d))
}

fn describe(r: &Resource, mode: Option<MountMode>) -> String {
    match r.kind {
        ResourceKind::Secret => format!("The secrets of group `{}` enter this chat's container", r.name),
        ResourceKind::Service => format!("This chat may use service `{}`", r.name),
        ResourceKind::Mount => format!(
            "Share `{}` is mounted into this chat's container ({})",
            r.name,
            if mode == Some(MountMode::ReadWrite) { "read-write" } else { "read-only" }
        ),
        ResourceKind::Event => format!("This chat may subscribe to the restricted event type `{}`", r.name),
    }
}

/// Carry out the user's answer to an access request and tell the chat.
pub async fn answer(hub: &Hub, dialog_id: &str, answer: &str) -> Result<Dialog, OpError> {
    let d = hub.store.dialog(dialog_id).await?;
    let spec: AccessSpec = serde_json::from_value(d.spec.clone().unwrap_or_default())
        .map_err(|e| OpError::BadRequest(format!("access request {dialog_id} is unreadable: {e}")))?;
    let r: Resource = spec.resource.parse().map_err(OpError::BadRequest)?;
    let d = hub.store.answer_dialog(dialog_id, answer).await?;
    let note = match answer {
        DENY => "Denied. Do without it, or explain to the user why you need it.".to_string(),
        _ => {
            give(hub, &d.instance, &r, spec.mode, "user", false).await?;
            if answer == ALLOW_ALWAYS {
                set_auto(hub, &r, &d.instance).await?;
            }
            format!("Granted `{r}`; you were restarted with it, so it is available now.")
        }
    };
    let payload = BuiltinPayload::DialogAnswered(DialogAnsweredPayload {
        dialog_id: d.id.clone(),
        question: d.question.clone(),
        answer: answer.into(),
        group: None,
        outcome: None,
        note: Some(note),
    });
    hub.emit(builtin(payload, Source::User)?.to(d.instance.clone())).await?;
    Ok(d)
}

/// Revoke grant `id`. `by` is the holder revoking its own grant, or `None` for the user.
pub async fn revoke(hub: &Hub, id: &str, by: Option<&str>) -> Result<(bool, Grant), OpError> {
    let g = hub.store.grant(id).await?;
    if by.is_some_and(|b| b != g.holder) {
        return Err(OpError::Forbidden("that grant is not yours".into()));
    }
    let revoked = hub.store.revoke_grant(id).await?;
    if revoked && by.is_none() {
        let note = format!("The user revoked `{}`; it is gone from your next wake on.", g.resource());
        changed(hub, &g, "revoked", note).await?;
    }
    tracing::info!(grant = id, holder = %g.holder, revoked, "grant revoked");
    Ok((revoked, hub.store.grant(id).await?))
}

/// `policies/grants.toml` with `r` set to `auto`, keeping comments and layout.
pub fn edit_policy(text: &str, r: &Resource) -> Result<String, String> {
    use toml_edit::{DocumentMut, Item, Table, value};
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    if !doc.contains_key("version") {
        doc["version"] = value(1);
    }
    if !doc.contains_key("resources") {
        doc["resources"] = Item::Table(Table::new());
    }
    let t = doc["resources"].as_table_mut().ok_or("`resources` is not a table")?;
    t[&r.to_string()] = value("auto");
    Ok(doc.to_string())
}

async fn set_auto(hub: &Hub, r: &Resource, instance: &str) -> Result<(), HubError> {
    let message = format!("grants: always allow {r} (asked by {instance}, approved by the user)");
    commit_edit(hub, POLICY_FILE, &message, |text| edit_policy(text, r)).await
}

/// Edit one file on main as the hub (the user's answer is the approval), validate the result and
/// apply it. Retries once if main moved meanwhile.
pub async fn commit_edit(
    hub: &Hub,
    path: &str,
    message: &str,
    edit: impl Fn(&str) -> Result<String, String>,
) -> Result<(), HubError> {
    for attempt in 0..2 {
        let base = hub.repo.main().await?;
        let text = hub.repo.read_file(&base, path).await?.unwrap_or_default();
        let edited = edit(&text).map_err(|e| HubError::Forbidden(format!("{path} could not be edited: {e}")))?;
        let head = hub.repo.commit_files(&base, &[(path, Some(edited.as_str()))], message).await?;
        // Validate before anything is applied: an answer must never break main.
        let applied = load_at(&hub.repo, &head).await?;
        match hub.repo.advance_main(&base, &head).await {
            Ok(()) => {
                hub.apply(applied).await?;
                tracing::info!(path, commit = %head, "hub commit applied");
                return Ok(());
            }
            Err(botler_repo::Error::MainMoved { .. }) if attempt == 0 => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(HubError::Forbidden("main kept moving; try again".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_edit_keeps_comments() {
        let r: Resource = "secret:nas".parse().unwrap();
        let out = edit_policy("# who gets what\nversion = 1\n\n[resources]\n\"service:x\" = \"ask\"\n", &r).unwrap();
        assert!(out.starts_with("# who gets what"), "{out}");
        let p: botler_core::defs::GrantPolicy = toml::from_str(&out).unwrap();
        assert_eq!(p.mode(&r), GrantMode::Auto);
        assert_eq!(p.resources.len(), 2);
        let p: botler_core::defs::GrantPolicy = toml::from_str(&edit_policy("", &r).unwrap()).unwrap();
        assert_eq!(p.mode(&r), GrantMode::Auto);
    }
}
