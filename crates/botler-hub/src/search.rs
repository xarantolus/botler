//! One full-text search over sessions, the event journal, memory notes and the monorepo.
//!
//! `search.db` (SQLite FTS5) holds derived data only: the `docs` table plus `cursors` saying how
//! far each source has been read. Deleting the file rebuilds the index from scratch. The indexer
//! is incremental:
//! - **events**: ids after the cursor (UUIDv7 ids sort by time);
//! - **session files**: append-only JSONL, read from a byte offset per file (a rewritten file, e.g.
//!   by [`crate::sessions::retarget_cwd`], has a new header and is read again from the start);
//!   every entry is indexed, so messages from before a compaction stay findable;
//! - **memory**: Markdown files under `<data>/memory/`, by modification time;
//! - **the monorepo**: text files of the applied commit, replaced whenever it changes;
//! - **event types**: built-in and declared, with their producers, replaced with the monorepo.
//!
//! Each document has a fixed rowid derived from `(source, ref, owner)`, so indexing it again
//! replaces it.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use botler_core::defs::Capability;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};
use tokio::sync::{Mutex, RwLock};
use ts_rs::TS;

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::ops::{Caller, Op, OpError, Who};
use crate::sessions::Item;

/// Bump to rebuild every index on upgrade (when what is indexed changes).
const SCHEMA_VERSION: &str = "2";
/// Monorepo files larger than this are not indexed.
const MAX_REPO_FILE: u64 = 256 * 1024;
/// Text kept per document.
const MAX_BODY: usize = 64 * 1024;
const SNIPPET_CHARS: usize = 300;
const DEFAULT_LIMIT: u32 = 10;
const MAX_LIMIT: u32 = 30;
const EVENT_BATCH: u32 = 500;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search index: {0}")]
    Db(#[from] sqlx::Error),
    #[error("search index io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] botler_store::Error),
    #[error("indexing the monorepo: {0}")]
    Hub(Box<HubError>),
    /// The query cannot be run; the message says how to fix it.
    #[error("{0}")]
    Query(String),
}

impl From<SearchError> for OpError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::Query(m) => OpError::BadRequest(m),
            other => OpError::Hub(other.into()),
        }
    }
}

type Result<T, E = SearchError> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SearchSource {
    /// Messages, replies and tool output in agent sessions; `ref` is the entry id.
    Session,
    /// Memory notes; `ref` is the path, `owner` the namespace.
    Memory,
    /// The event journal; `ref` is the event id.
    Event,
    /// Files of the applied monorepo commit; `ref` is the path.
    Repo,
    /// Event types (built-in and declared); `ref` is the type.
    EventType,
}

impl SearchSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Memory => "memory",
            Self::Event => "event",
            Self::Repo => "repo",
            Self::EventType => "event_type",
        }
    }
}

impl FromStr for SearchSource {
    type Err = SearchError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "session" => Ok(Self::Session),
            "memory" => Ok(Self::Memory),
            "event" => Ok(Self::Event),
            "repo" => Ok(Self::Repo),
            "event_type" => Ok(Self::EventType),
            other => Err(SearchError::Query(format!(
                "unknown source `{other}`; sources are session, memory, event, repo and event_type"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema, TS)]
pub enum Scope {
    /// Only your own session (including before compaction) and your memory namespaces.
    #[serde(rename = "self")]
    Own,
    /// Everything you may read.
    #[serde(rename = "all")]
    All,
}

/// The `search` operation.
#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct Search {
    /// Words to find; a result contains all of them (stemmed, so `deploy` finds `deployed`).
    pub query: String,
    /// Restrict to these sources (`session`, `memory`, `event`, `repo`, `event_type`); default all.
    #[serde(default)]
    pub sources: Option<Vec<SearchSource>>,
    /// `self` searches only your own conversation (also from before compaction) and your memory.
    #[serde(default)]
    pub scope: Option<Scope>,
    /// How many results (default 10, max 30).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Treat `query` as SQLite FTS5 syntax: `"exact phrase"`, `a OR b`, `a NOT b`, `prefix*`,
    /// `NEAR(a b)`.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub raw: bool,
}

/// `GET /api/v1/search`.
#[derive(Debug, Deserialize, TS)]
pub struct SearchQuery {
    pub q: String,
    /// Comma-separated sources (`session,memory,event,repo,event_type`); default all.
    #[ts(optional)]
    pub sources: Option<String>,
    #[ts(optional)]
    pub limit: Option<u32>,
    #[ts(optional)]
    pub raw: Option<bool>,
}

impl SearchQuery {
    pub fn into_search(self) -> Result<Search> {
        let sources = self
            .sources
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.split(',').map(|x| x.trim().parse()).collect::<Result<Vec<_>>>())
            .transpose()?;
        Ok(Search { query: self.q, sources, scope: None, limit: self.limit, raw: self.raw.unwrap_or(false) })
    }
}

#[derive(Debug, Serialize, TS)]
pub struct SearchHit {
    pub source: SearchSource,
    /// Session: the entry id (`read_session(instance=owner, around=ref)`); memory and repo: the
    /// path; event: the event id; event type: the type.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Session: the instance; memory: the namespace; event: the sender; repo and event type: empty.
    pub owner: String,
    /// When it happened (RFC 3339), if known.
    pub at: Option<String>,
    pub title: String,
    /// A short excerpt with the matches in `**bold**`.
    pub snippet: String,
}

#[derive(Debug, Serialize, TS)]
pub struct SearchResults {
    pub results: Vec<SearchHit>,
}

/// The index database. Reopened (and so rebuilt) when `search.db` is deleted.
pub struct Index {
    path: PathBuf,
    pool: RwLock<SqlitePool>,
    /// One indexing pass at a time.
    pass: Mutex<()>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index").field("path", &self.path).finish_non_exhaustive()
    }
}

async fn remove_files(path: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        match tokio::fs::remove_file(&p).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

async fn connect(path: &Path) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
    let version: Option<String> =
        match sqlx::query_scalar("SELECT value FROM cursors WHERE key = 'schema'").fetch_optional(&pool).await {
            Ok(v) => v,
            // A new database: no tables yet.
            Err(sqlx::Error::Database(e)) if e.message().contains("no such table") => None,
            Err(e) => return Err(e.into()),
        };
    if version.as_deref() != Some(SCHEMA_VERSION) {
        let mut tx = pool.begin().await?;
        for stmt in [
            "DROP TABLE IF EXISTS docs",
            "DROP TABLE IF EXISTS cursors",
            "CREATE VIRTUAL TABLE docs USING fts5(source UNINDEXED, ref UNINDEXED, owner UNINDEXED, at UNINDEXED, title, body, tokenize = 'porter unicode61')",
            "CREATE TABLE cursors (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
        ] {
            sqlx::query(stmt).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO cursors (key, value) VALUES ('schema', ?)")
            .bind(SCHEMA_VERSION)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
    }
    Ok(pool)
}

impl Index {
    /// Open the index at `path`. A file that cannot be opened is derived data: it is deleted and
    /// rebuilt.
    pub async fn open(path: &Path) -> Result<Self> {
        let pool = match connect(path).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "search index unusable; rebuilding it");
                remove_files(path).await?;
                connect(path).await?
            }
        };
        Ok(Self { path: path.to_path_buf(), pool: RwLock::new(pool), pass: Mutex::new(()) })
    }

    /// The pool, reopened on a fresh file if `search.db` was deleted.
    async fn pool(&self) -> Result<SqlitePool> {
        if !tokio::fs::try_exists(&self.path).await? {
            let mut guard = self.pool.write().await;
            if !tokio::fs::try_exists(&self.path).await? {
                tracing::info!(path = %self.path.display(), "search index deleted; rebuilding it");
                guard.close().await;
                remove_files(&self.path).await?;
                *guard = connect(&self.path).await?;
            }
        }
        Ok(self.pool.read().await.clone())
    }
}

/// A stable rowid per document, so indexing it again replaces it.
fn rowid(source: SearchSource, reference: &str, owner: &str) -> i64 {
    let mut h = Sha256::new();
    for part in [source.as_str(), reference, owner] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    let d = h.finalize();
    i64::from_le_bytes([d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]])
}

struct Doc<'a> {
    source: SearchSource,
    reference: &'a str,
    owner: &'a str,
    at: Option<&'a str>,
    title: &'a str,
    body: &'a str,
}

fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

async fn put(conn: &mut sqlx::SqliteConnection, d: &Doc<'_>) -> Result<()> {
    let id = rowid(d.source, d.reference, d.owner);
    sqlx::query("DELETE FROM docs WHERE rowid = ?").bind(id).execute(&mut *conn).await?;
    sqlx::query("INSERT INTO docs (rowid, source, ref, owner, at, title, body) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(id)
        .bind(d.source.as_str())
        .bind(d.reference)
        .bind(d.owner)
        .bind(d.at)
        .bind(d.title)
        .bind(clip(d.body, MAX_BODY))
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn remove(conn: &mut sqlx::SqliteConnection, source: SearchSource, reference: &str, owner: &str) -> Result<()> {
    sqlx::query("DELETE FROM docs WHERE rowid = ?").bind(rowid(source, reference, owner)).execute(&mut *conn).await?;
    Ok(())
}

async fn cursor(conn: &mut sqlx::SqliteConnection, key: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value FROM cursors WHERE key = ?").bind(key).fetch_optional(&mut *conn).await?)
}

async fn set_cursor(conn: &mut sqlx::SqliteConnection, key: &str, value: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO cursors (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// What one indexing pass added or replaced.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Indexed {
    pub events: usize,
    pub entries: usize,
    pub memory: usize,
    pub repo: usize,
}

/// Bring the index up to date with every source.
pub async fn index_pending(hub: &Hub) -> Result<Indexed> {
    let _pass = hub.search.pass.lock().await;
    let pool = hub.search.pool().await?;
    Ok(Indexed {
        events: index_events(hub, &pool).await?,
        entries: index_sessions(&hub.paths.root.join("sessions"), &pool).await?,
        memory: index_memory(&hub.paths.memory(), &pool).await?,
        repo: index_repo(hub, &pool).await?,
    })
}

/// Every string in a payload, one per line.
fn payload_text(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        Value::Array(a) => a.iter().for_each(|x| payload_text(x, out)),
        Value::Object(o) => o.values().for_each(|x| payload_text(x, out)),
        Value::Number(n) => {
            out.push_str(&n.to_string());
            out.push('\n');
        }
        Value::Bool(_) | Value::Null => {}
    }
}

async fn index_events(hub: &Hub, pool: &SqlitePool) -> Result<usize> {
    let mut n = 0;
    let mut conn = pool.acquire().await?;
    let mut after = cursor(&mut conn, "events").await?;
    drop(conn);
    loop {
        let events = hub.store.events_after(after.as_deref(), EVENT_BATCH).await?;
        let Some(last) = events.last() else { return Ok(n) };
        let last = last.id.to_string();
        let mut tx = pool.begin().await?;
        for ev in &events {
            let mut body = String::new();
            payload_text(&ev.payload, &mut body);
            let source = ev.source.key();
            let title = match &ev.to {
                Some(to) => format!("{} from {source} to {to}", ev.kind),
                None => format!("{} from {source}", ev.kind),
            };
            let (id, at) = (ev.id.to_string(), ev.at.to_rfc3339());
            let doc = Doc {
                source: SearchSource::Event,
                reference: &id,
                owner: &source,
                at: Some(&at),
                title: &title,
                body: &body,
            };
            put(&mut tx, &doc).await?;
        }
        set_cursor(&mut tx, "events", &last).await?;
        tx.commit().await?;
        n += events.len();
        after = Some(last);
    }
}

/// The searchable title and text of a session entry.
fn entry_text(item: &Item) -> Option<(String, String, Option<&str>, &str)> {
    let (title, body, at, id) = match item {
        Item::User { id, at, text, from, .. } => {
            (format!("from {}", from.as_deref().unwrap_or("user")), text.clone(), at, id)
        }
        Item::Note { id, at, text, from, .. } => (format!("note from {from}"), text.clone(), at, id),
        Item::Assistant { id, at, text, tool_calls, .. } => {
            let mut body = text.clone();
            for c in tool_calls {
                let args = c.arguments.to_string();
                body.push_str(&format!("\n{} {}", c.name, clip(&args, 2000)));
            }
            ("assistant".to_string(), body, at, id)
        }
        Item::ToolResult { id, at, tool, text, .. } => (format!("{tool} result"), text.clone(), at, id),
        Item::Compaction { id, at, summary } => ("compaction summary".to_string(), summary.clone(), at, id),
    };
    (!body.trim().is_empty() && !id.is_empty()).then_some((title, body, at.as_deref(), id.as_str()))
}

/// Index the entries appended to every session file since the last pass.
async fn index_sessions(root: &Path, pool: &SqlitePool) -> Result<usize> {
    let mut n = 0;
    for instance in list_dir(root).await? {
        let Some(name) = instance.file_name().and_then(|s| s.to_str()).map(String::from) else { continue };
        if !tokio::fs::metadata(&instance).await?.is_dir() {
            continue;
        }
        for file in list_dir(&instance).await? {
            if file.extension().is_some_and(|e| e == "jsonl") {
                n += index_session_file(&name, &file, pool).await?;
            }
        }
    }
    Ok(n)
}

async fn list_dir(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    while let Some(e) = rd.next_entry().await? {
        out.push(e.path());
    }
    out.sort();
    Ok(out)
}

/// A fingerprint of a session file's first line (its header). Appends leave it alone; rewriting
/// the file (e.g. [`crate::sessions::retarget_cwd`]) changes it, so the file is read again.
async fn header_fingerprint(path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(4096);
    tokio::fs::File::open(path).await?.take(4096).read_to_end(&mut head).await?;
    let first = head.split(|&b| b == b'\n').next().unwrap_or_default();
    Ok(hex::encode(&Sha256::digest(first)[..8]))
}

async fn index_session_file(instance: &str, path: &Path, pool: &SqlitePool) -> Result<usize> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let key = format!("session:{instance}/{}", path.file_name().map(|f| f.to_string_lossy()).unwrap_or_default());
    let len = tokio::fs::metadata(path).await?.len();
    let fingerprint = header_fingerprint(path).await?;
    let mut conn = pool.acquire().await?;
    let saved = cursor(&mut conn, &key).await?;
    drop(conn);
    // `<header fingerprint>:<offset>`; a rewritten or shorter file is read again from the start
    // (documents are replaced, so nothing is duplicated).
    let offset = saved
        .as_deref()
        .and_then(|s| s.split_once(':'))
        .filter(|&(f, _)| f == fingerprint)
        .and_then(|(_, o)| o.parse::<u64>().ok())
        .filter(|&o| o <= len)
        .unwrap_or(0);
    if offset == len {
        return Ok(0);
    }
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).await?;
    // Only complete lines: pi may be halfway through writing the last one.
    let Some(end) = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1) else { return Ok(0) };
    let mut n = 0;
    let mut tx = pool.begin().await?;
    for line in buf[..end].split(|&b| b == b'\n') {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else { continue };
        let Some(item) = crate::sessions::parse_entry(&entry) else { continue };
        let Some((title, body, at, id)) = entry_text(&item) else { continue };
        let doc = Doc { source: SearchSource::Session, reference: id, owner: instance, at, title: &title, body: &body };
        put(&mut tx, &doc).await?;
        n += 1;
    }
    set_cursor(&mut tx, &key, &format!("{fingerprint}:{}", offset + end as u64)).await?;
    tx.commit().await?;
    Ok(n)
}

/// The namespace of a memory file (`agents/<def>`, `projects/<home>`, `user`; see
/// [`crate::memory`]), or `None` for files outside every namespace.
fn memory_namespace(rel: &str) -> Option<String> {
    let segs: Vec<&str> = rel.split('/').collect();
    match segs[..] {
        ["user", _, ..] => Some("user".into()),
        [ns @ ("agents" | "projects"), name, _, ..] => Some(format!("{ns}/{name}")),
        _ => None,
    }
}

/// Files under `root` (relative paths with `/`), skipping hidden entries such as `.git`.
async fn walk(root: &Path) -> Result<Vec<(String, std::fs::Metadata)>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for p in list_dir(&dir).await? {
            if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
                continue;
            }
            let meta = tokio::fs::symlink_metadata(&p).await?;
            if meta.is_dir() {
                stack.push(p);
            } else if meta.is_file()
                && let Ok(rel) = p.strip_prefix(root)
            {
                out.push((rel.to_string_lossy().replace('\\', "/"), meta));
            }
        }
    }
    Ok(out)
}

async fn index_memory(root: &Path, pool: &SqlitePool) -> Result<usize> {
    let mut n = 0;
    let mut seen = std::collections::HashSet::new();
    let mut tx = pool.begin().await?;
    for (rel, meta) in walk(root).await? {
        let Some(ns) = memory_namespace(&rel).filter(|_| rel.ends_with(".md")) else { continue };
        let key = format!("memory:{rel}");
        let stamp = format!(
            "{}:{}",
            meta.modified()?.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0),
            meta.len()
        );
        seen.insert(key.clone());
        if cursor(&mut tx, &key).await?.as_deref() == Some(stamp.as_str()) {
            continue;
        }
        let content = match tokio::fs::read_to_string(root.join(&rel)).await {
            Ok(c) => c,
            // Not text: nothing to search.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => String::new(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // removed meanwhile
            Err(e) => return Err(e.into()),
        };
        let at = chrono::DateTime::<chrono::Utc>::from(meta.modified()?).to_rfc3339();
        let doc = Doc {
            source: SearchSource::Memory,
            reference: &rel,
            owner: &ns,
            at: Some(&at),
            title: &rel,
            body: &content,
        };
        put(&mut tx, &doc).await?;
        set_cursor(&mut tx, &key, &stamp).await?;
        n += 1;
    }
    let known: Vec<String> =
        sqlx::query_scalar("SELECT key FROM cursors WHERE key LIKE 'memory:%'").fetch_all(&mut *tx).await?;
    for key in known.into_iter().filter(|k| !seen.contains(k)) {
        let rel = &key["memory:".len()..];
        if let Some(ns) = memory_namespace(rel) {
            remove(&mut tx, SearchSource::Memory, rel, &ns).await?;
        }
        sqlx::query("DELETE FROM cursors WHERE key = ?").bind(&key).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(n)
}

/// Replace the monorepo documents when the applied commit changed.
async fn index_repo(hub: &Hub, pool: &SqlitePool) -> Result<usize> {
    let commit = hub.applied().await.commit.clone();
    let mut conn = pool.acquire().await?;
    if cursor(&mut conn, "repo").await?.as_deref() == Some(commit.as_str()) {
        return Ok(0);
    }
    drop(conn);
    let tree = crate::images::applied_tree(hub, &commit).await.map_err(|e| SearchError::Hub(Box::new(e)))?;
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM docs WHERE source IN ('repo', 'event_type')").execute(&mut *tx).await?;
    let defs = &hub.applied().await.defs;
    for t in crate::ops::catalog(defs, &[]) {
        let mut body = format!("{}\n", t.description);
        if !t.producers.is_empty() {
            body.push_str(&format!("produced by {}\n", t.producers.join(", ")));
        }
        body.push_str(&t.subscribers.join("\n"));
        if let Some(fields) = botler_core::event_check::payload_fields(defs, &t.kind) {
            body.push_str(&format!("\nfields: {}", fields.join(", ")));
        }
        let title = format!("event type {}", t.kind);
        let doc = Doc {
            source: SearchSource::EventType,
            reference: &t.kind,
            owner: "",
            at: None,
            title: &title,
            body: &body,
        };
        put(&mut tx, &doc).await?;
    }
    let mut n = 0;
    for (rel, meta) in walk(&tree).await? {
        if meta.len() > MAX_REPO_FILE {
            continue;
        }
        let bytes = tokio::fs::read(tree.join(&rel)).await?;
        let Ok(text) = String::from_utf8(bytes) else { continue };
        if text.contains('\0') {
            continue;
        }
        let doc = Doc { source: SearchSource::Repo, reference: &rel, owner: "", at: None, title: &rel, body: &text };
        put(&mut tx, &doc).await?;
        n += 1;
    }
    set_cursor(&mut tx, "repo", &commit).await?;
    tx.commit().await?;
    Ok(n)
}

/// Keep the index current: after events and agent activity (debounced) and every minute.
pub async fn run(hub: Arc<Hub>) {
    let mut live = hub.live.subscribe();
    loop {
        match index_pending(&hub).await {
            Ok(n) if n != Indexed::default() => tracing::debug!(?n, "search index updated"),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "search indexing failed"),
        }
        tokio::select! {
            r = live.recv() => {
                if matches!(r, Err(tokio::sync::broadcast::error::RecvError::Closed)) {
                    return;
                }
                // Coalesce a burst (a streaming turn sends many pi events).
                tokio::time::sleep(Duration::from_secs(2)).await;
                live = live.resubscribe();
            }
            _ = tokio::time::sleep(Duration::from_secs(60)) => {}
        }
    }
}

/// Plain words → an FTS5 query matching all of them (each quoted, so no syntax applies).
fn plain_query(q: &str) -> String {
    q.split_whitespace().map(|w| format!("\"{}\"", w.replace('"', "\"\""))).collect::<Vec<_>>().join(" ")
}

impl Op for Search {
    const NAME: &'static str = "search";
    const DESCRIPTION: &'static str = "Full-text search over sessions, memory notes, the event journal, the monorepo and event types (`event_type`: what can happen, who emits it), best matches first. `scope=\"self\"` searches only your own conversation (including what was compacted away) and your memory. Session results: read the context with `read_session(instance=owner, around=ref)`.";
    const WHO: Who = Who::UserAndAgents;
    type Output = SearchResults;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<SearchResults, OpError> {
        search(hub, caller, self).await
    }
}

/// Run a search for `caller`: the user sees everything; an agent sees its own session (others'
/// only with `read_sessions`), its memory namespaces, the event journal and the monorepo.
pub async fn search(hub: &Hub, caller: &Caller, a: Search) -> Result<SearchResults, OpError> {
    if a.query.trim().is_empty() {
        return Err(OpError::BadRequest("the query is empty".into()));
    }
    let limit = a.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let fts = if a.raw { a.query.clone() } else { plain_query(&a.query) };

    let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT source, ref, owner, at, title, snippet(docs, 5, '**', '**', '…', 40) AS snip FROM docs WHERE docs MATCH ",
    );
    qb.push_bind(fts);
    if let Some(sources) = &a.sources {
        if sources.is_empty() {
            return Ok(SearchResults { results: vec![] });
        }
        let mut list = qb.push(" AND source IN (").separated(", ");
        for s in sources {
            list.push_bind(s.as_str());
        }
        qb.push(")");
    }
    let own = a.scope == Some(Scope::Own);
    match &caller.principal {
        Principal::User if own => return Err(OpError::BadRequest("scope `self` is for agents".into())),
        Principal::User => {}
        Principal::Run { .. } => return Err(OpError::Forbidden("search is for agents and the user".into())),
        Principal::Agent { instance, .. } => {
            let inst = hub.store.instance(instance).await?;
            let applied = hub.applied().await;
            // An instance whose definition was removed keeps its session but no memory.
            let namespaces =
                applied.defs.agents.get(&inst.definition).map(crate::memory::namespaces).unwrap_or_default();
            qb.push(" AND ((source = 'session' AND owner = ").push_bind(inst.name.clone()).push(")");
            if !own && caller.may(Capability::ReadSessions) {
                qb.push(" OR source = 'session'");
            }
            if !own {
                qb.push(" OR source IN ('event', 'repo', 'event_type')");
            }
            let mut list = qb.push(" OR (source = 'memory' AND owner IN (").separated(", ");
            for (_, ns) in namespaces {
                list.push_bind(ns);
            }
            qb.push(")))");
        }
    }
    qb.push(" ORDER BY bm25(docs, 0, 0, 0, 0, 4.0, 1.0) LIMIT ").push_bind(limit);

    let pool = hub.search.pool().await?;
    let rows = qb.build().fetch_all(&pool).await.map_err(|e| match e {
        sqlx::Error::Database(db) if a.raw => SearchError::Query(format!(
            "invalid search syntax ({}). Use words, \"exact phrases\", AND, OR, NOT, prefix* and NEAR(a b); \
             or leave out `raw` to search for the words as written.",
            db.message()
        )),
        other => other.into(),
    })?;
    let mut results = Vec::with_capacity(rows.len());
    for r in rows {
        let source: String = r.try_get("source").map_err(SearchError::from)?;
        let snip: String = r.try_get("snip").map_err(SearchError::from)?;
        results.push(SearchHit {
            source: source.parse()?,
            reference: r.try_get("ref").map_err(SearchError::from)?,
            owner: r.try_get("owner").map_err(SearchError::from)?,
            at: r.try_get("at").map_err(SearchError::from)?,
            title: r.try_get("title").map_err(SearchError::from)?,
            snippet: snippet(&snip),
        });
    }
    Ok(SearchResults { results })
}

/// One line of at most [`SNIPPET_CHARS`].
fn snippet(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(SNIPPET_CHARS) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_queries_are_quoted() {
        assert_eq!(plain_query(r#"foo  "bar" OR baz*"#), r#""foo" """bar""" "OR" "baz*""#);
    }

    #[test]
    fn namespaces() {
        assert_eq!(memory_namespace("agents/dev/github.md").as_deref(), Some("agents/dev"));
        assert_eq!(memory_namespace("projects/gs/a/b.md").as_deref(), Some("projects/gs"));
        assert_eq!(memory_namespace("user/prefs.md").as_deref(), Some("user"));
        assert_eq!(memory_namespace("agents/x.md"), None);
        assert_eq!(memory_namespace("README.md"), None);
    }

    #[test]
    fn snippets_are_short() {
        let long = "word ".repeat(200);
        let s = snippet(&long);
        assert!(s.chars().count() <= SNIPPET_CHARS + 1, "{}", s.len());
        assert_eq!(snippet("a\n  b"), "a b");
    }
}
