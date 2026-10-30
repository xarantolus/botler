//! Change previews: the capability diff between two commits, a human-readable deployment summary,
//! and the transaction decision table.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::defs::{
    CapabilityPolicy, Definitions, Lifetime, Network, ResolvedTrigger, SecretClass, Target, TriggerKind,
};
use crate::limits::default_limit;

/// Why a change needs the user's approval (never auto-merged).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Escalation {
    ProtectedPath {
        path: String,
    },
    SecretGranted {
        secret: String,
        group: String,
        to: String,
    },
    SecretDeclared {
        secret: String,
        group: String,
        class: SecretClass,
    },
    SecretClassChanged {
        secret: String,
    },
    NetworkOpened {
        agent: String,
    },
    CapabilityAdded {
        agent: String,
        capability: String,
    },
    McpAccessAdded {
        agent: String,
        service: String,
    },
    LimitRaised {
        owner: String,
        operation: String,
    },
    ToolEffect {
        tool: String,
        effect: String,
    },
    ApprovalRemoved {
        tool: String,
    },
    WebhookAdded {
        name: String,
    },
    ServiceAdded {
        job: String,
    },
    /// The protection policy itself could not be evaluated; treated as touching everything.
    InvalidPolicy {
        detail: String,
    },
}

impl Escalation {
    pub fn explain(&self) -> String {
        match self {
            Self::ProtectedPath { path } => format!("edits protected path `{path}`"),
            Self::SecretGranted { secret, group, to } => format!("gives {to} secret `{secret}` (group `{group}`)"),
            Self::SecretDeclared { secret, group, class } => {
                format!("declares new {} secret `{secret}` in group `{group}`", format!("{class:?}").to_lowercase())
            }
            Self::SecretClassChanged { secret } => format!("changes the class, group or delivery of secret `{secret}`"),
            Self::NetworkOpened { agent } => format!("gives agent `{agent}` network access"),
            Self::CapabilityAdded { agent, capability } => {
                format!("gives agent `{agent}` the `{capability}` capability")
            }
            Self::McpAccessAdded { agent, service } => format!("lets agent `{agent}` reach service `{service}`"),
            Self::LimitRaised { owner, operation } => format!("raises the `{operation}` rate limit for {owner}"),
            Self::ToolEffect { tool, effect } => format!("tool `{tool}` gains effect `{effect}`"),
            Self::ApprovalRemoved { tool } => format!("action `{tool}` no longer requires approval"),
            Self::WebhookAdded { name } => format!("opens webhook `/hooks/{name}` to inbound calls"),
            Self::ServiceAdded { job } => format!("adds always-on service `{job}`"),
            Self::InvalidPolicy { detail } => {
                format!("cannot evaluate protected paths ({detail}); treating all paths as protected")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TriggerPreview {
    pub id: String,
    pub status: Status,
    /// Plain-language "when".
    pub when: String,
    /// Plain-language "what".
    pub then: String,
    /// Next fire times for schedules (UTC).
    pub upcoming: Vec<DateTime<Utc>>,
    pub shadow: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ItemChange {
    pub kind: &'static str,
    pub name: String,
    pub status: Status,
}

/// The full preview of applying `new` on top of `old`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChangeSummary {
    pub items: Vec<ItemChange>,
    pub triggers: Vec<TriggerPreview>,
    pub escalations: Vec<Escalation>,
}

pub const UPCOMING: usize = 5;

/// Compare two definition sets. `changed_paths` are repo paths touched by the change.
pub fn preview(old: &Definitions, new: &Definitions, changed_paths: &[String], now: DateTime<Utc>) -> ChangeSummary {
    let mut esc = BTreeSet::new();

    // Protection is judged by the *old* policy plus the built-ins, so a change cannot unprotect
    // paths and edit them in the same step.
    let mut protected: Vec<String> = old.policy.protected_paths.clone();
    protected.extend(CapabilityPolicy::ALWAYS_PROTECTED.iter().map(|s| (*s).to_string()));
    protected.push("policies/capabilities.toml".into());
    // Fail closed: if the protection set cannot be built, every changed path counts as protected.
    let mut builder = globset::GlobSetBuilder::new();
    let mut invalid = Vec::new();
    for p in &protected {
        match globset::Glob::new(p) {
            Ok(g) => {
                builder.add(g);
            }
            Err(e) => invalid.push(format!("{p:?}: {e}")),
        }
    }
    let set = match builder.build() {
        Ok(set) if invalid.is_empty() => Some(set),
        Ok(_) => {
            esc.insert(Escalation::InvalidPolicy { detail: invalid.join("; ") });
            None
        }
        Err(e) => {
            esc.insert(Escalation::InvalidPolicy { detail: e.to_string() });
            None
        }
    };
    for p in changed_paths {
        if set.as_ref().is_none_or(|set| set.is_match(p)) {
            esc.insert(Escalation::ProtectedPath { path: p.clone() });
        }
    }

    for (name, decl) in &new.secrets {
        match old.secrets.get(name) {
            None => {
                esc.insert(Escalation::SecretDeclared {
                    secret: name.clone(),
                    group: decl.group.clone(),
                    class: decl.class,
                });
            }
            Some(o) if o.class != decl.class || o.file != decl.file || o.group != decl.group => {
                esc.insert(Escalation::SecretClassChanged { secret: name.clone() });
            }
            _ => {}
        }
    }

    // Grants expanded to individual secrets: a secret added to an already granted group is a
    // new grant too, so a definition cannot grow its own access unnoticed.
    let grants = |d: &Definitions| -> BTreeSet<(String, String, String)> {
        let mut g = BTreeSet::new();
        let mut add = |groups: &[String], extra: &[String], to: String| {
            for s in d.secrets_for(groups, extra) {
                g.insert((s.name.clone(), s.group.clone(), to.clone()));
            }
        };
        for a in d.agents.values() {
            add(&a.spec.secrets, &[], format!("agent `{}`", a.name));
        }
        for j in d.jobs.values() {
            add(&j.spec.secrets, &[], format!("job `{}`", j.name));
        }
        for (r, route) in &d.instance.models.routes {
            if let Some(s) = &route.api_key_secret {
                add(&[], std::slice::from_ref(s), format!("model route `{r}`"));
            }
        }
        g
    };
    let old_grants = grants(old);
    for (secret, group, to) in grants(new).difference(&old_grants) {
        esc.insert(Escalation::SecretGranted { secret: secret.clone(), group: group.clone(), to: to.clone() });
    }

    for a in new.agents.values() {
        let prev = old.agents.get(&a.name);
        if a.spec.network == Network::Default && prev.is_some_and(|p| p.spec.network == Network::None) {
            esc.insert(Escalation::NetworkOpened { agent: a.name.clone() });
        }
        for c in &a.spec.capabilities {
            if !prev.is_some_and(|p| p.spec.capabilities.contains(c)) {
                esc.insert(Escalation::CapabilityAdded { agent: a.name.clone(), capability: c.as_str().into() });
            }
        }
        for m in &a.spec.mcp {
            if !prev.is_some_and(|p| p.spec.mcp.contains(m)) {
                esc.insert(Escalation::McpAccessAdded { agent: a.name.clone(), service: m.clone() });
            }
        }
        for (op, lim) in &a.spec.limits {
            let baseline = prev.and_then(|p| p.spec.limits.get(op)).copied().unwrap_or_else(|| default_limit(op));
            if !lim.is_at_most(&baseline) {
                esc.insert(Escalation::LimitRaised { owner: format!("agent `{}`", a.name), operation: op.clone() });
            }
        }
    }
    for j in new.jobs.values() {
        let prev = old.jobs.get(&j.name);
        if j.spec.lifetime == Lifetime::Service && !prev.is_some_and(|p| p.spec.lifetime == Lifetime::Service) {
            esc.insert(Escalation::ServiceAdded { job: j.name.clone() });
        }
        for (op, lim) in &j.spec.limits {
            let baseline = prev.and_then(|p| p.spec.limits.get(op)).copied().unwrap_or_else(|| default_limit(op));
            if !lim.is_at_most(&baseline) {
                esc.insert(Escalation::LimitRaised { owner: format!("job `{}`", j.name), operation: op.clone() });
            }
        }
    }

    for t in new.tools.values() {
        let prev = old.tools.get(&t.name);
        for e in &t.effects {
            let sensitive = e == "outbound" || e.starts_with("secret:") || e == "fs_write_outside";
            if sensitive && !prev.is_some_and(|p| p.effects.contains(e)) {
                esc.insert(Escalation::ToolEffect { tool: t.name.clone(), effect: e.clone() });
            }
        }
        if prev.is_some_and(|p| p.approval) && !t.approval {
            esc.insert(Escalation::ApprovalRemoved { tool: t.name.clone() });
        }
    }

    let old_hooks: BTreeSet<_> = webhooks(old).collect();
    for h in webhooks(new) {
        if !old_hooks.contains(&h) {
            esc.insert(Escalation::WebhookAdded { name: h });
        }
    }

    let mut items = Vec::new();
    diff_items(&mut items, "agent", &old.agents, &new.agents);
    diff_items(&mut items, "job", &old.jobs, &new.jobs);
    diff_items(&mut items, "secret", &old.secrets, &new.secrets);
    diff_items(&mut items, "tool", &old.tools, &new.tools);
    diff_items(&mut items, "event type", &old.events, &new.events);

    let mut triggers = Vec::new();
    for t in new.triggers.values() {
        let status = match old.triggers.get(&t.id) {
            None => Status::Added,
            Some(o) if o != t => Status::Changed,
            Some(_) => Status::Unchanged,
        };
        triggers.push(describe_trigger(t, status, now));
    }
    for t in old.triggers.values() {
        if !new.triggers.contains_key(&t.id) {
            triggers.push(describe_trigger(t, Status::Removed, now));
        }
    }

    ChangeSummary { items, triggers, escalations: esc.into_iter().collect() }
}

fn webhooks(d: &Definitions) -> impl Iterator<Item = String> + '_ {
    d.triggers.values().filter_map(|t| match &t.kind {
        TriggerKind::Webhook { name } => Some(name.clone()),
        _ => None,
    })
}

fn diff_items<T: PartialEq>(
    out: &mut Vec<ItemChange>,
    kind: &'static str,
    old: &BTreeMap<String, T>,
    new: &BTreeMap<String, T>,
) {
    for (name, v) in new {
        let status = match old.get(name) {
            None => Status::Added,
            Some(o) if o != v => Status::Changed,
            Some(_) => continue,
        };
        out.push(ItemChange { kind, name: name.clone(), status });
    }
    for name in old.keys() {
        if !new.contains_key(name) {
            out.push(ItemChange { kind, name: name.clone(), status: Status::Removed });
        }
    }
}

fn describe_target(t: &Target) -> String {
    match t {
        Target::Resume(n) => format!("message the `{n}` agent"),
        Target::Spawn(n) => format!("start a new `{n}` agent"),
        Target::Run(n) => format!("run job `{n}`"),
    }
}

pub fn describe_trigger(t: &ResolvedTrigger, status: Status, now: DateTime<Utc>) -> TriggerPreview {
    let (when, upcoming) = match &t.kind {
        TriggerKind::Schedule(s) => {
            (s.describe(), if status == Status::Removed { vec![] } else { s.upcoming(now, UPCOMING) })
        }
        TriggerKind::On { pattern } => {
            let mut w = format!("When an event of type `{pattern}` arrives");
            if !t.spec.filter.is_empty() {
                let conds: Vec<String> = t.spec.filter.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                w.push_str(&format!(" where {}", conds.join(" and ")));
            }
            (w, vec![])
        }
        TriggerKind::Webhook { name } => (format!("When `/hooks/{name}` is called"), vec![]),
    };
    let mut then = describe_target(&t.target);
    if let Some(p) = &t.spec.prompt {
        let short: String = p.chars().take(80).collect();
        then.push_str(&format!(" with \"{short}{}\"", if p.chars().count() > 80 { "…" } else { "" }));
    }
    TriggerPreview { id: t.id.clone(), status, when, then, upcoming, shadow: t.spec.mode == crate::defs::Mode::Shadow }
}

impl ChangeSummary {
    /// Markdown rendering for agents (tool results) and the approval dialog.
    pub fn to_markdown(&self, tz: chrono_tz::Tz) -> String {
        let mut s = String::new();
        if self.escalations.is_empty() {
            s.push_str("**Approval:** no capability changes; the reviewer can approve this alone.\n\n");
        } else {
            s.push_str("**Needs the user's approval because it:**\n");
            for e in &self.escalations {
                s.push_str(&format!("- {}\n", e.explain()));
            }
            s.push('\n');
        }
        let changed: Vec<_> = self.items.iter().collect();
        if !changed.is_empty() {
            s.push_str("**Changes:**\n");
            for i in changed {
                s.push_str(&format!("- {:?} {} `{}`\n", i.status, i.kind, i.name));
            }
            s.push('\n');
        }
        let shown: Vec<_> = self.triggers.iter().filter(|t| t.status != Status::Unchanged).collect();
        if !shown.is_empty() {
            s.push_str("**Triggers:**\n");
            for t in shown {
                s.push_str(&format!("- {:?} `{}`: {}, {}", t.status, t.id, t.when, t.then));
                if t.shadow {
                    s.push_str(" *(shadow mode: recorded, not executed)*");
                }
                s.push('\n');
                if !t.upcoming.is_empty() {
                    let times: Vec<String> = t
                        .upcoming
                        .iter()
                        .map(|u| u.with_timezone(&tz).format("%a %Y-%m-%d %H:%M %Z").to_string())
                        .collect();
                    s.push_str(&format!("  - next: {}\n", times.join(", ")));
                }
            }
        }
        if s.trim().is_empty() { "No effective changes.".into() } else { s }
    }
}

/// Reviewer verdict on a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approve,
    /// Approve, but the reviewer wants the user to look.
    Escalate,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Merge,
    AskUser,
    Reject,
}

/// The transaction decision table. Gate failures never reach this point.
pub fn decide(summary: &ChangeSummary, verdict: Verdict) -> Decision {
    match verdict {
        Verdict::Reject => Decision::Reject,
        Verdict::Escalate => Decision::AskUser,
        Verdict::Approve if summary.escalations.is_empty() => Decision::Merge,
        Verdict::Approve => Decision::AskUser,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::defs::{MemTree, load};

    fn base() -> MemTree {
        MemTree::new()
            .with("botler.toml", "timezone = \"Europe/Berlin\"\n[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"openrouter\"\nmodel = \"x\"\n")
            .with("images/agent/Dockerfile", "FROM x")
            .with("secrets.toml", "[[group]]\nname = \"gitlab\"\nclass = \"container\"\n[[group.secret]]\nname = \"GITLAB_TOKEN\"\n")
            .with("agents/main/agent.toml", "kind = \"persistent\"\n")
            .with("agents/main/AGENTS.md", "hi")
            .with("agents/triage/agent.toml", "")
            .with("agents/triage/AGENTS.md", "hi")
    }

    fn defs(t: &MemTree) -> Definitions {
        load(t).into_result().unwrap_or_else(|d| panic!("{d:#?}"))
    }

    #[test]
    fn new_cron_trigger_is_described_and_autonomous() {
        let old = defs(&base());
        let new_tree = base().with(
            "triggers/digest.toml",
            "[[trigger]]\nname = \"digest\"\ncron = \"0 7 * * 1-5\"\ntarget = { resume = \"main\" }\nprompt = \"Write my daily digest\"\n",
        );
        let new = defs(&new_tree);
        let s = preview(&old, &new, &["triggers/digest.toml".into()], Utc::now());
        assert!(s.escalations.is_empty(), "{:?}", s.escalations);
        assert_eq!(decide(&s, Verdict::Approve), Decision::Merge);
        let t = &s.triggers[0];
        assert_eq!(t.status, Status::Added);
        assert_eq!(t.upcoming.len(), UPCOMING);
        let md = s.to_markdown(chrono_tz::Europe::Berlin);
        assert!(md.contains("message the `main` agent"), "{md}");
        assert!(md.contains("next:"), "{md}");
    }

    #[test]
    fn secret_grant_and_protected_path_escalate() {
        let old = defs(&base());
        let new_tree = base().with("agents/main/agent.toml", "kind = \"persistent\"\nsecrets = [\"gitlab\"]\n");
        let new = defs(&new_tree);
        let s = preview(&old, &new, &["agents/main/agent.toml".into(), "policies/reviewer.md".into()], Utc::now());
        assert!(s.escalations.contains(&Escalation::SecretGranted {
            secret: "GITLAB_TOKEN".into(),
            group: "gitlab".into(),
            to: "agent `main`".into()
        }));
        assert!(s.escalations.contains(&Escalation::ProtectedPath { path: "policies/reviewer.md".into() }));
        assert_eq!(decide(&s, Verdict::Approve), Decision::AskUser);
        assert_eq!(decide(&s, Verdict::Reject), Decision::Reject);
    }

    #[test]
    fn growing_a_granted_group_is_a_new_grant() {
        let granted = base().with("agents/main/agent.toml", "kind = \"persistent\"\nsecrets = [\"gitlab\"]\n");
        let old = defs(&granted);
        let grown = granted.clone().with(
            "secrets.toml",
            "[[group]]\nname = \"gitlab\"\nclass = \"container\"\n[[group.secret]]\nname = \"GITLAB_TOKEN\"\n[[group.secret]]\nname = \"GITLAB_ADMIN_TOKEN\"\n",
        );
        let s = preview(&old, &defs(&grown), &[], Utc::now());
        assert!(
            s.escalations
                .iter()
                .any(|e| matches!(e, Escalation::SecretGranted { secret, .. } if secret == "GITLAB_ADMIN_TOKEN")),
            "{:?}",
            s.escalations
        );
    }

    #[test]
    fn unprotecting_is_still_protected() {
        let old = defs(&base());
        let new = defs(&base().with("policies/capabilities.toml", "protected_paths = []\n"));
        let s = preview(&old, &new, &["policies/capabilities.toml".into()], Utc::now());
        assert!(!s.escalations.is_empty());
    }

    #[test]
    fn raised_limit_escalates_tightened_does_not() {
        let old = defs(&base());
        let tight =
            defs(&base().with(
                "agents/main/agent.toml",
                "kind = \"persistent\"\n[limits.send_message]\nburst = 5\nper = \"1m\"\n",
            ));
        assert!(preview(&old, &tight, &[], Utc::now()).escalations.is_empty());
        let loose = defs(&base().with(
            "agents/main/agent.toml",
            "kind = \"persistent\"\n[limits.send_message]\nburst = 500\nper = \"1m\"\n",
        ));
        assert!(!preview(&old, &loose, &[], Utc::now()).escalations.is_empty());
    }
}
