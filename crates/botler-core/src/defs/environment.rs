//! The container a runnable gets, shared by agents, jobs and services.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{MountMode, Network, Publish, StdioMcp, Workspace};

/// A runnable's container: image, home, secrets, network and what it may reach. `agent.toml` and
/// `job.toml` hold these keys directly; keys that only apply to one kind are rejected on the
/// others when loading.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Environment {
    /// Agents: image name under `images/` (default `agent`). Jobs build theirs from their own
    /// directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// Agents: the persistent home directory mounted at `/home/agent` (`$HOME`, the working
    /// directory). Defaults to one per instance; naming one (e.g. `"groundstation"`) shares it
    /// between every instance of every definition that names it.
    #[serde(default)]
    pub home: Option<String>,
    /// Agents: what they get besides their home (default `monorepo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<Workspace>,
    /// Jobs and services: keep a persistent volume mounted at `/data` across runs.
    #[serde(default)]
    pub volume: bool,
    /// Secret groups (from `secrets.toml`) delivered into the container.
    #[serde(default)]
    pub secrets: Vec<String>,
    /// Network shares from `mounts/`, by name, and how: `mounts = { nas-photos = "ro" }`.
    #[serde(default)]
    pub mounts: BTreeMap<String, MountMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    /// Agents: services (jobs with `lifetime = "service"` and a `port`) whose MCP server this
    /// agent uses. The agent joins the services network and reaches each at
    /// `http://<name>:<port>/mcp`.
    #[serde(default)]
    pub mcp: Vec<String>,
    /// Agents: services (jobs with `lifetime = "service"` and a `port`) this agent reaches over
    /// plain HTTP or another protocol, not MCP: it joins the services network and finds each in
    /// `/botler/agent-dir/services.json` (name, IP and port).
    #[serde(default)]
    pub services: Vec<String>,
    /// Agents: MCP servers pi starts inside the container over stdio, by server name, e.g.
    /// `mcp_stdio = { fs = { command = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "."] } }`.
    #[serde(default)]
    pub mcp_stdio: BTreeMap<String, StdioMcp>,
    /// Agents: Python tool name globs from `tools/` registered into pi, e.g. `["github.*"]`.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Services: the port the service listens on inside its container. Agents granted it (`mcp`)
    /// reach it at `http://<name>:<port>`.
    #[serde(default)]
    pub port: Option<u16>,
    /// Services: container ports published on the host, reachable from outside (LAN, tailnet).
    #[serde(default)]
    pub publish: Vec<Publish>,
}

/// Something an environment gets beyond another: each needs a grant (or the user's approval).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvEscalation {
    SecretGroup(String),
    /// Network access where there was none.
    Network,
    /// Reaching a service's MCP server.
    Service(String),
    /// A new or changed stdio MCP server: code that runs in the container.
    McpStdio {
        server: String,
        command: String,
    },
    Tools(String),
    /// Another home: someone else's files.
    Home(String),
    /// A port published on the host.
    Port(Publish),
    /// A share mounted where it was not, or now read-write.
    Mount {
        name: String,
        mode: MountMode,
    },
}

impl Environment {
    pub fn image(&self) -> &str {
        self.image.as_deref().unwrap_or("agent")
    }

    pub fn workspace(&self) -> Workspace {
        self.workspace.unwrap_or_default()
    }

    pub fn network(&self) -> Network {
        self.network.unwrap_or_default()
    }

    /// Every service this environment reaches, over MCP or not.
    pub fn reached(&self) -> Vec<String> {
        let mut all = self.mcp.clone();
        all.extend(self.services.iter().filter(|s| !self.mcp.contains(s)).cloned());
        all
    }

    /// The keys set here that do not apply to agents (`agent = true`) or jobs and services.
    pub fn foreign_keys(&self, agent: bool) -> Vec<&'static str> {
        let keys = if agent {
            [("volume", self.volume), ("port", self.port.is_some()), ("publish", !self.publish.is_empty())].to_vec()
        } else {
            [
                ("image", self.image.is_some()),
                ("home", self.home.is_some()),
                ("workspace", self.workspace.is_some()),
                ("mcp", !self.mcp.is_empty()),
                ("services", !self.services.is_empty()),
                ("mcp_stdio", !self.mcp_stdio.is_empty()),
                ("tools", !self.tools.is_empty()),
            ]
            .to_vec()
        };
        keys.into_iter().filter(|(_, set)| *set).map(|(k, _)| k).collect()
    }

    /// This environment with `with` merged in: values `with` sets replace these, lists and
    /// servers are added. Also returns what the result gets beyond this one.
    pub fn extend(&self, with: &Environment) -> (Environment, Vec<EnvEscalation>) {
        fn add<T: Clone + PartialEq>(to: &mut Vec<T>, more: &[T]) {
            for m in more {
                if !to.contains(m) {
                    to.push(m.clone());
                }
            }
        }
        let mut env = self.clone();
        env.image = with.image.clone().or(env.image);
        env.home = with.home.clone().or(env.home);
        env.workspace = with.workspace.or(env.workspace);
        env.network = with.network.or(env.network);
        env.port = with.port.or(env.port);
        env.volume |= with.volume;
        add(&mut env.secrets, &with.secrets);
        add(&mut env.mcp, &with.mcp);
        add(&mut env.services, &with.services);
        add(&mut env.tools, &with.tools);
        add(&mut env.publish, &with.publish);
        env.mcp_stdio.extend(with.mcp_stdio.clone());
        for (name, mode) in &with.mounts {
            let m = env.mounts.entry(name.clone()).or_insert(*mode);
            *m = (*m).max(*mode);
        }
        let escalations = self.escalations(&env);
        (env, escalations)
    }

    /// What `new` gets beyond this environment.
    pub fn escalations(&self, new: &Environment) -> Vec<EnvEscalation> {
        let mut out = Vec::new();
        let added =
            |old: &[String], new: &[String]| new.iter().filter(|n| !old.contains(n)).cloned().collect::<Vec<_>>();
        out.extend(added(&self.secrets, &new.secrets).into_iter().map(EnvEscalation::SecretGroup));
        if self.network() == Network::None && new.network() == Network::Default {
            out.push(EnvEscalation::Network);
        }
        // Reaching a service is one escalation whether over MCP or not.
        out.extend(added(&self.reached(), &new.reached()).into_iter().map(EnvEscalation::Service));
        for (server, s) in &new.mcp_stdio {
            if self.mcp_stdio.get(server) != Some(s) {
                out.push(EnvEscalation::McpStdio { server: server.clone(), command: s.command.join(" ") });
            }
        }
        out.extend(added(&self.tools, &new.tools).into_iter().map(EnvEscalation::Tools));
        if let Some(home) = new.home.as_ref().filter(|h| self.home.as_ref() != Some(*h)) {
            out.push(EnvEscalation::Home(home.clone()));
        }
        out.extend(new.publish.iter().filter(|p| !self.publish.contains(p)).cloned().map(EnvEscalation::Port));
        for (name, mode) in &new.mounts {
            if self.mounts.get(name).is_none_or(|old| old < mode) {
                out.push(EnvEscalation::Mount { name: name.clone(), mode: *mode });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(toml: &str) -> Environment {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn extend_adds_and_reports_what_is_new() {
        let def = env(r#"
            secrets = ["github"]
            network = "none"
            mcp = ["notes"]
            tools = ["web.*"]
            mcp_stdio = { fs = { command = ["fs-server"] } }
            mounts = { nas = "ro", docs = "rw" }
            "#);
        let (merged, esc) = def.extend(&env(r#"
            secrets = ["github", "gitlab"]
            network = "default"
            mcp = ["notes", "calendar"]
            tools = ["web.*", "github.*"]
            home = "groundstation"
            mcp_stdio = { fs = { command = ["fs-server", "--write"] }, git = { command = ["git-mcp"] } }
            publish = [{ port = 80, host_port = 8080 }]
            mounts = { nas = "rw" }
            "#));
        assert_eq!(merged.secrets, ["github", "gitlab"]);
        assert_eq!(merged.mcp, ["notes", "calendar"]);
        assert_eq!(merged.network(), Network::Default);
        assert_eq!(merged.home.as_deref(), Some("groundstation"));
        assert_eq!(merged.mcp_stdio["fs"].command, ["fs-server", "--write"]);
        // Mounts it does not name stay as they were.
        assert_eq!(merged.mounts["docs"], MountMode::ReadWrite);
        let publish = Publish { port: 80, host_port: 8080, bind: "0.0.0.0".into() };
        assert_eq!(
            esc,
            [
                EnvEscalation::SecretGroup("gitlab".into()),
                EnvEscalation::Network,
                EnvEscalation::Service("calendar".into()),
                EnvEscalation::McpStdio { server: "fs".into(), command: "fs-server --write".into() },
                EnvEscalation::McpStdio { server: "git".into(), command: "git-mcp".into() },
                EnvEscalation::Tools("github.*".into()),
                EnvEscalation::Home("groundstation".into()),
                EnvEscalation::Port(publish),
                EnvEscalation::Mount { name: "nas".into(), mode: MountMode::ReadWrite },
            ]
        );
    }

    #[test]
    fn extending_with_nothing_or_less_escalates_nothing() {
        let def = env(r#"secrets = ["github"]
            home = "shared"
            network = "default"
            mcp = ["notes"]"#);
        let (merged, esc) = def.extend(&Environment::default());
        assert_eq!(merged, def);
        assert!(esc.is_empty());
        // Narrowing the network or naming the same home needs no grant.
        let (merged, esc) = def.extend(&env("network = \"none\"\nhome = \"shared\"\nsecrets = [\"github\"]"));
        assert_eq!(merged.network(), Network::None);
        assert!(esc.is_empty(), "{esc:?}");
    }

    #[test]
    fn definitions_keep_their_keys_and_refuse_others() {
        use crate::defs::{MemTree, load};
        let tree = |agent: &str, job: &str| {
            MemTree::new()
                .with("botler.toml", "[models]\ndefault = \"d\"\n[models.routes.d]\nprovider = \"p\"\nmodel = \"m\"\n")
                .with("images/agent/Dockerfile", "FROM x")
                .with("agents/a/agent.toml", agent)
                .with("agents/a/AGENTS.md", "hi")
                .with("jobs/j/job.toml", format!("command = [\"run\"]\n{job}"))
                .with("jobs/j/Dockerfile", "FROM x")
        };
        let errors = |t: &MemTree| load(t).errors().map(|d| d.message.clone()).collect::<Vec<_>>();
        let ok = tree("workspace = \"none\"\nhome = \"h\"\n", "volume = true\nnetwork = \"none\"\n");
        let defs = load(&ok).into_result().unwrap_or_else(|d| panic!("{d:#?}"));
        assert_eq!(defs.agents["a"].spec.env.workspace(), Workspace::None);
        assert_eq!(defs.agents["a"].spec.env.home.as_deref(), Some("h"));
        assert!(defs.jobs["j"].spec.env.volume && defs.jobs["j"].spec.env.network() == Network::None);
        // Jobs may run without network; an agent could never reach its (remote) model.
        let offline = errors(&tree("network = \"none\"\n", ""));
        assert_eq!(offline.len(), 1, "{offline:?}");
        assert!(offline[0].contains("network = \"none\" cannot reach model route \"d\" (p/m)"), "{offline:?}");
        let unknown = errors(&tree("bogus = 1\n", "bogus = 1\n"));
        assert_eq!(unknown.len(), 2, "{unknown:?}");
        assert!(unknown.iter().all(|e| e.contains("unknown field `bogus`")), "{unknown:?}");
        let foreign = errors(&tree("volume = true\n", "tools = [\"x.*\"]\n"));
        assert!(foreign.iter().any(|e| e.contains("`volume` only applies to jobs")), "{foreign:?}");
        assert!(foreign.iter().any(|e| e.contains("`tools` only applies to agents")), "{foreign:?}");
    }

    #[test]
    fn foreign_keys_are_named() {
        let e = env("volume = true\nmcp = [\"x\"]\nport = 8080");
        assert_eq!(e.foreign_keys(true), ["volume", "port"]);
        assert_eq!(e.foreign_keys(false), ["mcp"]);
        assert_eq!(Environment::default().image(), "agent");
    }
}
