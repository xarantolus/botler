//! Change previews: the capability diff between two commits, a human-readable deployment summary,
//! and the transaction decision table.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::defs::{
    CapabilityPolicy, Definitions, EnvEscalation, Environment, GrantMode, Lifetime, ResolvedTrigger, SecretClass,
    Target, TriggerKind,
};
use crate::limits::default_limit;

/// Why a change needs the user's approval (never auto-merged).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize, ts_rs::TS)]
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
    /// A service port is newly published on the host (or its mapping changed).
    PortPublished {
        job: String,
        /// e.g. `your network at 0.0.0.0:8080`.
        reach: String,
    },
    /// An agent gets a new (or changed) stdio MCP server: code that runs in its container.
    McpStdioAdded {
        agent: String,
        server: String,
        command: String,
    },
    /// `policies/grants.toml` lets chats get a resource without asking the user.
    GrantAuto {
        resource: String,
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
            Self::PortPublished { job, reach } => format!("makes service `{job}` reachable from {reach}"),
            Self::McpStdioAdded { agent, server, command } => {
                format!("runs MCP server `{server}` (`{command}`) inside agent `{agent}`'s container")
            }
            Self::GrantAuto { resource } => format!("lets any chat get `{resource}` without asking you"),
            Self::InvalidPolicy { detail } => {
                format!("cannot evaluate protected paths ({detail}); treating all paths as protected")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Added,
    Removed,
    Changed,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
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
    /// For changed triggers: what it was before.
    #[serde(default)]
    pub before: Option<Before>,
}

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
pub struct Before {
    pub when: String,
    pub then: String,
}

/// Something that runs continuously (a service, an always-on agent) starting or stopping.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
pub struct Activity {
    pub status: Status,
    pub what: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
pub struct ItemChange {
    pub kind: String,
    pub name: String,
    pub status: Status,
}

/// The full preview of applying `new` on top of `old`.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize, ts_rs::TS)]
pub struct ChangeSummary {
    pub items: Vec<ItemChange>,
    #[serde(default)]
    pub activities: Vec<Activity>,
    /// Plain-language warnings about likely mistakes.
    #[serde(default)]
    pub checks: Vec<String>,
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
            add(&a.spec.env.secrets, &[], format!("agent `{}`", a.name));
        }
        for j in d.jobs.values() {
            add(&j.spec.env.secrets, &[], format!("job `{}`", j.name));
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

    // Secret grants are compared per secret above; tools and homes are no escalation (yet).
    let none = Environment::default();
    for a in new.agents.values() {
        let prev = old.agents.get(&a.name);
        for e in prev.map_or(&none, |p| &p.spec.env).escalations(&a.spec.env) {
            let agent = a.name.clone();
            esc.insert(match e {
                EnvEscalation::Network => Escalation::NetworkOpened { agent },
                EnvEscalation::Service(service) => Escalation::McpAccessAdded { agent, service },
                EnvEscalation::McpStdio { server, command } => Escalation::McpStdioAdded { agent, server, command },
                _ => continue,
            });
        }
        // Spawning is a base capability; listing it changes nothing.
        for c in a.spec.capabilities.iter().filter(|c| **c != crate::defs::Capability::Spawn) {
            if !prev.is_some_and(|p| p.spec.capabilities.contains(c)) {
                esc.insert(Escalation::CapabilityAdded { agent: a.name.clone(), capability: c.as_str().into() });
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
        if j.spec.lifetime == Lifetime::Service {
            let before = prev.filter(|o| o.spec.lifetime == Lifetime::Service).map_or(&none, |o| &o.spec.env);
            for e in before.escalations(&j.spec.env) {
                if let EnvEscalation::Port(p) = e {
                    esc.insert(Escalation::PortPublished { job: j.name.clone(), reach: p.reach() });
                }
            }
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

    for (resource, mode) in &new.grant_policy.resources {
        if *mode == GrantMode::Auto && old.grant_policy.resources.get(resource) != Some(&GrantMode::Auto) {
            esc.insert(Escalation::GrantAuto { resource: resource.clone() });
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
        let (status, before) = match old.triggers.get(&t.id) {
            None => (Status::Added, None),
            Some(o) if o != t => {
                let was = describe_trigger(o, Status::Removed, now);
                (Status::Changed, Some(Before { when: was.when, then: was.then }))
            }
            Some(_) => (Status::Unchanged, None),
        };
        let mut preview = describe_trigger(t, status, now);
        preview.before = before;
        triggers.push(preview);
    }
    for t in old.triggers.values() {
        if !new.triggers.contains_key(&t.id) {
            triggers.push(describe_trigger(t, Status::Removed, now));
        }
    }

    let mut activities = Vec::new();
    // Key -> (description, the definition it runs: a change restarts it).
    let running = |d: &Definitions| -> BTreeMap<String, (String, String)> {
        let mut m = BTreeMap::new();
        for j in d.services() {
            let mut what = format!("service `{}` runs continuously ({})", j.name, j.spec.description);
            if let Some(p) = j.spec.env.port {
                what.push_str(&format!(", on port {p} inside the services network"));
            }
            for p in &j.spec.env.publish {
                what.push_str(&format!(", reachable from {} (container port {})", p.reach(), p.port));
            }
            m.insert(format!("service:{}", j.name), (what, format!("{:?}", j.spec)));
        }
        for a in d.persistent_agents().filter(|a| a.spec.always_on) {
            m.insert(
                format!("agent:{}", a.name),
                (format!("agent `{}` stays awake at all times", a.name), String::new()),
            );
        }
        m
    };
    let (was, will) = (running(old), running(new));
    for (k, (what, def)) in &will {
        match was.get(k) {
            None => activities.push(Activity { status: Status::Added, what: what.clone() }),
            Some((_, old_def)) if old_def != def => activities
                .push(Activity { status: Status::Changed, what: format!("{what}; restarted with the new definition") }),
            Some(_) => {}
        }
    }
    for (k, (what, _)) in &was {
        if !will.contains_key(k) {
            activities.push(Activity { status: Status::Removed, what: what.clone() });
        }
    }

    let checks = likely_mistakes(old, new, now);
    ChangeSummary { items, activities, checks, triggers, escalations: esc.into_iter().collect() }
}

/// Things that are valid but probably not what the author meant.
fn likely_mistakes(old: &Definitions, new: &Definitions, now: DateTime<Utc>) -> Vec<String> {
    let mut out = Vec::new();
    let day = now + chrono::Duration::days(1);
    for t in new.triggers.values() {
        let TriggerKind::Schedule(spec) = &t.kind else { continue };
        let unchanged = old.triggers.get(&t.id) == Some(t);
        if unchanged {
            continue;
        }
        // Bounded: a per-second cron would otherwise enumerate 86400 times.
        const CAP: usize = 3000;
        let fires = spec.upcoming(now, CAP).into_iter().take_while(|f| *f <= day).count();
        if fires > 24 {
            let every = 24 * 60 / fires.max(1);
            out.push(format!(
                "`{}` fires {}{} times in the next 24 hours (about every {} min); is that intended?",
                t.id,
                if fires >= CAP { "at least " } else { "" },
                fires,
                every.max(1)
            ));
        }
        if spec.upcoming(now, 1).first().is_none_or(|f| *f > now + chrono::Duration::days(30)) {
            out.push(format!("`{}` does not fire in the next 30 days.", t.id));
        }
    }
    for kind in old.events.keys() {
        if new.events.contains_key(kind) {
            continue;
        }
        let listeners: Vec<&str> = new
            .triggers
            .values()
            .filter(|t| matches!(&t.kind, TriggerKind::On { pattern } if crate::routing::matches_type(pattern, kind)))
            .map(|t| t.id.as_str())
            .collect();
        if !listeners.is_empty() {
            out.push(format!(
                "event type `{kind}` is removed but still listened to by {}; emitting it will now be rejected.",
                listeners.join(", ")
            ));
        }
    }
    for t in new.triggers.values() {
        if let TriggerKind::On { pattern } = &t.kind
            && !new.events.keys().any(|k| crate::routing::matches_type(pattern, k))
            && !crate::catalog::BUILTIN.iter().any(|b| crate::routing::matches_type(pattern, b.kind))
            && old.triggers.get(&t.id) != Some(t)
        {
            out.push(format!("`{}` listens for `{pattern}`, which matches no declared event type.", t.id));
        }
    }
    for j in new.jobs.values().filter(|j| j.spec.lifetime == crate::defs::Lifetime::Oneshot) {
        let ran_before = old.triggers.values().any(|t| t.target == crate::defs::Target::Run(j.name.clone()));
        let runs_now = new.triggers.values().any(|t| t.target == crate::defs::Target::Run(j.name.clone()))
            || new.jobs.values().any(|o| o.spec.on_failure == Some(crate::defs::Target::Run(j.name.clone())));
        if ran_before && !runs_now {
            out.push(format!("job `{}` is no longer run by any trigger (it still exists).", j.name));
        }
    }
    for a in new.agents.values() {
        let before = old.agents.get(&a.name).map(|p| p.spec.env.home.as_deref());
        if before.is_none_or(|h| h == a.spec.env.home.as_deref()) {
            continue;
        }
        match &a.spec.env.home {
            Some(home) => {
                let others: Vec<&str> = new
                    .agents
                    .values()
                    .filter(|o| o.name != a.name && o.spec.env.home.as_deref() == Some(home.as_str()))
                    .map(|o| o.name.as_str())
                    .collect();
                out.push(format!(
                    "agent `{}` switches to the shared home `{home}`{}; files in its previous home are not moved.",
                    a.name,
                    if others.is_empty() { String::new() } else { format!(" (shared with {})", others.join(", ")) }
                ));
            }
            None => out.push(format!(
                "agent `{}` stops using a shared home: each instance gets its own; the shared files are not moved.",
                a.name
            )),
        }
    }
    out
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
        out.push(ItemChange { kind: kind.to_string(), name: name.clone(), status });
    }
    for name in old.keys() {
        if !new.contains_key(name) {
            out.push(ItemChange { kind: kind.to_string(), name: name.clone(), status: Status::Removed });
        }
    }
}

pub fn describe_target(t: &Target) -> String {
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
    TriggerPreview {
        id: t.id.clone(),
        status,
        when,
        then,
        upcoming,
        shadow: t.spec.mode == crate::defs::Mode::Shadow,
        before: None,
    }
}

impl ChangeSummary {
    /// Markdown rendering for agents (tool results) and the approval dialog.
    pub fn to_markdown(&self, tz: chrono_tz::Tz) -> String {
        let fmt_times = |ts: &[DateTime<Utc>]| -> String {
            ts.iter()
                .map(|u| u.with_timezone(&tz).format("%a %Y-%m-%d %H:%M %Z").to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let trigger_line = |t: &TriggerPreview| -> String {
            let mut l = format!("- {}: {} *(`{}`)*", t.when, t.then, t.id);
            if t.shadow {
                l.push_str(" *(shadow mode: recorded, not executed)*");
            }
            l.push('\n');
            if !t.upcoming.is_empty() {
                l.push_str(&format!("  - next: {}\n", fmt_times(&t.upcoming)));
            }
            l
        };
        let mut s = String::new();
        if !self.checks.is_empty() {
            s.push_str("**Check this (likely mistakes):**\n");
            for c in &self.checks {
                s.push_str(&format!("- {c}\n"));
            }
            s.push('\n');
        }
        if self.escalations.is_empty() {
            s.push_str("**Approval:** no capability changes; the reviewer can approve this alone.\n\n");
        } else {
            s.push_str("**Needs the user's approval because it:**\n");
            for e in &self.escalations {
                s.push_str(&format!("- {}\n", e.explain()));
            }
            s.push('\n');
        }

        let starts: Vec<String> = self
            .triggers
            .iter()
            .filter(|t| t.status == Status::Added)
            .map(trigger_line)
            .chain(self.activities.iter().filter(|a| a.status == Status::Added).map(|a| format!("- {}\n", a.what)))
            .collect();
        if !starts.is_empty() {
            s.push_str("**Will start happening:**\n");
            s.extend(starts);
            s.push('\n');
        }
        let stops: Vec<String> = self
            .triggers
            .iter()
            .filter(|t| t.status == Status::Removed)
            .map(|t| format!("- {}: {} *(`{}`)*\n", t.when, t.then, t.id))
            .chain(self.activities.iter().filter(|a| a.status == Status::Removed).map(|a| format!("- {}\n", a.what)))
            .collect();
        if !stops.is_empty() {
            s.push_str("**Will stop happening:**\n");
            s.extend(stops);
            s.push('\n');
        }
        let changed: Vec<&TriggerPreview> = self.triggers.iter().filter(|t| t.status == Status::Changed).collect();
        let changed_activities: Vec<&Activity> =
            self.activities.iter().filter(|a| a.status == Status::Changed).collect();
        if !changed.is_empty() || !changed_activities.is_empty() {
            s.push_str("**Will change:**\n");
            for a in changed_activities {
                s.push_str(&format!("- {}\n", a.what));
            }
            for t in changed {
                match &t.before {
                    Some(b) => s.push_str(&format!(
                        "- `{}`: was \"{}: {}\", becomes \"{}: {}\"\n",
                        t.id, b.when, b.then, t.when, t.then
                    )),
                    None => s.push_str(&format!("- `{}`: {}: {}\n", t.id, t.when, t.then)),
                }
                if !t.upcoming.is_empty() {
                    s.push_str(&format!("  - next: {}\n", fmt_times(&t.upcoming)));
                }
            }
            s.push('\n');
        }
        if !self.items.is_empty() {
            s.push_str("**Definitions:**\n");
            for (status, label) in
                [(Status::Added, "added"), (Status::Removed, "removed"), (Status::Changed, "changed")]
            {
                let names: Vec<String> = self
                    .items
                    .iter()
                    .filter(|i| i.status == status)
                    .map(|i| format!("{} `{}`", i.kind, i.name))
                    .collect();
                if !names.is_empty() {
                    s.push_str(&format!("- {label}: {}\n", names.join(", ")));
                }
            }
        }
        if s.trim().is_empty() { "No effective changes.".into() } else { s }
    }
}

/// Reviewer verdict on a proposal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema, ts_rs::TS)]
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
        assert!(md.contains("**Will start happening:**") && md.contains("message the `main` agent"), "{md}");
        assert!(md.contains("next:"), "{md}");
    }

    fn service(port: u16, host_port: u16) -> String {
        format!(
            "lifetime = \"service\"\ncommand = [\"python\", \"-m\", \"app\"]\nport = {port}\n[[publish]]\nport = {port}\nhost_port = {host_port}\n"
        )
    }

    #[test]
    fn published_ports_and_stdio_mcp_escalate_and_show_up() {
        let with_svc = |host_port: u16| {
            base().with("jobs/app/job.toml", service(8000, host_port)).with("jobs/app/Dockerfile", "FROM x").with(
                "agents/main/agent.toml",
                "kind = \"persistent\"\nmcp = [\"app\"]\n[mcp_stdio.fs]\ncommand = [\"npx\", \"fs-mcp\"]\n",
            )
        };
        let (old, new) = (defs(&base()), defs(&with_svc(8080)));
        let s = preview(&old, &new, &[], Utc::now());
        let reach = "your network at 0.0.0.0:8080".to_string();
        assert!(s.escalations.contains(&Escalation::PortPublished { job: "app".into(), reach: reach.clone() }));
        assert!(s.escalations.contains(&Escalation::McpAccessAdded { agent: "main".into(), service: "app".into() }));
        assert!(s.escalations.contains(&Escalation::McpStdioAdded {
            agent: "main".into(),
            server: "fs".into(),
            command: "npx fs-mcp".into()
        }));
        let md = s.to_markdown(chrono_tz::Europe::Berlin);
        assert!(md.contains("reachable from your network at 0.0.0.0:8080"), "{md}");
        assert!(
            md.contains("**Will start happening:**") && md.contains("on port 8000 inside the services network"),
            "{md}"
        );

        // Moving the port is a new exposure and restarts the service; an identical tree is quiet.
        let s = preview(&new, &defs(&with_svc(8081)), &[], Utc::now());
        assert_eq!(s.escalations.len(), 1, "{:?}", s.escalations);
        assert!(s.to_markdown(chrono_tz::Europe::Berlin).contains("**Will change:**\n- service `app`"));
        assert!(preview(&new, &defs(&with_svc(8080)), &[], Utc::now()).escalations.is_empty());
    }

    #[test]
    fn service_ports_are_validated() {
        let errors = |t: MemTree| -> Vec<String> {
            load(&t)
                .diagnostics
                .into_iter()
                .filter(|d| d.severity == crate::defs::Severity::Error)
                .map(|d| d.message)
                .collect()
        };
        let t = base()
            .with("jobs/a/job.toml", service(8000, 80))
            .with("jobs/a/Dockerfile", "FROM x")
            .with("jobs/b/job.toml", service(8000, 80))
            .with("jobs/b/Dockerfile", "FROM x")
            .with("jobs/c/job.toml", "command = [\"x\"]\nport = 1\n")
            .with("jobs/c/Dockerfile", "FROM x")
            .with("jobs/d/job.toml", "lifetime = \"service\"\ncommand = [\"x\"]\n")
            .with("jobs/d/Dockerfile", "FROM x")
            .with("agents/main/agent.toml", "kind = \"persistent\"\nmcp = [\"d\"]\nnetwork = \"none\"\n");
        let e = errors(t).join("\n");
        assert!(e.contains("below 1024"), "{e}");
        assert!(e.contains("host port 80 is already published by service `a`"), "{e}");
        assert!(e.contains("only apply to services"), "{e}");
        assert!(e.contains("declares no `port`"), "{e}");
        assert!(e.contains("network = \"none\""), "{e}");
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
    fn changed_and_removed_schedules_are_described() {
        let with = |cron: &str| {
            base().with(
                "triggers/d.toml",
                format!("[[trigger]]\nname = \"d\"\ncron = \"{cron}\"\ntarget = {{ resume = \"main\" }}\n[[trigger]]\nname = \"gone\"\nevery = \"1h\"\ntarget = {{ resume = \"main\" }}\n"),
            )
        };
        let old = defs(&with("0 7 * * 1-5"));
        let new = defs(&base().with(
            "triggers/d.toml",
            "[[trigger]]\nname = \"d\"\ncron = \"30 6 * * *\"\ntarget = { resume = \"main\" }\n",
        ));
        let md = preview(&old, &new, &[], Utc::now()).to_markdown(chrono_tz::UTC);
        assert!(
            md.contains("**Will change:**") && md.contains("was \"At 07:00") && md.contains("becomes \"At 06:30"),
            "{md}"
        );
        assert!(md.contains("**Will stop happening:**") && md.contains("Every 1h"), "{md}");
    }

    #[test]
    fn likely_mistakes_are_called_out() {
        let old = defs(&base().with("events/e.toml", "[[event]]\ntype = \"mail.received\"\n").with(
            "triggers/t.toml",
            "[[trigger]]\nname = \"m\"\non = \"mail.received\"\ntarget = { resume = \"main\" }\n",
        ));
        let new = defs(
            &base()
                .with("triggers/t.toml", "[[trigger]]\nname = \"m\"\non = \"mail.received\"\ntarget = { resume = \"main\" }\n[[trigger]]\nname = \"spam\"\ncron = \"* * * * *\"\ntarget = { resume = \"main\" }\n[[trigger]]\nname = \"typo\"\non = \"mial.*\"\ntarget = { resume = \"main\" }\n"),
        );
        let md = preview(&old, &new, &[], Utc::now()).to_markdown(chrono_tz::UTC);
        assert!(md.contains("Check this") && md.contains("fires 1440 times"), "{md}");
        assert!(md.contains("event type `mail.received` is removed but still listened to"), "{md}");
        assert!(md.contains("listens for `mial.*`, which matches no declared event type"), "{md}");
    }

    #[test]
    fn switching_homes_is_called_out() {
        let old = defs(&base());
        let new = defs(
            &base()
                .with("agents/main/agent.toml", "kind = \"persistent\"\nhome = \"station\"\n")
                .with("agents/triage/agent.toml", "home = \"station\"\n"),
        );
        let md = preview(&old, &new, &[], Utc::now()).to_markdown(chrono_tz::UTC);
        assert!(md.contains("agent `main` switches to the shared home `station` (shared with triage)"), "{md}");
        let back = preview(&new, &old, &[], Utc::now()).to_markdown(chrono_tz::UTC);
        assert!(back.contains("agent `main` stops using a shared home"), "{back}");
    }

    #[test]
    fn renamed_scratch_key_names_its_replacement() {
        let r = load(&base().with("agents/main/agent.toml", "kind = \"persistent\"\nscratch = \"x\"\n"));
        assert!(r.definitions.is_none());
        assert!(
            r.diagnostics.iter().any(|d| d.path == "agents/main/agent.toml" && d.message.contains("renamed to `home`")),
            "{:?}",
            r.diagnostics
        );
        let bad = load(&base().with("agents/main/agent.toml", "kind = \"persistent\"\nhome = \"Bad Name\"\n"));
        assert!(bad.diagnostics.iter().any(|d| d.message.contains("home name")), "{:?}", bad.diagnostics);
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

    #[test]
    fn auto_grant_policy_escalates() {
        let old = defs(&base());
        let new = defs(
            &base().with("policies/grants.toml", "[resources]\n\"secret:nas\" = \"auto\"\n\"secret:x\" = \"ask\"\n"),
        );
        let s = preview(&old, &new, &["policies/grants.toml".into()], Utc::now());
        assert!(s.escalations.contains(&Escalation::GrantAuto { resource: "secret:nas".into() }), "{s:?}");
        assert!(
            !s.escalations.iter().any(|e| matches!(e, Escalation::GrantAuto { resource } if resource == "secret:x"))
        );
        let t = base().with("policies/grants.toml", "[resources]\n\"disk:x\" = \"auto\"\n");
        let bad: Vec<String> = load(&t).errors().map(|d| d.message.clone()).collect();
        assert!(bad.iter().any(|m| m.contains("unknown resource kind")), "{bad:?}");
    }
}
