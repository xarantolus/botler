//! Loading and validating a monorepo tree into [`Definitions`].
//!
//! Loading never stops at the first problem: it collects every diagnostic so an agent gets the
//! whole list of what to fix in one gate run.

mod environment;
mod grants;
mod model;
mod mounts;
pub mod tree;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub use environment::*;
pub use grants::*;
pub use model::*;
pub use mounts::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
pub use tree::{FsTree, MemTree, Tree, TreeEntry};

use crate::trigger::ScheduleSpec;

/// The uv workspace's shared code and lock: what tools, connectors and packages are built from.
pub const SHARED_INPUTS: [&str; 5] = ["pyproject.toml", "uv.lock", "packages", "tools", "connectors"];

/// Repo paths a job's image is built from (its build context): its directory and the shared code.
pub fn job_inputs(job: &str) -> Vec<String> {
    let mut inputs = vec![format!("jobs/{job}")];
    inputs.extend(SHARED_INPUTS.map(String::from));
    inputs
}

/// A trigger with its owner and target resolved.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ResolvedTrigger {
    /// Stable id: `<file path>#<name>`, e.g. `agents/botler/agent.toml#morning`.
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
    pub secret_groups: BTreeMap<String, SecretGroup>,
    /// Every secret by its unique name.
    pub secrets: BTreeMap<String, SecretDecl>,
    pub events: BTreeMap<String, EventDecl>,
    /// Network shares from `mounts/<name>.toml`.
    pub mounts: BTreeMap<String, MountDef>,
    pub tools: BTreeMap<String, ToolDecl>,
    pub images: BTreeSet<String>,
    pub policy: CapabilityPolicy,
    pub grant_policy: GrantPolicy,
}

impl Definitions {
    /// The secrets a container gets for these group grants plus individually named secrets
    /// (route API keys), deduplicated, in a stable order.
    pub fn secrets_for(&self, groups: &[String], extra: &[String]) -> Vec<&SecretDecl> {
        let mut names: Vec<&str> = groups
            .iter()
            .filter_map(|g| self.secret_groups.get(g))
            .flat_map(|g| g.secrets.iter().map(String::as_str))
            .chain(extra.iter().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        names.into_iter().filter_map(|n| self.secrets.get(n)).collect()
    }

    /// The services that should run: enabled jobs with `lifetime = "service"`.
    pub fn services(&self) -> impl Iterator<Item = &JobDef> {
        self.jobs.values().filter(|j| j.spec.is_service())
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

    /// Tools granted to an environment by its `tools` globs. Globs are validated at load; if one is
    /// somehow invalid, no tools are granted (fail closed).
    pub fn tools_for(&self, env: &Environment) -> Vec<&ToolDecl> {
        let Ok(set) = glob_set(&env.tools) else { return Vec::new() };
        self.tools.values().filter(|t| set.is_match(&t.name)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// One problem found while loading, tied to the file it is in.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, ts_rs::TS)]
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

/// Whether the TOML file at `path` has the top-level key `key` (for keys that were renamed, so the
/// error names the replacement instead of serde's generic "unknown field").
fn renamed_key(tree: &dyn Tree, path: &str, key: &str) -> bool {
    match tree.read(path) {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .ok()
            .and_then(|text| toml::from_str::<toml::Table>(&text).ok())
            .is_some_and(|t| t.contains_key(key)),
        // Unreadable files are reported by the regular parse that follows.
        _ => false,
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

/// What [`load_lenient`] could keep of a tree that does not load.
#[derive(Debug)]
pub struct Lenient {
    pub definitions: Definitions,
    /// The errors of the full tree.
    pub errors: Vec<Diagnostic>,
    /// What was left out to load the rest: `agents/<name>`, `jobs/<name>`, `triggers/<file>`.
    pub skipped: Vec<String>,
}

/// The agent, job or trigger file a diagnostic belongs to, if it can be left out on its own.
fn unit(path: &str) -> Option<String> {
    let path = path.split('#').next().unwrap_or(path);
    let mut segs = path.split('/');
    match (segs.next(), segs.next()) {
        (Some(dir @ ("agents" | "jobs")), Some(name)) if !name.is_empty() => Some(format!("{dir}/{name}")),
        (Some("triggers"), Some(file)) if !file.is_empty() => Some(format!("triggers/{file}")),
        _ => None,
    }
}

/// A tree with some paths (and everything under them) hidden.
struct Without<'a> {
    tree: &'a dyn Tree,
    hidden: &'a BTreeSet<String>,
}

impl Without<'_> {
    fn hides(&self, path: &str) -> bool {
        self.hidden.iter().any(|h| path == h || path.starts_with(&format!("{h}/")))
    }
}

impl Tree for Without<'_> {
    fn read(&self, path: &str) -> std::io::Result<Option<Vec<u8>>> {
        if self.hides(path) { Ok(None) } else { self.tree.read(path) }
    }

    fn list(&self, dir: &str) -> std::io::Result<Vec<TreeEntry>> {
        let mut entries = self.tree.list(dir)?;
        entries.retain(|e| !self.hides(&format!("{}/{}", dir.trim_end_matches('/'), e.name)));
        Ok(entries)
    }
}

/// Load the valid part of a tree that does not load: agents, jobs and trigger files with errors
/// are left out (then whatever referred to them, and so on) until the rest loads. `None` when an
/// error is elsewhere (`botler.toml`, secrets, events, policies), which nothing can be left out for.
pub fn load_lenient(tree: &dyn Tree, default_tz: chrono_tz::Tz) -> Option<Lenient> {
    let mut hidden = BTreeSet::new();
    let mut errors = None;
    loop {
        let report = load_with(&Without { tree, hidden: &hidden }, default_tz);
        let errs: Vec<Diagnostic> = report.errors().cloned().collect();
        if let Some(definitions) = report.definitions {
            let errors = errors.unwrap_or_default();
            return Some(Lenient { definitions, errors, skipped: hidden.into_iter().collect() });
        }
        let before = hidden.len();
        for d in &errs {
            hidden.insert(unit(&d.path)?);
        }
        errors.get_or_insert(errs);
        if hidden.len() == before {
            return None;
        }
    }
}

/// Load and validate the monorepo at `tree`, with UTC where `botler.toml` names no time zone.
pub fn load(tree: &dyn Tree) -> LoadReport {
    load_with(tree, chrono_tz::UTC)
}

/// Load and validate the monorepo at `tree`; `default_tz` applies when `botler.toml` names no
/// time zone (the hub passes the host's).
pub fn load_with(tree: &dyn Tree, default_tz: chrono_tz::Tz) -> LoadReport {
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
    match defs.instance.timezone.as_deref().map(str::parse::<chrono_tz::Tz>) {
        None => defs.timezone = default_tz,
        Some(Ok(tz)) => defs.timezone = tz,
        Some(Err(_)) => {
            let tz = defs.instance.timezone.clone().unwrap_or_default();
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

    if let Some(p) = l.toml::<GrantPolicy>("policies/grants.toml") {
        l.check_version("policies/grants.toml", p.version);
        for r in p.resources.keys() {
            if let Err(e) = r.parse::<Resource>() {
                l.err("policies/grants.toml", e);
            }
        }
        defs.grant_policy = p;
    }

    if let Some(s) = l.toml::<SecretsFile>("secrets.toml") {
        const P: &str = "secrets.toml";
        l.check_version(P, s.version);
        for g in s.groups {
            l.check_name(P, "secret group", &g.name);
            if g.secrets.is_empty() {
                l.err(P, format!("secret group {} has no secrets", g.name));
            }
            let mut names = Vec::new();
            for f in &g.secrets {
                if f.name.is_empty()
                    || !f.name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                {
                    l.err(P, format!("secret name {:?} must be UPPER_SNAKE_CASE", f.name));
                }
                match &f.file {
                    Some(_) if g.class == SecretClass::Hub => {
                        l.err(
                            P,
                            format!(
                                "{} is in hub group {} and never enters a container, so it cannot have a file path",
                                f.name, g.name
                            ),
                        );
                    }
                    Some(path) if !path.starts_with('/') => {
                        l.err(P, format!("file path of {} must be absolute", f.name))
                    }
                    _ => {}
                }
                let decl = SecretDecl {
                    name: f.name.clone(),
                    group: g.name.clone(),
                    description: f.description.clone(),
                    class: g.class,
                    file: f.file.clone(),
                };
                if let Some(prev) = defs.secrets.insert(f.name.clone(), decl) {
                    l.err(P, format!("secret {} is declared twice (groups {} and {})", f.name, prev.group, g.name));
                }
                names.push(f.name.clone());
            }
            let group =
                SecretGroup { name: g.name.clone(), description: g.description, class: g.class, secrets: names };
            if defs.secret_groups.insert(g.name.clone(), group).is_some() {
                l.err(P, format!("secret group {} is declared twice", g.name));
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

    for entry in l.list("mounts") {
        let Some(name) = entry.name.strip_suffix(".toml").filter(|_| !entry.is_dir) else { continue };
        let path = format!("mounts/{}", entry.name);
        l.check_name(&path, "mount", name);
        if let Some(spec) = l.toml::<MountFile>(&path) {
            l.check_version(&path, spec.version);
            defs.mounts.insert(name.to_string(), MountDef { name: name.to_string(), spec });
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
        if renamed_key(tree, &path, "scratch") {
            l.err(
                &path,
                "`scratch` was renamed to `home`: write `home = \"<name>\"` to share a home directory \
                 (`/home/agent`) between agents, or remove the line for a home of its own",
            );
            continue;
        }
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
        match (spec.lifetime, &spec.args) {
            (Lifetime::Action, None) => {
                l.err(&path, "an action needs `args`: the JSON Schema of the arguments it is invoked with");
            }
            (Lifetime::Action, Some(schema)) => {
                if let Err(e) = crate::event_check::compile(schema) {
                    l.err(&path, format!("`args` is not a valid JSON Schema: {e}"));
                }
                if !spec.triggers.is_empty() {
                    l.err(&path, "an action runs only when invoked and approved; it cannot have triggers");
                }
            }
            (_, Some(_)) => l.err(&path, "`args` only applies to actions (lifetime = \"action\")"),
            (_, None) => {}
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
    validate_mounts(&mut l, &defs);
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
            check_route_secret(l, defs, "botler.toml", s);
        }
    }
    if let Some(p) = &defs.instance.push {
        if !(p.ntfy_url.starts_with("https://") || p.ntfy_url.starts_with("http://")) {
            l.err("botler.toml", format!("push.ntfy_url {:?} must be an http(s) URL", p.ntfy_url));
        }
        if !defs.secrets.contains_key(&p.topic_secret) {
            l.err("botler.toml", format!("push.topic_secret {} is not declared in secrets.toml", p.topic_secret));
        }
    }
}

/// A grant of a secret group to a container.
fn check_container_group(l: &mut Loader<'_>, defs: &Definitions, path: &str, group: &str) {
    match defs.secret_groups.get(group) {
        None if defs.secrets.contains_key(group) => {
            l.err(path, format!("`{group}` is a secret, not a group; grant its group `{}`", defs.secrets[group].group))
        }
        None => l.err(path, format!("secret group {group} is not declared in secrets.toml")),
        Some(g) if g.class == SecretClass::Hub => l.err(
            path,
            format!("secret group {group} is hub-class and can never enter a container; use an approval-gated action"),
        ),
        Some(_) => {}
    }
}

/// A single secret a model route uses as its API key.
fn check_route_secret(l: &mut Loader<'_>, defs: &Definitions, path: &str, name: &str) {
    match defs.secrets.get(name) {
        None => l.err(path, format!("secret {name} is not declared in secrets.toml")),
        Some(d) if d.class == SecretClass::Hub => {
            l.err(path, format!("secret {name} is hub-class and can never enter a container"))
        }
        Some(_) => {}
    }
}

fn validate_mounts(l: &mut Loader<'_>, defs: &Definitions) {
    for m in defs.mounts.values() {
        let path = format!("mounts/{}.toml", m.name);
        for p in m.problems() {
            l.err(&path, p);
        }
        check_container_group(l, defs, &path, &m.spec.secrets);
        let Some(group) = defs.secret_groups.get(&m.spec.secrets) else { continue };
        let options = m.spec.kind.options();
        for s in group.secrets.iter().filter_map(|s| defs.secrets.get(s)) {
            if m.option_for(&s.name).is_none() {
                let ends = options.iter().map(|o| format!("_{o}")).collect::<Vec<_>>().join(", ");
                l.err(
                    &path,
                    format!(
                        "secret {} of group {}: a {} share needs names ending in {ends}",
                        s.name,
                        group.name,
                        m.spec.kind.as_str()
                    ),
                );
            }
            if s.file.is_some() {
                l.err(&path, format!("secret {} is delivered as a file; a share's secrets must be values", s.name));
            }
        }
        if !group.secrets.iter().any(|s| m.option_for(s) == Some(options[0])) {
            l.err(
                &path,
                format!(
                    "group {} has no secret ending in _{} (required for {})",
                    group.name,
                    options[0],
                    m.spec.kind.as_str()
                ),
            );
        }
    }
}

/// The mounts an agent or job lists: defined, writable when `rw`, reachable, at distinct paths.
fn check_mounts(l: &mut Loader<'_>, defs: &Definitions, path: &str, env: &Environment) {
    let mut paths = BTreeMap::new();
    for (name, mode) in &env.mounts {
        let Some(m) = defs.mounts.get(name) else {
            l.err(path, format!("mount {name:?} is not defined in mounts/"));
            continue;
        };
        if *mode == MountMode::ReadWrite && m.spec.read_only {
            l.err(path, format!("mount {name} is read_only = true in mounts/{name}.toml and cannot be \"rw\""));
        }
        if let Some(other) = paths.insert(m.spec.path.trim_end_matches('/'), name) {
            l.err(path, format!("mounts {other} and {name} both use {}", m.spec.path));
        }
    }
    if !env.mounts.is_empty() && env.network() == Network::None {
        l.err(path, "mounts are reached over the network; network = \"none\" cannot use them");
    }
}

fn validate_agents(l: &mut Loader<'_>, defs: &Definitions) {
    for a in defs.agents.values() {
        let path = format!("agents/{}/agent.toml", a.name);
        for key in a.spec.env.foreign_keys(true) {
            l.err(&path, format!("`{key}` only applies to jobs and services, not agents"));
        }
        if !defs.images.contains(a.spec.env.image()) {
            l.err(&path, format!("image {:?} not found under images/", a.spec.env.image()));
        }
        if let Some(m) = &a.spec.model
            && !defs.instance.models.routes.contains_key(m)
        {
            l.err(&path, format!("model route {m:?} not defined in botler.toml"));
        }
        if defs.route_for(&a.spec).is_none() {
            l.err(&path, "no model route: set `model` or `models.default` in botler.toml");
        }
        for g in &a.spec.env.secrets {
            check_container_group(l, defs, &path, g);
        }
        check_mounts(l, defs, &path, &a.spec.env);
        let listed = a.spec.env.mcp.iter().map(|m| ("mcp", m));
        for (field, m) in listed.chain(a.spec.env.services.iter().map(|s| ("services", s))) {
            match defs.jobs.get(m) {
                Some(j) if j.spec.lifetime == Lifetime::Service && j.spec.env.port.is_none() => {
                    l.err(&path, format!("{field} {m:?}: the service declares no `port` to reach it on"))
                }
                Some(j) if j.spec.lifetime == Lifetime::Service => {}
                Some(_) => l.err(&path, format!("{field} {m:?} is a oneshot job or action, not a service")),
                None => l.err(&path, format!("{field} {m:?} is not a job in jobs/")),
            }
        }
        // Every model route is reached over the network today (even a local `base_url` lies
        // outside the container), so an agent without network could never call its model.
        if a.spec.env.network() == Network::None
            && let Some((route, r)) = defs.route_for(&a.spec)
        {
            l.err(
                &path,
                format!(
                    "network = \"none\" cannot reach model route {route:?} ({}/{}): agents call their model over \
                     the network. Remove `network = \"none\"`",
                    r.provider, r.model
                ),
            );
        }
        if !a.spec.env.reached().is_empty() && a.spec.env.network() == Network::None {
            l.err(
                &path,
                "`mcp` and `services` are reached over the network; an agent with network = \"none\" cannot use them",
            );
        }
        for (name, s) in &a.spec.env.mcp_stdio {
            let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
            if !ok {
                l.err(&path, format!("mcp_stdio server name {name:?} may only use letters, digits, `_` and `-`"));
            }
            if name == "botler" || a.spec.env.mcp.contains(name) {
                l.err(&path, format!("mcp_stdio server name {name:?} is already taken"));
            }
            if s.command.first().is_none_or(|c| c.trim().is_empty()) {
                l.err(&path, format!("mcp_stdio {name:?}: command must not be empty"));
            }
        }
        if let Err(e) = glob_set(&a.spec.env.tools) {
            l.err(&path, format!("invalid tools glob: {e}"));
        } else if !a.spec.env.tools.is_empty() && defs.tools_for(&a.spec.env).is_empty() {
            l.warn(&path, "tools globs match no tool in tools.manifest.json");
        }
        if let Some(home) = &a.spec.env.home {
            l.check_name(&path, "home", home);
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
    let mut host_ports: BTreeMap<u16, &str> = BTreeMap::new();
    for j in defs.jobs.values() {
        let path = format!("jobs/{}/job.toml", j.name);
        for key in j.spec.env.foreign_keys(false) {
            l.err(&path, format!("`{key}` only applies to agents"));
        }
        if j.spec.lifetime != Lifetime::Service && (j.spec.env.port.is_some() || !j.spec.env.publish.is_empty()) {
            l.err(&path, "`port` and `publish` only apply to services (lifetime = \"service\")");
        }
        for p in &j.spec.env.publish {
            if p.host_port < 1024 {
                l.err(
                    &path,
                    format!(
                        "publish host_port {} is below 1024; rootless Podman cannot bind privileged ports, use e.g. {}",
                        p.host_port,
                        8000 + p.host_port % 1000
                    ),
                );
            }
            if p.bind.parse::<std::net::IpAddr>().is_err() {
                l.err(
                    &path,
                    format!("publish bind {:?} is not an IP address (use \"0.0.0.0\" or \"127.0.0.1\")", p.bind),
                );
            }
            if let Some(other) = host_ports.insert(p.host_port, &j.name) {
                l.err(&path, format!("host port {} is already published by service `{other}`", p.host_port));
            }
        }
        for g in &j.spec.env.secrets {
            // Actions run only after the user approved their arguments: the one place hub-class
            // secrets may be used.
            if j.spec.lifetime == Lifetime::Action && defs.secret_groups.contains_key(g) {
                continue;
            }
            check_container_group(l, defs, &path, g);
        }
        check_mounts(l, defs, &path, &j.spec.env);
        if let Some(t) = &j.spec.on_failure {
            check_target(l, defs, &path, t);
        }
        for e in j.spec.emits.iter().filter(|e| !defs.events.contains_key(*e)) {
            l.err(&path, format!("emits {e:?}, which is not a declared event type (events/ or a pydantic model)"));
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
            Some(_) => Some(format!("run target {n:?} is a service or action; only oneshot jobs are triggered")),
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
    let tz_name = spec.timezone.clone().unwrap_or_else(|| defs.timezone.name().to_string());
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
        AGENT_IDLE,
        ACCESS_CHANGED,
        CHAT_RESTORED,
        ENVIRONMENT_UPDATED,
        TRIAL_FINISHED,
        CHAT_STOPPED,
        HANDOVER_REQUESTED,
        SESSION_STARTED,
    ]
    .contains(&kind)
}

#[cfg(test)]
mod lenient_tests {
    use super::*;

    fn base() -> MemTree {
        MemTree::new()
            .with("botler.toml", "[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"p\"\nmodel = \"m\"\n")
            .with("images/agent/Dockerfile", "FROM x")
            .with("agents/good/agent.toml", "kind = \"persistent\"\n")
            .with("agents/good/AGENTS.md", "hi")
            .with("agents/bad/agent.toml", "bogus = 1\n")
            .with("agents/bad/AGENTS.md", "hi")
            .with("triggers/t.toml", "[[trigger]]\nname = \"x\"\nevery = \"1h\"\ntarget = { spawn = \"bad\" }\n")
            .with("triggers/u.toml", "[[trigger]]\nname = \"y\"\nevery = \"1h\"\ntarget = { resume = \"good\" }\n")
    }

    #[test]
    fn invalid_agents_and_what_refers_to_them_are_left_out() {
        let t = base();
        assert!(load(&t).definitions.is_none());
        let l = load_lenient(&t, chrono_tz::UTC).unwrap();
        assert_eq!(l.skipped, ["agents/bad", "triggers/t.toml"]);
        assert!(l.definitions.agents.contains_key("good") && !l.definitions.agents.contains_key("bad"));
        assert_eq!(l.definitions.triggers.len(), 1);
        assert!(l.errors.iter().any(|d| d.path == "agents/bad/agent.toml"), "{:?}", l.errors);
        // Nothing to leave out for a broken botler.toml.
        assert!(load_lenient(&base().with("botler.toml", "nope = ["), chrono_tz::UTC).is_none());
    }
}
