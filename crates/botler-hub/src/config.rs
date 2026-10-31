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
    /// Short id of this hub: a hash of its data directory. It is part of every container and
    /// network name the hub creates, and of its runtime directory.
    pub fn hub_id(&self) -> String {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(path_str(&self.root)))[..8].to_string()
    }
    /// The hub's Unix socket lives here: `$XDG_RUNTIME_DIR/botler-<hub id>`, else
    /// `/tmp/botler-<uid>-<hub id>`. Not in the data directory, whose path may be too long for a
    /// socket (about 108 bytes). The directory (not the socket) is mounted into every container
    /// that talks to the hub, at [`CONTAINER_RUN_DIR`], so a restarted hub's new socket is
    /// visible to containers that outlive it.
    pub fn run_dir(&self) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let id = self.hub_id();
        match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_absolute() && d.is_dir()) {
            Some(dir) => Ok(dir.join(format!("botler-{id}"))),
            None => Ok(PathBuf::from(format!("/tmp/botler-{}-{id}", std::fs::metadata("/proc/self")?.uid()))),
        }
    }
    pub fn socket(&self) -> std::io::Result<PathBuf> {
        Ok(self.run_dir()?.join(SOCKET_NAME))
    }
    pub fn uploads(&self) -> PathBuf {
        self.root.join("uploads")
    }
    /// Uploads attached to messages to `instance` (hard links), mounted at `/inbox/files`.
    pub fn inbox(&self, instance: &str) -> PathBuf {
        self.root.join("inbox").join(instance)
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
        for d in [
            "sessions",
            "workspaces",
            "homes",
            "shared-homes",
            "agent-dirs",
            "uploads",
            "logs",
            "exports",
            "worktrees",
            "inbox",
        ] {
            std::fs::create_dir_all(self.root.join(d))?;
        }
        private_dir(&self.run_dir()?)
    }
}

/// Runtime settings of the hub process (not of the monorepo).
#[derive(Clone, Debug)]
pub struct HubConfig {
    pub listen: SocketAddr,
    /// Base URL agents use to reach the hub (`BOTLER_URL`, MCP, git): the forwarder inside the
    /// agent container, which relays to the hub's socket. Jobs use the socket directly.
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
            container_url: FORWARD_URL.into(),
            runtime: None,
            pi_timeout: std::time::Duration::from_secs(60),
        }
    }
}

/// Where containers find the hub's socket directory, and the socket in it.
pub const CONTAINER_RUN_DIR: &str = "/run/botler";
pub const SOCKET_NAME: &str = "hub.sock";
/// The forwarder in agent containers listens here and relays to the socket.
pub const FORWARD_URL: &str = "http://127.0.0.1:7420";

/// Create `dir` (0700) or check an existing one is ours: in a shared place like `/tmp` someone
/// else could have made it first.
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != std::fs::metadata("/proc/self")?.uid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("{} is not a directory of this user; remove it or set XDG_RUNTIME_DIR", dir.display()),
        ));
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

pub fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}
