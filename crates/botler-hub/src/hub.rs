//! The hub: shared state and the operations every subsystem builds on.

use std::sync::Arc;

use botler_core::payloads::{BuiltinPayload, MessagePayload};
use botler_core::{Definitions, Diagnostic, Event, Provenance, Source};
use botler_repo::Repo;
use botler_runtime::Containers;
use botler_store::{SecretKey, Store};
use chrono::Utc;
use tokio::sync::{Mutex, Notify, RwLock, broadcast};

use crate::auth::Tokens;
use crate::config::{HubConfig, Paths};

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error(transparent)]
    Store(#[from] botler_store::Error),
    #[error(transparent)]
    Repo(#[from] botler_repo::Error),
    #[error(transparent)]
    Container(#[from] botler_runtime::ContainerError),
    #[error(transparent)]
    Pi(#[from] botler_runtime::PiError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("definitions at {commit} are invalid ({} problems)", diagnostics.len())]
    InvalidDefinitions { commit: String, diagnostics: Vec<Diagnostic> },
    #[error("{kind} {name} is not defined")]
    Undefined { kind: &'static str, name: String },
    #[error("{operation} is rate limited; retry in {retry_after:?}")]
    RateLimited { operation: String, retry_after: std::time::Duration },
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("botler is paused")]
    Paused,
    #[error("secret {0} has no value; enter it in the web app")]
    MissingSecret(String),
    #[error(
        "{parent} is {depth} spawns deep, the most allowed (max_spawn_depth); do the work yourself or report back to whoever started you"
    )]
    SpawnTooDeep { parent: String, depth: u32 },
    #[error("migrating the data directory: {0}")]
    Migration(String),
    #[error(transparent)]
    Host(#[from] crate::host::HostError),
    #[error(transparent)]
    Search(#[from] crate::search::SearchError),
    #[error(transparent)]
    Memory(#[from] crate::memory::MemoryError),
    #[error(transparent)]
    Storage(#[from] crate::storage::StorageError),
    #[error(transparent)]
    Upload(#[from] crate::uploads::UploadError),
    #[error("{instance}'s agent definition `{definition}` no longer exists; restore it as a generic chat or delete it")]
    DefinitionGone { instance: String, definition: String },
    #[error("{instance} keeps crashing; next start allowed at {until}")]
    Backoff { instance: String, until: chrono::DateTime<chrono::Utc> },
    #[error("host port {ports} is already in use by another program or container; publish a free port: {source}")]
    PortInUse { ports: String, source: botler_runtime::ContainerError },
    #[error("images build FROM each other in a cycle: {0}")]
    ImageCycle(String),
    #[error("proposal {id} cannot move to stage {to}: {detail}")]
    StageRefused { id: String, to: &'static str, detail: String },
}

pub type Result<T> = std::result::Result<T, HubError>;

/// The definitions currently applied, and the commit they came from.
#[derive(Debug)]
pub struct Applied {
    pub commit: String,
    pub defs: Definitions,
    /// Degraded: the commit does not load, so only its valid part is applied. These are its
    /// errors; empty when it loads.
    pub problems: Vec<Diagnostic>,
    /// What was left out because of them (`agents/<name>`, `jobs/<name>`, `triggers/<file>`).
    pub skipped: Vec<String>,
}

/// What live viewers (web app) receive.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Live {
    Event { event: Event },
    Instance { instance: String, status: crate::phase::AgentStatus },
    Dialog { notice: crate::notify::DialogNotice },
    Proposal { proposal: Box<botler_store::Proposal> },
}

pub struct Hub {
    pub paths: Paths,
    /// Short id of this hub (a hash of its data directory). It is part of every container and
    /// network name the hub creates, so hubs sharing one container runtime never touch each
    /// other's containers.
    pub id: String,
    pub config: HubConfig,
    pub store: Store,
    pub repo: Repo,
    pub containers: Arc<dyn Containers>,
    pub secret_key: SecretKey,
    pub tokens: Tokens,
    applied: RwLock<Arc<Applied>>,
    pub limiter: Mutex<botler_core::limits::Limiter>,
    /// Woken when events are journaled.
    pub route_notify: Notify,
    /// Woken when deliveries are queued or an instance becomes idle.
    pub dispatch_notify: Notify,
    /// Woken when timers or schedules change.
    pub schedule_notify: Notify,
    /// Woken when services may need starting or stopping (apply, resume, a service exit).
    pub reconcile_notify: Notify,
    pub live: broadcast::Sender<Live>,
    /// Each watched chat's pi events (streaming text, tool calls), apart from `live`: they are
    /// many, and only a viewer of that chat wants them.
    pi: std::sync::Mutex<std::collections::HashMap<String, broadcast::Sender<serde_json::Value>>>,
    pub agents: crate::agents::Agents,
    /// Serialises image builds so the same tag is never built twice concurrently.
    pub build_lock: Mutex<()>,
    /// Serialises packing, unpacking and clearing per-instance homes against waking them.
    pub home_lock: Mutex<()>,
    /// The last storage overview (computing it walks the data directory).
    pub storage_cache: Mutex<Option<crate::storage::StorageOverview>>,
    pub search: crate::search::Index,
    pub memory: crate::memory::Memory,
    /// OpenRouter's model catalog, when it was ever fetched ([`crate::model_catalog`]).
    pub catalog: RwLock<Option<Arc<crate::model_catalog::Catalog>>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("paths", &self.paths).finish_non_exhaustive()
    }
}

impl Hub {
    /// Hand a pi event of `instance` to its viewers, if it has any.
    pub fn send_pi(&self, instance: &str, event: impl FnOnce() -> serde_json::Value) {
        let mut pi = self.pi.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tx) = pi.get(instance)
            && tx.send(event()).is_err()
        {
            // Its last viewer left.
            pi.remove(instance);
        }
    }

    /// Follow `instance`'s pi events.
    pub fn watch_pi(&self, instance: &str) -> broadcast::Receiver<serde_json::Value> {
        let mut pi = self.pi.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        pi.entry(instance.to_string()).or_insert_with(|| broadcast::channel(512).0).subscribe()
    }

    /// Assemble a hub over an existing data directory whose repo already has a main branch.
    pub async fn open(paths: Paths, config: HubConfig, containers: Arc<dyn Containers>) -> Result<Arc<Self>> {
        let stale_worktrees = crate::migrate::layout(&paths)?;
        paths.ensure()?;
        let store = Store::open(&paths.db()).await?;
        let secret_key = SecretKey::load_or_create(&paths.secret_key())?;
        let tokens = Tokens::from_secret_key(&secret_key);
        let repo = Repo::new(paths.repo(), paths.worktrees());
        if stale_worktrees {
            repo.prune_worktrees().await?;
        }
        crate::migrate::front_door(&paths, &store, &repo).await?;
        crate::migrate::model_names(&repo).await?;
        let applied = Self::load_bootable(&store, &repo).await?;
        let memory = crate::memory::Memory::open(paths.memory()).await?;
        let (live, _) = broadcast::channel(1024);
        let search = crate::search::Index::open(&paths.search_db()).await?;
        let id = paths.hub_id();
        let catalog = crate::model_catalog::load_cached(&paths.root);
        Ok(Arc::new(Self {
            id,
            paths,
            config,
            store,
            repo,
            containers,
            secret_key,
            tokens,
            applied: RwLock::new(Arc::new(applied)),
            limiter: Mutex::default(),
            route_notify: Notify::new(),
            dispatch_notify: Notify::new(),
            schedule_notify: Notify::new(),
            reconcile_notify: Notify::new(),
            live,
            pi: Default::default(),
            agents: crate::agents::Agents::default(),
            build_lock: Mutex::new(()),
            home_lock: Mutex::new(()),
            storage_cache: Mutex::new(None),
            search,
            memory,
            catalog: RwLock::new(catalog),
        }))
    }

    /// At boot: apply main if it loads. Otherwise the hub still comes up (a newer hub may
    /// validate more strictly than the one that applied main): the valid part of main, or else the
    /// last applied commit (or its valid part), with an alarm to the front door listing the
    /// problems, so an agent can fix them with a proposal. Fails only when nothing loads at all.
    async fn load_bootable(store: &Store, repo: &Repo) -> Result<Applied> {
        let main = repo.main().await?;
        let (commit, diagnostics) = match load_at(repo, &main).await {
            Ok(applied) => return Ok(applied),
            Err(HubError::InvalidDefinitions { commit, diagnostics }) => (commit, diagnostics),
            Err(e) => return Err(e),
        };
        let errors = || diagnostics.iter().filter(|d| d.severity == botler_core::defs::Severity::Error);
        let list = errors().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n");
        let (applied, text) = if let Some(a) = load_degraded(repo, &commit).await? {
            let text = format!(
                "The monorepo's main ({commit}) does not load, so botler runs without {}. Fix it with a proposal. Problems:\n{list}",
                a.skipped.join(", ")
            );
            (a, text)
        } else {
            let Some(last) = store.last_applied_commit().await?.filter(|l| *l != commit) else {
                return Err(HubError::InvalidDefinitions { commit, diagnostics });
            };
            let a = match load_at(repo, &last).await {
                Ok(a) => a,
                Err(HubError::InvalidDefinitions { commit, diagnostics }) => {
                    match load_degraded(repo, &commit).await? {
                        Some(a) => a,
                        None => return Err(HubError::InvalidDefinitions { commit, diagnostics }),
                    }
                }
                Err(e) => return Err(e),
            };
            (
                a,
                format!(
                    "The monorepo's main ({commit}) does not load, so botler is still running {last}. Problems:\n{list}"
                ),
            )
        };
        tracing::error!(%commit, running = %applied.commit, skipped = ?applied.skipped, "definitions do not load; starting degraded");
        let ev = builtin(
            BuiltinPayload::Message(MessagePayload { text, files: vec![], mode: None, via: None }),
            Source::Hub,
        )?
        .to(botler_core::FRONT_DOOR)
        .urgent(true);
        store.append_event(&ev).await?;
        Ok(applied)
    }

    pub async fn applied(&self) -> Arc<Applied> {
        self.applied.read().await.clone()
    }

    /// Switch to definitions from a newer commit. Callers reconcile afterwards.
    pub async fn apply(&self, applied: Applied) -> Result<()> {
        self.store.set_last_applied_commit(&applied.commit).await?;
        *self.applied.write().await = Arc::new(applied);
        crate::router::create_persistent_chats(self).await?;
        self.schedule_notify.notify_one();
        self.reconcile_notify.notify_one();
        // Running agents whose environment changed are restarted (`agents::refresh_environments`).
        self.dispatch_notify.notify_one();
        Ok(())
    }

    /// Journal an event and wake the router. The single entry point for everything that happens.
    pub async fn emit(&self, event: Event) -> Result<Event> {
        self.store.append_event(&event).await?;
        // An agent writing to someone who waits on it answers them (a reply, an end-of-turn notice).
        if let (Source::Agent(from), Some(to)) = (&event.source, &event.to) {
            self.store.clear_expectations(Some(to), from).await?;
        }
        if self.live.send(Live::Event { event: event.clone() }).is_err() {
            tracing::trace!("no live viewers");
        }
        self.route_notify.notify_one();
        Ok(event)
    }

    /// Check a rate limit for `subject` doing `operation`, honouring definition overrides.
    pub async fn rate_limit(
        &self,
        subject: &str,
        operation: &str,
        limit: botler_core::limits::RateLimit,
    ) -> Result<()> {
        match self.limiter.lock().await.check(subject, operation, limit, Utc::now()) {
            botler_core::limits::Decision::Allow => Ok(()),
            botler_core::limits::Decision::Deny { retry_after } => {
                Err(HubError::RateLimited { operation: operation.into(), retry_after })
            }
        }
    }

    /// Labels of a container this hub creates: its role and the owning data directory (the same
    /// owner label its images carry). Lists and cleanups filter by the owner.
    pub fn labels(&self, role: &str) -> std::collections::BTreeMap<String, String> {
        std::collections::BTreeMap::from([
            (crate::jobs::ROLE_LABEL.to_string(), role.to_string()),
            (crate::images::OWNER_LABEL.to_string(), crate::config::path_str(&self.paths.root)),
        ])
    }

    /// The user-defined network services and the agents granted them share.
    pub fn network(&self) -> String {
        format!("botler-{}", self.id)
    }

    pub async fn paused(&self) -> Result<bool> {
        Ok(self.store.paused().await?)
    }
}

/// A built-in event from `source` with its typed payload (the only way the hub builds one).
pub fn builtin(payload: BuiltinPayload, source: Source) -> Result<Event> {
    Event::builtin(payload, source, Provenance::Trusted, Utc::now()).map_err(|e| HubError::Io(std::io::Error::other(e)))
}

/// Load and validate definitions at `commit`.
pub async fn load_at(repo: &Repo, commit: &str) -> Result<Applied> {
    let tree = repo.definition_tree(commit).await?;
    let report = botler_core::load_with(&tree, crate::localtime::host());
    for d in &report.diagnostics {
        tracing::debug!(%commit, "{d}");
    }
    match report.definitions {
        Some(defs) => Ok(Applied { commit: commit.to_string(), defs, problems: vec![], skipped: vec![] }),
        None => Err(HubError::InvalidDefinitions { commit: commit.to_string(), diagnostics: report.diagnostics }),
    }
}

/// `commit`'s definitions, or their valid part when it does not load (see [`load_degraded`]).
pub async fn load_tolerant(repo: &Repo, commit: &str) -> Result<Applied> {
    match load_at(repo, commit).await {
        Err(HubError::InvalidDefinitions { commit, diagnostics }) => match load_degraded(repo, &commit).await? {
            Some(a) => Ok(a),
            None => Err(HubError::InvalidDefinitions { commit, diagnostics }),
        },
        other => other,
    }
}

/// The valid part of `commit` when it does not load (see [`botler_core::defs::load_lenient`]).
pub async fn load_degraded(repo: &Repo, commit: &str) -> Result<Option<Applied>> {
    let tree = repo.definition_tree(commit).await?;
    Ok(botler_core::defs::load_lenient(&tree, crate::localtime::host()).map(|l| Applied {
        commit: commit.to_string(),
        defs: l.definitions,
        problems: l.errors,
        skipped: l.skipped,
    }))
}
