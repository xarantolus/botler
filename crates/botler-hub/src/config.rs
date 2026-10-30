//! Where the hub keeps things, and how containers reach it.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// Layout of the data directory. Everything the hub owns lives under one root, so a backup is a
/// copy of this directory (with `hub.db` taken through the online backup).
#[derive(Clone, Debug)]
pub struct Paths {
    pub root: PathBuf,
}

impl Paths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn db(&self) -> PathBuf {
        self.root.join("hub.db")
    }
    /// The full-text search index: derived data only, rebuilt when deleted (never backed up).
    pub fn search_db(&self) -> PathBuf {
        self.root.join("search.db")
    }
    pub fn secret_key(&self) -> PathBuf {
        self.root.join("secrets.key")
    }
    pub fn repo(&self) -> PathBuf {
        self.root.join("repo.git")
    }
    /// Agent memory: Markdown files in namespaces, with a git repo for history.
    pub fn memory(&self) -> PathBuf {
        self.root.join("memory")
    }
    /// The hub's own short-lived git worktrees (rebases, reverts, hub-authored commits).
    pub fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }
    /// Exports from the monorepo: `pi-<tree>` (agent pi resources, shared by every commit with
    /// the same `pi/`) and short-lived `build-*` image build contexts.
    pub fn exports(&self) -> PathBuf {
        self.root.join("exports")
    }
    /// pi session files, one directory per instance.
    pub fn sessions(&self, instance: &str) -> PathBuf {
        self.root.join("sessions").join(instance)
    }
    /// Monorepo clones of agents, one per instance, mounted at `/home/agent/monorepo`.
    pub fn workspace(&self, instance: &str) -> PathBuf {
        self.root.join("workspaces").join(instance)
    }
    /// Rendered pi config files for an instance (mounted read-only over a tmpfs agent dir).
    pub fn agent_dir(&self, instance: &str) -> PathBuf {
        self.root.join("agent-dirs").join(instance)
    }
    /// An instance's own home directory (`/home/agent`), kept across sleeps and archiving.
    pub fn home(&self, instance: &str) -> PathBuf {
        self.root.join("homes").join(instance)
    }
    /// A home shared by every agent whose definition names it (`home = "<name>"`). Kept apart
    /// from per-instance homes so a shared name can never collide with an instance id.
    pub fn shared_home(&self, name: &str) -> PathBuf {
        self.root.join("shared-homes").join(name)
    }
    pub fn uploads(&self) -> PathBuf {
        self.root.join("uploads")
    }
    /// Full logs of finished runs.
    pub fn run_log(&self, run_id: &str) -> PathBuf {
        self.root.join("logs").join(format!("{run_id}.log"))
    }
    /// Files the binary ships (pi extension, Python SDK, base image), unpacked per hub version.
    pub fn assets(&self) -> PathBuf {
        self.root.join("assets").join(env!("CARGO_PKG_VERSION"))
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for d in
            ["sessions", "workspaces", "homes", "shared-homes", "agent-dirs", "uploads", "logs", "exports", "worktrees"]
        {
            std::fs::create_dir_all(self.root.join(d))?;
        }
        Ok(())
    }
}

/// Runtime settings of the hub process (not of the monorepo).
#[derive(Clone, Debug)]
pub struct HubConfig {
    pub listen: SocketAddr,
    /// Base URL containers use to reach the hub (`BOTLER_URL`).
    pub container_url: String,
    /// `podman` or `docker`; auto-detected when `None`.
    pub runtime: Option<String>,
    /// How long an attached pi may take to answer a command.
    pub pi_timeout: std::time::Duration,
}

impl HubConfig {
    pub fn new(listen: SocketAddr) -> Self {
        Self {
            listen,
            // Rootless podman and docker both resolve this name to the host.
            container_url: format!("http://host.containers.internal:{}", listen.port()),
            runtime: None,
            pi_timeout: std::time::Duration::from_secs(60),
        }
    }
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
