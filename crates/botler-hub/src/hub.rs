//! The hub: shared state and the operations every subsystem builds on.

use std::sync::Arc;

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
    #[error("{instance} keeps crashing; next start allowed at {until}")]
    Backoff { instance: String, until: chrono::DateTime<chrono::Utc> },
}

pub type Result<T> = std::result::Result<T, HubError>;

/// The definitions currently applied, and the commit they came from.
#[derive(Debug)]
pub struct Applied {
    pub commit: String,
    pub defs: Definitions,
}

/// What live viewers (web app) receive.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Live {
    Event { event: Event },
    Pi { instance: String, event: serde_json::Value },
    Instance { instance: String, state: String },
    Dialog { notice: crate::notify::DialogNotice },
}

pub struct Hub {
    pub paths: Paths,
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
    pub live: broadcast::Sender<Live>,
    pub agents: crate::agents::Agents,
    /// Serialises image builds so the same tag is never built twice concurrently.
    pub build_lock: Mutex<()>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("paths", &self.paths).finish_non_exhaustive()
    }
}

impl Hub {
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
        let applied = Self::load_bootable(&store, &repo).await?;
        let (live, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
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
            live,
            agents: crate::agents::Agents::default(),
            build_lock: Mutex::new(()),
        }))
    }

    /// At boot: apply main if it loads; otherwise fall back to the last applied commit and raise
    /// an alarm, so a broken main never takes the whole system down.
    async fn load_bootable(store: &Store, repo: &Repo) -> Result<Applied> {
        let main = repo.main().await?;
        match load_at(repo, &main).await {
            Ok(applied) => Ok(applied),
            Err(HubError::InvalidDefinitions { commit, diagnostics }) => {
                let Some(last) = store.last_applied_commit().await? else {
                    return Err(HubError::InvalidDefinitions { commit, diagnostics });
                };
                tracing::error!(%commit, fallback = %last, problems = diagnostics.len(), "main does not load; staying on last applied commit");
                let ev = Event::new(botler_core::event::kinds::MESSAGE, Source::Hub, Provenance::Trusted, Utc::now())
                    .to(botler_core::FRONT_DOOR)
                    .urgent(true)
                    .payload(serde_json::json!({
                        "text": format!(
                            "The monorepo's main ({commit}) does not load, so botler is still running {last}. Problems:\n{}",
                            diagnostics.iter().map(|d| format!("- {d}")).collect::<Vec<_>>().join("\n")
                        )
                    }));
                store.append_event(&ev).await?;
                load_at(repo, &last).await
            }
            Err(e) => Err(e),
        }
    }

    pub async fn applied(&self) -> Arc<Applied> {
        self.applied.read().await.clone()
    }

    /// Switch to definitions from a newer commit. Callers reconcile afterwards.
    pub async fn apply(&self, applied: Applied) -> Result<()> {
        self.store.set_last_applied_commit(&applied.commit).await?;
        *self.applied.write().await = Arc::new(applied);
        self.schedule_notify.notify_one();
        Ok(())
    }

    /// Journal an event and wake the router. The single entry point for everything that happens.
    pub async fn emit(&self, event: Event) -> Result<Event> {
        self.store.append_event(&event).await?;
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

    pub async fn paused(&self) -> Result<bool> {
        Ok(self.store.paused().await?)
    }
}

/// Load and validate definitions at `commit`.
pub async fn load_at(repo: &Repo, commit: &str) -> Result<Applied> {
    let tree = repo.definition_tree(commit).await?;
    let report = botler_core::load_with(&tree, crate::localtime::host());
    for d in &report.diagnostics {
        tracing::debug!(%commit, "{d}");
    }
    match report.definitions {
        Some(defs) => Ok(Applied { commit: commit.to_string(), defs }),
        None => Err(HubError::InvalidDefinitions { commit: commit.to_string(), diagnostics: report.diagnostics }),
    }
}
