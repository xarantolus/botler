//! Loading and validating a monorepo tree into [`Definitions`].
//!
//! Loading never stops at the first problem: it collects every diagnostic so an agent gets the
//! whole list of what to fix in one gate run.

mod model;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use model::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use tree::{FsTree, MemTree, Tree, TreeEntry};

use crate::trigger::ScheduleSpec;

/// A trigger with its owner and target resolved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedTrigger {
    /// Stable id: `<file path>#<name>`, e.g. `agents/main/agent.toml#morning`.
    pub id: String,
    pub spec: Trigger,
    pub kind: TriggerKind,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum TriggerKind {
    Schedule(ScheduleSpec),
    On { pattern: String },
    Webhook { name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AgentDef {
    pub name: String,
    pub spec: Agent,
    /// Contents of `AGENTS.md` next to `agent.toml`, if present.
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct JobDef {
    pub name: String,
    pub spec: Job,
}

/// Everything the hub applies from one commit of the monorepo.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Definitions {
    pub instance: Instance,
    /// `instance.timezone`, parsed at load.
    #[serde(skip)]
    pub timezone: chrono_tz::Tz,
    pub agents: BTreeMap<String, AgentDef>,
    pub jobs: BTreeMap<String, JobDef>,
    pub triggers: BTreeMap<String, ResolvedTrigger>,
    pub secrets: BTreeMap<String, SecretDecl>,
    pub events: BTreeMap<String, EventDecl>,
    pub tools: BTreeMap<String, ToolDecl>,
    pub images: BTreeSet<String>,
    pub policy: CapabilityPolicy,
}

impl Definitions {
    pub fn services(&self) -> impl Iterator<Item = &JobDef> {
        self.jobs.values().filter(|j| j.spec.lifetime == Lifetime::Service)
    }

    pub fn persistent_agents(&self) -> impl Iterator<Item = &AgentDef> {
        self.agents.values().filter(|a| a.spec.kind == AgentKind::Persistent)
    }

    /// The model route an agent resolves to, if any.
    pub fn route_for(&self, agent: &Agent) -> Option<(&str, &ModelRoute)> {
        let name = agent.model.as_deref().or(self.instance.models.default.as_deref())?;
        self.instance.models.routes.get_key_value(name).map(|(k, v)| (k.as_str(), v))
    }

    /// Where failures of a runnable go.
    pub fn failure_target(&self, job: &str) -> Target {
        self.jobs.get(job).and_then(|j| j.spec.on_failure.clone()).unwrap_or_else(|| Target::Spawn("triage".into()))
    }

    /// Tools granted to an agent by its `tools` globs. Globs are validated at load; if one is
    /// somehow invalid, no tools are granted (fail closed).
    pub fn tools_for(&self, agent: &Agent) -> Vec<&ToolDecl> {
        let Ok(set) = glob_set(&agent.tools) else { return Vec::new() };
        self.tools.values().filter(|t| set.is_match(&t.name)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// One problem found while loading, tied to the file it is in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub path: String,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{sev}: {}: {}", self.path, self.message)
    }
}

/// Result of loading: definitions are present only if there were no errors.
#[derive(Debug)]
pub struct LoadReport {
    pub definitions: Option<Definitions>,
    pub diagnostics: Vec<Diagnostic>,
}

impl LoadReport {
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics.iter().filter(|d| d.severity == Severity::Error)
    }

    pub fn into_result(self) -> Result<Definitions, Vec<Diagnostic>> {
        match self.definitions {
            Some(d) => Ok(d),
            None => Err(self.diagnostics),
        }
    }
}

struct Loader<'a> {
    tree: &'a dyn Tree,
    diags: Vec<Diagnostic>,
}

impl Loader<'_> {
    fn err(&mut self, path: &str, msg: impl Into<String>) {
        self.diags.push(Diagnostic { severity: Severity::Error, path: path.into(), message: msg.into() });
    }

    fn warn(&mut self, path: &str, msg: impl Into<String>) {
        self.diags.push(Diagnostic { severity: Severity::Warning, path: path.into(), message: msg.into() });
    }

    fn read_string(&mut self, path: &str) -> Option<String> {
        match self.tree.read(path) {
            Ok(Some(bytes)) => match String::from_utf8(bytes) {
                Ok(s) => Some(s),
                Err(_) => {
                    self.err(path, "file is not valid UTF-8");
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                self.err(path, format!("could not read: {e}"));
                None
            }
        }
    }

    fn toml<T: DeserializeOwned>(&mut self, path: &str) -> Option<T> {
        let text = self.read_string(path)?;
        match toml::from_str::<T>(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                self.err(path, e.to_string().trim_end().to_string());
                None
            }
        }
    }

    fn list(&mut self, dir: &str) -> Vec<TreeEntry> {
        match self.tree.list(dir) {
            Ok(entries) => entries,
            Err(e) => {
                self.err(dir, format!("could not list: {e}"));
                Vec::new()
            }
        }
    }

    fn check_version(&mut self, path: &str, version: u32) {
        if version != FORMAT_VERSION {
            self.err(path, format!("unsupported format version {version} (hub supports {FORMAT_VERSION})"));
        }
    }

    fn check_name(&mut self, path: &str, what: &str, name: &str) {
        if !valid_name(name) {
            self.err(path, format!("{what} name {name:?} must match [a-z0-9][a-z0-9-]* (max 48 chars)"));
        }
    }
}

/// Names of agents, jobs, triggers and instances.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 48
        && chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn glob_set(patterns: &[String]) -> Result<globset::GlobSet, globset::Error> {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        b.add(globset::GlobBuilder::new(p).literal_separator(false).build()?);
    }
    b.build()
}

/// Load and validate the monorepo at `tree`.
pub fn load(tree: &dyn Tree) -> LoadReport {
    let mut l = Loader { tree, diags: Vec::new() };
    let mut defs = Definitions::default();

    match l.toml::<Instance>("botler.toml") {
        Some(inst) => {
            l.check_version("botler.toml", inst.version);
            defs.instance = inst;
        }
        None if l.diags.is_empty() => l.err("botler.toml", "missing: every monorepo needs a botler.toml"),
        None => {}
    }
    match defs.instance.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => defs.timezone = tz,
        Err(_) => {
            let tz = defs.instance.timezone.clone();
            l.err("botler.toml", format!("unknown time zone {tz:?}"));
        }
    }

    if let Some(p) = l.toml::<CapabilityPolicy>("policies/capabilities.toml") {
        l.check_version("policies/capabilities.toml", p.version);
        for pattern in &p.protected_paths {
            if let Err(e) = globset::Glob::new(pattern) {
                l.err("policies/capabilities.toml", format!("invalid protected path glob {pattern:?}: {e}"));
            }
        }
        defs.policy = p;
    }

    if let Some(s) = l.toml::<SecretsFile>("secrets.toml") {
        l.check_version("secrets.toml", s.version);
        for decl in s.secrets {
            if !decl.name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                || decl.name.is_empty()
            {
                l.err("secrets.toml", format!("secret name {:?} must be UPPER_SNAKE_CASE", decl.name));
            }
            if decl.class == SecretClass::Hub && decl.file.is_some() {
                l.err("secrets.toml", format!("hub secret {} cannot have a container file path", decl.name));
            }
            if defs.secrets.insert(decl.name.clone(), decl.clone()).is_some() {
                l.err("secrets.toml", format!("secret {} declared twice", decl.name));
            }
        }
    }

    for entry in l.list("images") {
        if entry.is_dir {
            let df = format!("images/{}/Dockerfile", entry.name);
            if matches!(tree.read(&df), Ok(Some(_))) {
                defs.images.insert(entry.name);
            } else {
                l.warn(&format!("images/{}", entry.name), "image directory has no Dockerfile");
            }
        }
    }

    for entry in l.list("events") {
        if entry.is_dir || !entry.name.ends_with(".toml") {
            continue;
        }
        let path = format!("events/{}", entry.name);
        if let Some(f) = l.toml::<EventsFile>(&path) {
            l.check_version(&path, f.version);
            for e in f.events {
                if defs.events.insert(e.kind.clone(), e.clone()).is_some() {
                    l.err(&path, format!("event type {} declared twice", e.kind));
                }
            }
        }
    }

    let mut raw_triggers: Vec<(String, Trigger, Option<Target>)> = Vec::new();

    for entry in l.list("agents") {
        if !entry.is_dir {
            continue;
        }
        let name = entry.name;
        let path = format!("agents/{name}/agent.toml");
        l.check_name(&path, "agent", &name);
        let Some(spec) = l.toml::<Agent>(&path) else {
            if matches!(tree.read(&path), Ok(None)) {
                l.err(&format!("agents/{name}"), "agent directory has no agent.toml");
            }
            continue;
        };
        l.check_version(&path, spec.version);
        let prompt = l.read_string(&format!("agents/{name}/AGENTS.md"));
        let own = match spec.kind {
            AgentKind::Persistent => Target::Resume(name.clone()),
            AgentKind::Ephemeral => Target::Spawn(name.clone()),
        };
        for t in &spec.triggers {
            raw_triggers.push((path.clone(), t.clone(), Some(own.clone())));
        }
        defs.agents.insert(name.clone(), AgentDef { name, spec, prompt });
    }

    for entry in l.list("jobs") {
        if !entry.is_dir {
            continue;
        }
        let name = entry.name;
        let path = format!("jobs/{name}/job.toml");
        l.check_name(&path, "job", &name);
        let Some(spec) = l.toml::<Job>(&path) else {
            if matches!(tree.read(&path), Ok(None)) {
                l.err(&format!("jobs/{name}"), "job directory has no job.toml");
            }
            continue;
        };
        l.check_version(&path, spec.version);
        if !matches!(tree.read(&format!("jobs/{name}/Dockerfile")), Ok(Some(_))) {
            l.err(&path, "job has no Dockerfile next to job.toml");
        }
        if spec.command.is_empty() {
            l.err(&path, "command must not be empty");
        }
        if spec.lifetime == Lifetime::Service && !spec.triggers.is_empty() {
            l.err(&path, "a service runs forever and cannot have triggers; use a oneshot job instead");
        }
        if spec.lifetime == Lifetime::Oneshot && spec.health.is_some() {
            l.warn(&path, "health heartbeats only apply to services; ignored for oneshot jobs");
        }
        for t in &spec.triggers {
            raw_triggers.push((path.clone(), t.clone(), Some(Target::Run(name.clone()))));
        }
        defs.jobs.insert(name.clone(), JobDef { name, spec });
    }

    for entry in l.list("triggers") {
        if entry.is_dir || !entry.name.ends_with(".toml") {
            continue;
        }
        let path = format!("triggers/{}", entry.name);
        if let Some(f) = l.toml::<TriggerFile>(&path) {
            l.check_version(&path, f.version);
            for t in f.triggers {
                raw_triggers.push((path.clone(), t, None));
            }
        }
    }

    // Generated from pydantic models by the SDK (`python -m botler.events manifest`).
    match tree.read("events.manifest.json") {
        Ok(Some(bytes)) => match serde_json::from_slice::<Vec<EventDecl>>(&bytes) {
            Ok(events) => {
                for e in events {
                    if defs.events.insert(e.kind.clone(), e.clone()).is_some() {
                        l.err("events.manifest.json", format!("event type {} is declared twice", e.kind));
                    }
                }
            }
            Err(e) => l.err("events.manifest.json", e.to_string()),
        },
        Ok(None) => {}
        Err(e) => l.err("events.manifest.json", format!("could not read: {e}")),
    }
    for e in defs.events.values() {
        if crate::catalog::builtin(&e.kind).is_some() {
            l.err("events", format!("{} is a built-in hub event type and cannot be redeclared", e.kind));
        }
        if let Some(schema) = &e.schema
            && let Err(err) = crate::event_check::compile(schema)
        {
            l.err("events", format!("schema of {} is invalid: {err}", e.kind));
        }
    }

    match tree.read("tools.manifest.json") {
        Ok(Some(bytes)) => match serde_json::from_slice::<Vec<ToolDecl>>(&bytes) {
            Ok(tools) => {
                for t in tools {
                    if defs.tools.insert(t.name.clone(), t.clone()).is_some() {
                        l.err("tools.manifest.json", format!("tool {} declared twice", t.name));
                    }
                }
            }
            Err(e) => l.err("tools.manifest.json", e.to_string()),
        },
        Ok(None) => {}
        Err(e) => l.err("tools.manifest.json", format!("could not read: {e}")),
    }

    // Cross-references, now that every file is loaded.
    validate_models(&mut l, &defs);
    validate_agents(&mut l, &defs);
    validate_jobs(&mut l, &defs);
    for (path, trigger, default_target) in raw_triggers {
        if let Some(rt) = resolve_trigger(&mut l, &defs, &path, trigger, default_target)
            && let Some(prev) = defs.triggers.insert(rt.id.clone(), rt)
        {
            l.err(&path, format!("trigger {} declared twice", prev.spec.name));
        }
    }
    let webhooks: Vec<_> = defs
        .triggers
        .values()
        .filter_map(|t| match &t.kind {
            TriggerKind::Webhook { name } => Some((name.clone(), t.id.clone())),
            _ => None,
        })
        .collect();
    let mut seen = BTreeSet::new();
    for (hook, id) in webhooks {
        if !seen.insert(hook.clone()) {
            l.err(&id, format!("webhook name {hook:?} is used by more than one trigger"));
        }
    }

    l.diags.sort();
    l.diags.dedup();
    let ok = !l.diags.iter().any(|d| d.severity == Severity::Error);
    LoadReport { definitions: ok.then_some(defs), diagnostics: l.diags }
}

fn validate_models(l: &mut Loader<'_>, defs: &Definitions) {
    let models = &defs.instance.models;
    if let Some(d) = &models.default
        && !models.routes.contains_key(d)
    {
        l.err("botler.toml", format!("models.default names unknown route {d:?}"));
    }
    for (name, route) in &models.routes {
        for fb in &route.fallback {
            match models.routes.get(fb) {
                None => l.err("botler.toml", format!("route {name}: fallback {fb:?} does not exist")),
                Some(f) if route.privacy == Privacy::Local && f.privacy != Privacy::Local => l.err(
                    "botler.toml",
                    format!("route {name} is local-only and may not fall back to hosted route {fb}"),
                ),
                _ => {}
            }
        }
        if let Some(s) = &route.api_key_secret {
            check_container_secret(l, defs, "botler.toml", s);
        }
    }
}

fn check_container_secret(l: &mut Loader<'_>, defs: &Definitions, path: &str, name: &str) {
    match defs.secrets.get(name) {
        None => l.err(path, format!("secret {name} is not declared in secrets.toml")),
        Some(d) if d.class == SecretClass::Hub => l.err(
            path,
            format!("secret {name} is hub-class and can never enter a container; use an approval-gated action"),
        ),
        Some(_) => {}
    }
}

fn validate_agents(l: &mut Loader<'_>, defs: &Definitions) {
    for a in defs.agents.values() {
        let path = format!("agents/{}/agent.toml", a.name);
        if !defs.images.contains(&a.spec.image) {
            l.err(&path, format!("image {:?} not found under images/", a.spec.image));
        }
        if let Some(m) = &a.spec.model
            && !defs.instance.models.routes.contains_key(m)
        {
            l.err(&path, format!("model route {m:?} not defined in botler.toml"));
        }
        if defs.route_for(&a.spec).is_none() {
            l.err(&path, "no model route: set `model` or `models.default` in botler.toml");
        }
        for s in &a.spec.secrets {
            check_container_secret(l, defs, &path, s);
        }
        for m in &a.spec.mcp {
            match defs.jobs.get(m) {
                Some(j) if j.spec.lifetime == Lifetime::Service => {}
                Some(_) => l.err(&path, format!("mcp {m:?} is a oneshot job, not a service")),
                None => l.err(&path, format!("mcp {m:?} is not a job in jobs/")),
            }
        }
        if let Err(e) = glob_set(&a.spec.tools) {
            l.err(&path, format!("invalid tools glob: {e}"));
        } else if !a.spec.tools.is_empty() && defs.tools_for(&a.spec).is_empty() {
            l.warn(&path, "tools globs match no tool in tools.manifest.json");
        }
        if a.spec.always_on && a.spec.kind != AgentKind::Persistent {
            l.err(&path, "always_on only applies to persistent agents");
        }
        if a.prompt.is_none() {
            l.warn(&path, "no AGENTS.md next to agent.toml; the agent gets only the core system prompt");
        }
    }
}

fn validate_jobs(l: &mut Loader<'_>, defs: &Definitions) {
    for j in defs.jobs.values() {
        let path = format!("jobs/{}/job.toml", j.name);
        for s in &j.spec.secrets {
            check_container_secret(l, defs, &path, s);
        }
        if let Some(t) = &j.spec.on_failure {
            check_target(l, defs, &path, t);
        }
    }
    if defs.jobs.values().any(|j| j.spec.on_failure.is_none()) && !defs.agents.contains_key("triage") {
        l.warn("agents", "no `triage` agent: failures of jobs without on_failure will only alert the user");
    }
}

fn check_target(l: &mut Loader<'_>, defs: &Definitions, path: &str, target: &Target) -> bool {
    let problem = match target {
        Target::Resume(n) => match defs.agents.get(n) {
            Some(a) if a.spec.kind == AgentKind::Persistent => None,
            Some(_) => Some(format!("resume target {n:?} is ephemeral; use spawn or make it persistent")),
            None => Some(format!("resume target agent {n:?} does not exist")),
        },
        Target::Spawn(n) => (!defs.agents.contains_key(n)).then(|| format!("spawn target agent {n:?} does not exist")),
        Target::Run(n) => match defs.jobs.get(n) {
            Some(j) if j.spec.lifetime == Lifetime::Oneshot => None,
            Some(_) => Some(format!("run target {n:?} is a service; services cannot be triggered")),
            None => Some(format!("run target job {n:?} does not exist")),
        },
    };
    match problem {
        Some(p) => {
            l.err(path, p);
            false
        }
        None => true,
    }
}

fn resolve_trigger(
    l: &mut Loader<'_>,
    defs: &Definitions,
    path: &str,
    spec: Trigger,
    default_target: Option<Target>,
) -> Option<ResolvedTrigger> {
    let id = format!("{path}#{}", spec.name);
    l.check_name(&id, "trigger", &spec.name);
    let tz_name = spec.timezone.clone().unwrap_or_else(|| defs.instance.timezone.clone());
    let kind = match (&spec.cron, spec.every, &spec.on, &spec.webhook) {
        (Some(expr), None, None, None) => match ScheduleSpec::cron(expr, &tz_name) {
            Ok(s) => TriggerKind::Schedule(s),
            Err(e) => {
                l.err(&id, e.to_string());
                return None;
            }
        },
        (None, Some(every), None, None) => {
            if every.0 < std::time::Duration::from_secs(10) {
                l.err(&id, "`every` must be at least 10s; use a service for tighter loops");
                return None;
            }
            TriggerKind::Schedule(ScheduleSpec::Every(every))
        }
        (None, None, Some(pattern), None) => {
            if let Err(e) = globset::Glob::new(pattern) {
                l.err(&id, format!("invalid event pattern: {e}"));
                return None;
            }
            if !pattern.contains('*') && !defs.events.contains_key(pattern) && !is_builtin_event(pattern) {
                l.warn(&id, format!("event type {pattern:?} is not declared in events/"));
            }
            TriggerKind::On { pattern: pattern.clone() }
        }
        (None, None, None, Some(name)) => {
            l.check_name(&id, "webhook", name);
            match &spec.webhook_secret {
                Some(s) if defs.secrets.contains_key(s) => {}
                Some(s) => l.err(&id, format!("webhook_secret {s} is not declared in secrets.toml")),
                None => l.err(&id, "webhooks need a `webhook_secret` to verify callers"),
            }
            TriggerKind::Webhook { name: name.clone() }
        }
        _ => {
            l.err(&id, "a trigger needs exactly one of `cron`, `every`, `on`, `webhook`");
            return None;
        }
    };
    if spec.webhook.is_none() && spec.webhook_secret.is_some() {
        l.warn(&id, "webhook_secret is ignored on non-webhook triggers");
    }
    let Some(target) = spec.target.clone().or(default_target) else {
        l.err(&id, "standalone triggers need a `target`");
        return None;
    };
    if !check_target(l, defs, &id, &target) {
        return None;
    }
    if matches!(target, Target::Run(_)) && spec.prompt.is_some() {
        l.warn(&id, "`prompt` is ignored for job targets; the event payload is passed instead");
    }
    Some(ResolvedTrigger { id, spec, kind, target })
}

fn is_builtin_event(kind: &str) -> bool {
    use crate::event::kinds::*;
    [
        MESSAGE,
        RUNNABLE_FAILED,
        RUNNABLE_FINISHED,
        DIALOG_ANSWERED,
        DIALOG_ABORTED,
        PROPOSAL_UPDATED,
        ACTION_RESULT,
        BREAKER_TRIPPED,
        TIMER,
        ARCHIVE_REQUESTED,
    ]
    .contains(&kind)
}
