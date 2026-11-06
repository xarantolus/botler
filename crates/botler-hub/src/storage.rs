//! Disk space: gitignore-aware listings, cleaning and packing the homes of archived chats,
//! archiving dormant chats automatically, the storage overview and garbage collection.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use botler_core::Source;
use botler_core::limits::RateLimit;
use botler_core::payloads::{ArchiveRequestedPayload, BuiltinPayload};
use botler_store::{ArchiveRecord, Instance, InstanceState};
use chrono::{DateTime, Utc};
use serde::Serialize;
use ts_rs::TS;

use crate::hub::{Hub, HubError, Result};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{tool} failed in {}: {detail}", dir.display())]
    Tool { tool: &'static str, dir: PathBuf, detail: String },
    #[error("zstd is not installed; archived chats' homes cannot be packed or unpacked")]
    NoZstd,
    #[error("storage task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

type SResult<T> = std::result::Result<T, StorageError>;

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> StorageError + '_ {
    move |source| StorageError::Io { path: path.to_path_buf(), source }
}

/// Run blocking file work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> SResult<T> + Send + 'static) -> Result<T> {
    Ok(tokio::task::spawn_blocking(f).await.map_err(StorageError::from)??)
}

/// Bytes on disk under `path` (not following symlinks); 0 if it does not exist.
pub fn disk_usage(path: &Path) -> SResult<u64> {
    let meta = match path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io(path)(e)),
    };
    #[cfg(unix)]
    let mut n = std::os::unix::fs::MetadataExt::blocks(&meta) * 512;
    #[cfg(not(unix))]
    let mut n = meta.len();
    if meta.is_dir() {
        // A directory removed while we walk counts as empty.
        for entry in read_dir_if_exists(path)? {
            n += disk_usage(&entry.path())?;
        }
    }
    Ok(n)
}

/// The entries of `dir`; none if it does not exist.
fn read_dir_if_exists(dir: &Path) -> SResult<Vec<std::fs::DirEntry>> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries.collect::<std::io::Result<_>>().map_err(io(dir)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(io(dir)(e)),
    }
}

/// Remove a file or directory tree (a symlink itself, never its target); fine if it is gone.
fn remove_path(path: &Path) -> SResult<()> {
    let r = match path.symlink_metadata() {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) => Err(e),
    };
    match r {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io(path)(e)),
        _ => Ok(()),
    }
}

/// What git ignores in every git checkout under `root` (each with its own `.git` directory),
/// as absolute paths; an ignored directory is listed once, not its contents. Symlinks are not
/// followed, and git runs with the checkout's config neutralised so files an agent wrote can never
/// make the hub run a command.
pub fn ignored_paths(root: &Path) -> SResult<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk_checkouts(root, &mut out)?;
    // git can list a directory and files in it; keep the directory.
    out.sort();
    let mut kept: Vec<PathBuf> = Vec::with_capacity(out.len());
    for p in out {
        if !kept.last().is_some_and(|k| p.starts_with(k)) {
            kept.push(p);
        }
    }
    Ok(kept)
}

fn walk_checkouts(dir: &Path, out: &mut Vec<PathBuf>) -> SResult<()> {
    let start = out.len();
    if dir.join(".git").symlink_metadata().is_ok_and(|m| m.is_dir()) {
        out.extend(git_ignored(dir)?);
    }
    for entry in read_dir_if_exists(dir)? {
        let path = entry.path();
        let ignored = out[start..].contains(&path);
        if entry.file_type().map_err(io(&path))?.is_dir() && entry.file_name() != ".git" && !ignored {
            walk_checkouts(&path, out)?;
        }
    }
    Ok(())
}

fn git_ignored(dir: &Path) -> SResult<Vec<PathBuf>> {
    let tool_err = |detail: String| StorageError::Tool { tool: "git ls-files", dir: dir.to_path_buf(), detail };
    let out = Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null"])
        .args(["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"])
        .current_dir(dir)
        .env("GIT_DIR", dir.join(".git"))
        .env("GIT_WORK_TREE", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .map_err(|e| tool_err(e.to_string()))?;
    if !out.status.success() {
        return Err(tool_err(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    let mut paths = Vec::new();
    for raw in out.stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rel = Path::new(std::str::from_utf8(raw).map_err(|e| tool_err(e.to_string()))?.trim_end_matches('/'));
        if !rel.components().all(|c| matches!(c, std::path::Component::Normal(_))) {
            return Err(tool_err(format!("unexpected path {}", rel.display())));
        }
        paths.push(dir.join(rel));
    }
    Ok(paths)
}

/// Delete what is rebuildable under `dir`: gitignored files in its git checkouts, plus `extra`
/// (relative paths, e.g. `.cache`). Returns the bytes freed.
pub fn clear_rebuildable(dir: &Path, extra: &[&str]) -> SResult<u64> {
    let mut targets = ignored_paths(dir)?;
    targets.extend(extra.iter().map(|e| dir.join(e)));
    let mut freed = 0;
    for t in targets {
        freed += disk_usage(&t)?;
        remove_path(&t)?;
    }
    Ok(freed)
}

fn require_zstd() -> SResult<()> {
    match Command::new("zstd").arg("--version").output() {
        Ok(o) if o.status.success() => Ok(()),
        _ => Err(StorageError::NoZstd),
    }
}

fn tar(dir: &Path, args: &[&std::ffi::OsStr]) -> SResult<()> {
    let out = Command::new("tar").args(args).output().map_err(io(dir))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(StorageError::Tool { tool: "tar", dir: dir.to_path_buf(), detail });
    }
    Ok(())
}

/// Where an archived chat's packed home lives.
pub fn packed_home(hub: &Hub, instance: &str) -> PathBuf {
    hub.paths.root.join("homes").join(format!("{instance}.tar.zst"))
}

/// Pack `home` into `archive` (tar + zstd) and remove `home`.
fn pack(home: &Path, archive: &Path) -> SResult<()> {
    require_zstd()?;
    let partial = archive.with_extension("partial");
    tar(
        home,
        &["--zstd".as_ref(), "-cf".as_ref(), partial.as_os_str(), "-C".as_ref(), home.as_os_str(), ".".as_ref()],
    )?;
    std::fs::rename(&partial, archive).map_err(io(archive))?;
    remove_path(home)
}

/// Unpack `archive` into `home` and remove `archive`.
fn unpack(archive: &Path, home: &Path) -> SResult<()> {
    require_zstd()?;
    let partial = home.with_extension("unpacking");
    remove_path(&partial)?;
    std::fs::create_dir_all(&partial).map_err(io(&partial))?;
    tar(home, &["--zstd".as_ref(), "-xf".as_ref(), archive.as_os_str(), "-C".as_ref(), partial.as_os_str()])?;
    std::fs::rename(&partial, home).map_err(io(home))?;
    remove_path(archive)
}

/// Before a chat wakes: unpack its home if archiving packed it.
pub async fn unpack_home(hub: &Hub, instance: &str) -> Result<()> {
    let archive = packed_home(hub, instance);
    if !tokio::fs::try_exists(&archive).await? {
        return Ok(());
    }
    let _guard = hub.home_lock.lock().await;
    let home = hub.paths.home(instance);
    tracing::info!(instance, "unpacking the archived home");
    blocking(move || if archive.exists() { unpack(&archive, &home) } else { Ok(()) }).await
}

/// Is the instance's container still around (e.g. stopping after its handover)?
async fn is_live(hub: &Hub, instance: &str) -> bool {
    hub.agents.get(instance).await.is_some()
}

/// Clean and pack the home of an archived chat, unless it was restored or is still stopping.
/// Returns the bytes freed, or `None` if there was nothing to do. Only the chat's own home is
/// packed: shared homes belong to every chat of their definition and stay as they are.
pub async fn pack_archived(hub: &Hub, instance: &str) -> Result<Option<u64>> {
    let home = hub.paths.home(instance);
    let _guard = hub.home_lock.lock().await;
    let state = hub.store.instance(instance).await?.state;
    if state != InstanceState::Archived || is_live(hub, instance).await || !tokio::fs::try_exists(&home).await? {
        return Ok(None);
    }
    let (archive, workspace) = (packed_home(hub, instance), hub.paths.workspace(instance));
    let freed = blocking(move || {
        let before = disk_usage(&home)? + disk_usage(&workspace)?;
        clear_rebuildable(&workspace, &[])?;
        clear_rebuildable(&home, &[".cache"])?;
        pack(&home, &archive)?;
        Ok(before.saturating_sub(disk_usage(&archive)? + disk_usage(&workspace)?))
    })
    .await?;
    hub.store.set_archive_freed(instance, freed).await?;
    tracing::info!(instance, freed, "archived chat's home cleaned and packed");
    Ok(Some(freed))
}

/// Delete a dormant chat's rebuildable files (gitignored files, `~/.cache`) without archiving it.
pub async fn clear_chat_caches(hub: &Hub, instance: &str) -> Result<u64> {
    let _guard = hub.home_lock.lock().await;
    if is_live(hub, instance).await {
        return Err(HubError::Forbidden(format!("{instance} is running; clear its caches while it sleeps")));
    }
    let (home, workspace) = (hub.paths.home(instance), hub.paths.workspace(instance));
    blocking(move || Ok(clear_rebuildable(&workspace, &[])? + clear_rebuildable(&home, &[".cache"])?)).await
}

/// Delete an archived chat for good: its home (or packed home), workspace, session files,
/// rendered pi config, search entries and the store rows only it uses. Events stay.
pub async fn delete_chat(hub: &Hub, instance: &str) -> Result<u64> {
    let _guard = hub.home_lock.lock().await;
    if is_live(hub, instance).await {
        return Err(HubError::Forbidden(format!("{instance} is still stopping; try again in a moment")));
    }
    if !hub.store.delete_archived_instance(instance).await? {
        return Err(HubError::Forbidden(format!("{instance} is not archived; archive it first")));
    }
    let dirs = [
        hub.paths.home(instance),
        packed_home(hub, instance),
        hub.paths.workspace(instance),
        hub.paths.sessions(instance),
        hub.paths.agent_dir(instance),
    ];
    let freed = blocking(move || {
        let mut freed = 0;
        for d in &dirs {
            freed += disk_usage(d)?;
            remove_path(d)?;
        }
        Ok(freed)
    })
    .await?;
    hub.search.forget_sessions(instance).await?;
    tracing::info!(instance, freed, "archived chat deleted");
    Ok(freed)
}

/// Ask a chat to hand over and archive itself (`archive.requested`; it wakes to answer). A chat
/// whose agent writes no handover (reviewers, triage) is archived right away instead.
pub async fn request_archive(hub: &Hub, instance: &str, reason: Option<String>, source: Source) -> Result<()> {
    if !hands_over(hub, instance).await? {
        return force_archive(hub, instance, reason).await;
    }
    hub.store.request_archive(instance).await?;
    let ev = crate::hub::builtin(BuiltinPayload::ArchiveRequested(ArchiveRequestedPayload { reason }), source)?
        .to(instance.to_string());
    hub.emit(ev).await?;
    Ok(())
}

async fn hands_over(hub: &Hub, instance: &str) -> Result<bool> {
    let inst = hub.store.instance(instance).await?;
    Ok(hub.applied().await.defs.agents.get(&inst.definition).is_none_or(|d| d.spec.hands_over()))
}

/// Archive without the agent's handover.
pub async fn force_archive(hub: &Hub, instance: &str, reason: Option<String>) -> Result<()> {
    let record = ArchiveRecord {
        confirmed_by_agent: false,
        forced: true,
        reason,
        report: None,
        handed_to: None,
        at: Utc::now(),
        freed_bytes: None,
    };
    hub.store.archive_instance(instance, &record).await?;
    crate::agents::stop_after_reply(hub, instance).await;
    Ok(())
}

/// At most this many dormant chats are asked to hand over per hour.
pub const AUTO_ARCHIVE_PER_HOUR: u32 = 4;
/// A chat of an agent without a handover (a reviewer, triage) quiet this long is done.
pub const DONE_AFTER: Duration = Duration::from_secs(3600);
/// A handover asked for this long ago that never came is replaced by a forced archive.
pub const HANDOVER_WAIT: Duration = Duration::from_secs(24 * 3600);

/// Ask chats dormant longer than `auto_archive_after` to hand over (a few per hour), and archive
/// those whose handover never came. Persistent and always-on agents are never archived here.
pub async fn auto_archive(hub: &Hub, now: DateTime<Utc>) -> Result<()> {
    if hub.paused().await? {
        return Ok(());
    }
    let applied = hub.applied().await;
    let wait = chrono::Duration::from_std(HANDOVER_WAIT).unwrap_or(chrono::Duration::MAX);
    for inst in hub.store.stale_archive_requests(now - wait).await? {
        if hub.agents.get(&inst.name).await.is_some_and(|a| a.busy()) {
            continue;
        }
        tracing::info!(instance = %inst.name, "no handover within a day; archiving without it");
        force_archive(hub, &inst.name, Some("no handover within a day of the archive request".into())).await?;
    }
    // Done helpers (no handover): quiet for an hour, asked nothing and owed no answer.
    let quiet = chrono::Duration::from_std(DONE_AFTER).unwrap_or(chrono::Duration::MAX);
    let open = hub.store.open_dialogs().await?;
    for inst in hub.store.archivable_instances(now - quiet).await? {
        if applied.defs.agents.get(&inst.definition).is_none_or(|d| d.spec.hands_over())
            || open.iter().any(|d| d.instance == inst.name)
            || !hub.store.expectations_on(&inst.name).await?.is_empty()
            || !hub.store.pending_deliveries(&inst.name).await?.is_empty()
        {
            continue;
        }
        tracing::info!(instance = %inst.name, "done; archiving it (its agent writes no handover)");
        force_archive(hub, &inst.name, Some("done: quiet for an hour".into())).await?;
    }
    let Some(after) = applied.defs.instance.defaults.auto_archive_after else { return Ok(()) };
    let limit = RateLimit::new(AUTO_ARCHIVE_PER_HOUR, 3600);
    for inst in hub.store.archivable_instances(now - after.as_chrono()).await? {
        if applied.defs.agents.get(&inst.definition).is_some_and(|d| d.spec.always_on) {
            continue;
        }
        match hub.rate_limit("hub", "auto_archive", limit).await {
            Ok(()) => {}
            // The rest wait for a later sweep.
            Err(HubError::RateLimited { .. }) => break,
            Err(e) => return Err(e),
        }
        tracing::info!(instance = %inst.name, "dormant too long; asking it to hand over");
        let reason = format!("dormant for more than {after} (auto_archive_after)");
        request_archive(hub, &inst.name, Some(reason), Source::Hub).await?;
    }
    Ok(())
}

/// One storage pass: auto-archive, then clean and pack archived chats' homes. A chat that
/// cannot be packed is logged and retried on the next pass.
pub async fn sweep(hub: &Hub, now: DateTime<Utc>) -> Result<()> {
    auto_archive(hub, now).await?;
    for inst in hub.store.all_instances(u32::MAX).await? {
        if inst.state == InstanceState::Archived
            && let Err(e) = pack_archived(hub, &inst.name).await
        {
            tracing::error!(instance = %inst.name, error = %e, "could not pack the archived chat's home");
        }
    }
    Ok(())
}

pub async fn run(hub: Arc<Hub>) {
    loop {
        tokio::time::sleep(Duration::from_secs(600)).await;
        if let Err(e) = sweep(&hub, Utc::now()).await {
            tracing::error!(error = %e, "storage sweep failed");
        }
    }
}

/// Image and export garbage collection now, plus run logs older than `logs_older_than`.
/// Returns the bytes freed on disk (image space is the runtime's and not counted).
pub async fn collect_garbage(hub: &Hub, logs_older_than: Duration) -> Result<u64> {
    let (exports, logs) = (hub.paths.exports(), hub.paths.root.join("logs"));
    let measure = |a: PathBuf, b: PathBuf| blocking(move || Ok(disk_usage(&a)? + disk_usage(&b)?));
    let before = measure(exports.clone(), logs.clone()).await?;
    crate::images::collect_garbage(hub).await?;
    let dir = logs.clone();
    blocking(move || {
        for entry in read_dir_if_exists(&dir)? {
            let path = entry.path();
            let modified = path.symlink_metadata().and_then(|m| m.modified()).map_err(io(&path))?;
            if modified.elapsed().is_ok_and(|age| age > logs_older_than) {
                remove_path(&path)?;
            }
        }
        Ok(())
    })
    .await?;
    Ok(before.saturating_sub(measure(exports, logs).await?))
}

/// Disk use of the data directory, by chat and by kind, largest first.
#[derive(Clone, Debug, Serialize, TS)]
pub struct StorageOverview {
    pub computed_at: DateTime<Utc>,
    /// Everything listed, chats and items.
    pub total_bytes: u64,
    pub chats: Vec<ChatStorage>,
    pub items: Vec<StorageItem>,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct ChatStorage {
    pub instance: String,
    pub title: Option<String>,
    pub state: InstanceState,
    /// Never archived automatically and cannot be deleted.
    pub persistent: bool,
    /// Its own home, unpacked or packed (`packed`).
    pub home_bytes: u64,
    pub packed: bool,
    /// Its monorepo clone.
    pub workspace_bytes: u64,
    pub session_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, TS)]
pub struct StorageItem {
    pub kind: StorageKind,
    /// The shared home's or image's name; empty for the others.
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    SharedHome,
    /// A container image the hub built (shared layers are counted in every image).
    Image,
    /// Build contexts and pi resources exported from the monorepo (rebuilt on demand).
    Exports,
    /// Logs of finished runs.
    Logs,
    /// The full-text search index (rebuilt when deleted).
    Search,
    Memory,
    /// `hub.db`.
    Database,
    /// `repo.git`.
    Monorepo,
    Uploads,
}

/// The cached overview, computed if there is none or `refresh` is set.
pub async fn overview(hub: &Hub, refresh: bool) -> Result<StorageOverview> {
    let mut cache = hub.storage_cache.lock().await;
    if let Some(o) = cache.as_ref().filter(|_| !refresh) {
        return Ok(o.clone());
    }
    let o = compute_overview(hub).await?;
    *cache = Some(o.clone());
    Ok(o)
}

async fn compute_overview(hub: &Hub) -> Result<StorageOverview> {
    let instances = hub.store.all_instances(u32::MAX).await?;
    let root = hub.paths.root.clone();
    let paths: Vec<(Instance, [PathBuf; 4])> = instances
        .into_iter()
        .map(|i| {
            let p = [
                hub.paths.home(&i.name),
                packed_home(hub, &i.name),
                hub.paths.workspace(&i.name),
                hub.paths.sessions(&i.name),
            ];
            (i, p)
        })
        .collect();
    let (chats, mut items) = blocking(move || {
        let mut chats = Vec::new();
        for (i, [home, packed, workspace, sessions]) in paths {
            let packed_bytes = disk_usage(&packed)?;
            let home_bytes = disk_usage(&home)? + packed_bytes;
            let (workspace_bytes, session_bytes) = (disk_usage(&workspace)?, disk_usage(&sessions)?);
            chats.push(ChatStorage {
                instance: i.name,
                title: i.title,
                state: i.state,
                persistent: i.persistent,
                home_bytes,
                packed: packed_bytes > 0,
                workspace_bytes,
                session_bytes,
                total_bytes: home_bytes + workspace_bytes + session_bytes,
            });
        }
        let mut items = Vec::new();
        let shared = root.join("shared-homes");
        for e in read_dir_if_exists(&shared)? {
            let name = e.file_name().to_string_lossy().into_owned();
            items.push(StorageItem { kind: StorageKind::SharedHome, name, bytes: disk_usage(&e.path())? });
        }
        for (kind, rel) in [
            (StorageKind::Exports, "exports"),
            (StorageKind::Logs, "logs"),
            (StorageKind::Search, "search.db"),
            (StorageKind::Memory, "memory"),
            (StorageKind::Database, "hub.db"),
            (StorageKind::Monorepo, "repo.git"),
            (StorageKind::Uploads, "uploads"),
        ] {
            // SQLite keeps recent writes in `-wal` next to the database.
            let wal = root.join(format!("{rel}-wal"));
            items.push(StorageItem {
                kind,
                name: String::new(),
                bytes: disk_usage(&root.join(rel))? + disk_usage(&wal)?,
            });
        }
        Ok((chats, items))
    })
    .await?;
    let owner = std::collections::BTreeMap::from([(crate::images::OWNER_LABEL.to_string(), crate::images::owner(hub))]);
    for tag in hub.containers.images(&owner).await? {
        let bytes = hub.containers.image_size(&tag).await?;
        items.push(StorageItem { kind: StorageKind::Image, name: tag, bytes });
    }
    let mut chats = chats;
    chats.sort_by_key(|c| std::cmp::Reverse(c.total_bytes));
    items.sort_by_key(|i| std::cmp::Reverse(i.bytes));
    let total_bytes = chats.iter().map(|c| c.total_bytes).sum::<u64>() + items.iter().map(|i| i.bytes).sum::<u64>();
    Ok(StorageOverview { computed_at: Utc::now(), total_bytes, chats, items })
}
