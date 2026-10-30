//! Agent memory: plain Markdown notes under `<data>/memory/`, with a git repo there for history.
//!
//! Namespaces are directories: `agents/<definition>/` (shared by every instance of a
//! definition), `projects/<home>/` (agents sharing the home `<home>`) and `user/` (the user
//! profile and global facts). Agents read theirs through read-only mounts (`/memory/self`,
//! `/memory/project`, `/memory/user`; the repo's `.git` sits above them and is never mounted) and
//! change them through the `memory_*` ops. The hub is the only writer: one lock serialises
//! writes, each write is a temp file plus rename and a commit authored by the caller, and writes
//! carry the sha256 of the version they replace, so concurrent writers never silently overwrite
//! each other.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use botler_core::defs::AgentDef;
use botler_runtime::Mount;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::Mutex;
use ts_rs::TS;

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::ops::{Caller, OpError, OpResult, args, to_json};

/// Largest memory file.
pub const MAX_BYTES: usize = 64 * 1024;
const MAX_PATH: usize = 200;
const MAX_DEPTH: usize = 8;
/// How much of each `INDEX.md` goes into an agent's system prompt.
const INDEX_BYTES: usize = 4 * 1024;
const HISTORY: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("invalid memory path `{path}`: {reason}")]
    InvalidPath { path: String, reason: &'static str },
    #[error("{path} would be {bytes} bytes; a memory file holds at most {MAX_BYTES}")]
    TooLarge { path: String, bytes: usize },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("you may not {action} {path}; your memory namespaces are: {allowed}")]
    Forbidden { action: &'static str, path: String, allowed: String },
    #[error("{0} does not exist")]
    NotFound(String),
    #[error("{0}")]
    Conflict(Box<MemoryConflict>),
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("unexpected git output: {0}")]
    Malformed(String),
    #[error("memory io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<MemoryError> for OpError {
    fn from(e: MemoryError) -> Self {
        Self::Hub(e.into())
    }
}

/// A write based on a version that is no longer current.
#[derive(Debug, Clone, Serialize, TS)]
pub struct MemoryConflict {
    pub path: String,
    /// The current version; `None` when the file does not exist.
    pub current: Option<MemoryFile>,
}

impl std::fmt::Display for MemoryConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.current {
            None => write!(f, "conflict: {} does not exist; write it without expected_sha256 to create it", self.path),
            Some(c) => write!(
                f,
                "conflict: {} is not the version you expected (current sha256 {}). Merge your change into the \
                 current content below and write again with expected_sha256 = \"{}\".\n--- current content ---\n{}",
                self.path, c.sha256, c.sha256, c.content
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
pub struct MemoryFile {
    pub path: String,
    pub sha256: String,
    pub content: String,
}

#[derive(Debug, Serialize, TS)]
pub struct MemoryWritten {
    pub path: String,
    /// The sha256 of the new content: pass it as `expected_sha256` to change the file again.
    pub sha256: String,
}

#[derive(Debug, Serialize, TS)]
pub struct MemoryEntry {
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize, TS)]
pub struct MemoryCommit {
    pub commit: String,
    /// The instance (or `user`) that made the change.
    pub author: String,
    pub at: DateTime<Utc>,
    pub message: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct MemoryWrite {
    /// e.g. `agents/dev/github.md`, `projects/groundstation/INDEX.md`, `user/preferences.md`.
    pub path: String,
    /// The whole new content (Markdown).
    pub content: String,
    /// sha256 of the version you are replacing (`sha256sum <file>`); omit to create a new file.
    #[serde(default)]
    #[ts(optional, as = "Option<String>")]
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct MemoryAppend {
    pub path: String,
    /// Added at the end, on its own line(s). Creates the file if needed.
    pub text: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct MemoryDelete {
    pub path: String,
    /// sha256 of the version you are deleting.
    pub expected_sha256: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct MemoryPath {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct MemoryList {
    /// A directory such as `user` or `agents/dev`; omit for every file you may read.
    #[serde(default)]
    #[ts(optional, as = "Option<String>")]
    pub prefix: Option<String>,
}

pub fn sha256(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Check a memory path; `file` paths must name a `.md` file inside a namespace.
fn check_path(path: &str, file: bool) -> Result<(), MemoryError> {
    let bad = |reason| Err(MemoryError::InvalidPath { path: path.into(), reason });
    if path.len() > MAX_PATH {
        return bad("longer than 200 characters");
    }
    let segs: Vec<&str> = path.split('/').collect();
    if segs.len() > MAX_DEPTH {
        return bad("nested too deeply");
    }
    let seg_ok = |s: &&str| {
        !s.is_empty() && !s.starts_with('.') && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if !segs.iter().all(seg_ok) {
        return bad(
            "use relative paths of letters, digits, `-`, `_` and `.`, with no segment starting with `.` (no `..`)",
        );
    }
    let ns_len = match segs[0] {
        "user" => 1,
        "agents" | "projects" => 2,
        _ => return bad("must start with agents/<definition>/, projects/<home>/ or user/"),
    };
    if file {
        if segs.len() <= ns_len {
            return bad("name a file inside the namespace");
        }
        let name = segs[segs.len() - 1];
        if !name.ends_with(".md") || name.len() <= 3 {
            return bad("memory files are Markdown (`.md`)");
        }
    }
    Ok(())
}

/// The namespace of a checked path: `user`, `agents/<def>` or `projects/<home>`.
fn namespace_of(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    match segs[0] {
        "user" => "user".into(),
        _ => segs.iter().take(2).copied().collect::<Vec<_>>().join("/"),
    }
}

/// An agent's namespaces as `(mount name, namespace)`: they are mounted at `/memory/<name>`.
pub fn namespaces(def: &AgentDef) -> Vec<(&'static str, String)> {
    let mut ns = vec![("self", format!("agents/{}", def.name))];
    if let Some(home) = &def.spec.home {
        ns.push(("project", format!("projects/{home}")));
    }
    ns.push(("user", "user".into()));
    ns
}

fn too_large(path: &str, content: &str) -> Result<(), MemoryError> {
    if content.len() > MAX_BYTES {
        return Err(MemoryError::TooLarge { path: path.into(), bytes: content.len() });
    }
    Ok(())
}

/// The memory directory and its write lock.
#[derive(Debug)]
pub struct Memory {
    root: PathBuf,
    lock: Mutex<()>,
}

impl Memory {
    /// Open `root`, creating it and its git repo if needed.
    pub async fn open(root: PathBuf) -> Result<Self, MemoryError> {
        tokio::fs::create_dir_all(&root).await?;
        let memory = Self { root, lock: Mutex::new(()) };
        if !memory.root.join(".git").exists() {
            memory.git(&["init", "-q", "-b", "main"]).await?;
        }
        Ok(memory)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn git(&self, args: &[&str]) -> Result<String, MemoryError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("GIT_COMMITTER_NAME", "botler hub")
            .env("GIT_COMMITTER_EMAIL", "hub@botler.invalid")
            .stdin(Stdio::null())
            .output()
            .await?;
        if !out.status.success() {
            return Err(MemoryError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub async fn read(&self, path: &str) -> Result<Option<MemoryFile>, MemoryError> {
        check_path(path, true)?;
        let bytes = match tokio::fs::read(self.root.join(path)).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let content = String::from_utf8(bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Some(MemoryFile { path: path.into(), sha256: sha256(&content), content }))
    }

    /// Replace (or with `expected = None`, create) `path`.
    pub async fn write(
        &self,
        author: &str,
        path: &str,
        content: &str,
        expected: Option<&str>,
    ) -> Result<MemoryWritten, MemoryError> {
        check_path(path, true)?;
        too_large(path, content)?;
        let _write = self.lock.lock().await;
        let current = self.read(path).await?;
        let matches = match (expected, &current) {
            (None, None) => true,
            (Some(e), Some(c)) => c.sha256 == e,
            _ => false,
        };
        if !matches {
            return Err(MemoryError::Conflict(Box::new(MemoryConflict { path: path.into(), current })));
        }
        if current.is_none_or(|c| c.content != content) {
            self.put(path, content).await?;
            self.commit(author, &format!("write {path}")).await?;
        }
        Ok(MemoryWritten { path: path.into(), sha256: sha256(content) })
    }

    /// Add `text` as new line(s) at the end of `path`, creating it if needed.
    pub async fn append(&self, author: &str, path: &str, text: &str) -> Result<MemoryWritten, MemoryError> {
        check_path(path, true)?;
        if text.trim().is_empty() {
            return Err(MemoryError::Invalid("nothing to append"));
        }
        let _write = self.lock.lock().await;
        let mut content = self.read(path).await?.map(|c| c.content).unwrap_or_default();
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(text);
        if !text.ends_with('\n') {
            content.push('\n');
        }
        too_large(path, &content)?;
        self.put(path, &content).await?;
        self.commit(author, &format!("append to {path}")).await?;
        Ok(MemoryWritten { path: path.into(), sha256: sha256(&content) })
    }

    pub async fn delete(&self, author: &str, path: &str, expected: &str) -> Result<(), MemoryError> {
        check_path(path, true)?;
        let _write = self.lock.lock().await;
        let current = self.read(path).await?;
        if current.as_ref().is_none_or(|c| c.sha256 != expected) {
            return Err(MemoryError::Conflict(Box::new(MemoryConflict { path: path.into(), current })));
        }
        tokio::fs::remove_file(self.root.join(path)).await?;
        self.commit(author, &format!("delete {path}")).await
    }

    /// Every file under `prefix` (a namespace or directory), sorted by path.
    pub async fn list(&self, prefix: Option<&str>) -> Result<Vec<MemoryEntry>, MemoryError> {
        if let Some(p) = prefix {
            check_path(p, false)?;
        }
        let root = self.root.clone();
        let start = prefix.map_or_else(|| root.clone(), |p| root.join(p));
        let mut files = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            walk(&root, &start, &mut out).map(|()| out)
        })
        .await
        .map_err(std::io::Error::other)??;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }

    /// The commits that changed `path`, newest first.
    pub async fn history(&self, path: &str) -> Result<Vec<MemoryCommit>, MemoryError> {
        check_path(path, true)?;
        // An empty repo has no HEAD yet, so there is no history to read.
        if self.git(&["rev-parse", "-q", "--verify", "HEAD"]).await.is_err() {
            return Ok(vec![]);
        }
        let n = HISTORY.to_string();
        let out = self.git(&["log", "-n", &n, "--format=%H%x1f%an%x1f%aI%x1f%s", "--", path]).await?;
        out.lines()
            .map(|line| {
                let f: Vec<&str> = line.split('\x1f').collect();
                let [commit, author, at, message] = f.as_slice() else {
                    return Err(MemoryError::Malformed(line.into()));
                };
                let at = DateTime::parse_from_rfc3339(at).map_err(|e| MemoryError::Malformed(format!("{at}: {e}")))?;
                Ok(MemoryCommit {
                    commit: commit.to_string(),
                    author: author.to_string(),
                    at: at.with_timezone(&Utc),
                    message: message.to_string(),
                })
            })
            .collect()
    }

    /// Write `content` atomically: a temp file beside the target, then a rename.
    async fn put(&self, path: &str, content: &str) -> Result<(), MemoryError> {
        let target = self.root.join(path);
        let dir = target.parent().ok_or(MemoryError::Invalid("path has no directory"))?;
        tokio::fs::create_dir_all(dir).await?;
        let tmp = dir.join(format!(".tmp-{}", uuid::Uuid::now_v7()));
        tokio::fs::write(&tmp, content).await?;
        if let Err(e) = tokio::fs::rename(&tmp, &target).await {
            if let Err(cleanup) = tokio::fs::remove_file(&tmp).await {
                tracing::warn!(tmp = %tmp.display(), error = %cleanup, "could not remove memory temp file");
            }
            return Err(e.into());
        }
        Ok(())
    }

    async fn commit(&self, author: &str, message: &str) -> Result<(), MemoryError> {
        self.git(&["add", "-A"]).await?;
        let author = format!("--author={author} <{author}@botler.invalid>");
        self.git(&["commit", "-q", "--no-verify", &author, "-m", message]).await?;
        Ok(())
    }
}

/// Collect the files under `dir`, skipping dot entries (`.git`, temp files).
fn walk(root: &Path, dir: &Path, out: &mut Vec<MemoryEntry>) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for e in entries {
        let e = e?;
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let ty = e.file_type()?;
        if ty.is_dir() {
            walk(root, &e.path(), out)?;
        } else if ty.is_file() {
            let path = e.path();
            let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
            out.push(MemoryEntry { path: rel.to_string_lossy().into_owned(), bytes: e.metadata()?.len() });
        }
    }
    Ok(())
}

/// The read-only mounts of an agent's namespaces (created if missing).
pub async fn mounts(hub: &Hub, def: &AgentDef) -> Result<Vec<Mount>, MemoryError> {
    let mut mounts = Vec::new();
    for (name, ns) in namespaces(def) {
        let src = hub.memory.root().join(&ns);
        tokio::fs::create_dir_all(&src).await?;
        mounts.push(Mount::Bind { src, dst: format!("/memory/{name}"), read_only: true });
    }
    Ok(mounts)
}

/// The agent's `INDEX.md` files for its system prompt. Read once per wake, so the prompt stays
/// the same (and cacheable) for the whole session; changes show from the next wake.
pub async fn index_section(hub: &Hub, def: &AgentDef) -> Result<String, MemoryError> {
    let mut s = String::new();
    for (name, ns) in namespaces(def) {
        let Some(f) = hub.memory.read(&format!("{ns}/INDEX.md")).await? else { continue };
        let mut text = f.content;
        if text.len() > INDEX_BYTES {
            let cut = (0..=INDEX_BYTES).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
            text.truncate(cut);
            text.push_str("\n[... truncated: keep INDEX.md short]");
        }
        s.push_str(&format!("### /memory/{name}/INDEX.md\n{}\n\n", text.trim_end()));
    }
    if s.is_empty() {
        return Ok(s);
    }
    Ok(format!("\n## Memory indexes (as of this wake)\n{s}"))
}

/// What a caller may read and write: `None` means everything.
struct Scope {
    read: Option<Vec<String>>,
    write: Option<Vec<String>>,
}

async fn scope(hub: &Hub, caller: &Caller) -> Result<Scope, OpError> {
    match &caller.principal {
        Principal::User => Ok(Scope { read: None, write: None }),
        // Jobs read memory but do not write it.
        Principal::Run { .. } => Ok(Scope { read: None, write: Some(vec![]) }),
        Principal::Agent { instance, .. } => {
            let inst = hub
                .store
                .find_instance(instance)
                .await?
                .ok_or_else(|| OpError::Forbidden(format!("unknown instance {instance}")))?;
            let applied = hub.applied().await;
            let def = applied
                .defs
                .agents
                .get(&inst.definition)
                .ok_or_else(|| HubError::Undefined { kind: "agent", name: inst.definition.clone() })?;
            let ns: Vec<String> = namespaces(def).into_iter().map(|(_, n)| n).collect();
            Ok(Scope { read: Some(ns.clone()), write: Some(ns) })
        }
    }
}

fn permit(allowed: &Option<Vec<String>>, action: &'static str, path: &str) -> Result<(), MemoryError> {
    let Some(list) = allowed else { return Ok(()) };
    check_path(path, false)?;
    let ns = namespace_of(path);
    if list.contains(&ns) {
        return Ok(());
    }
    let allowed = if list.is_empty() { "none".to_string() } else { list.join(", ") };
    Err(MemoryError::Forbidden { action, path: path.into(), allowed })
}

fn author(caller: &Caller) -> String {
    match &caller.principal {
        Principal::User => "user".into(),
        Principal::Agent { instance, .. } => instance.clone(),
        Principal::Run { .. } => caller.runnable.clone().unwrap_or_else(|| "job".into()),
    }
}

/// Callers that have no memory mount (the user, jobs) see the read ops.
pub fn without_mount(c: &Caller) -> bool {
    !matches!(c.principal, Principal::Agent { .. })
}

/// Callers that may write memory (agents and the user).
pub fn writers(c: &Caller) -> bool {
    !matches!(c.principal, Principal::Run { .. })
}

/// Run memory op `name`.
pub async fn call(hub: &Hub, caller: &Caller, name: &str, input: Value) -> OpResult {
    let scope = scope(hub, caller).await?;
    let who = author(caller);
    let m = &hub.memory;
    match name {
        "memory_write" => {
            let a: MemoryWrite = args(input)?;
            permit(&scope.write, "write", &a.path)?;
            to_json(&m.write(&who, &a.path, &a.content, a.expected_sha256.as_deref()).await?)
        }
        "memory_append" => {
            let a: MemoryAppend = args(input)?;
            permit(&scope.write, "write", &a.path)?;
            to_json(&m.append(&who, &a.path, &a.text).await?)
        }
        "memory_delete" => {
            let a: MemoryDelete = args(input)?;
            permit(&scope.write, "delete", &a.path)?;
            m.delete(&who, &a.path, &a.expected_sha256).await?;
            Ok(serde_json::json!({ "deleted": a.path }))
        }
        "memory_read" => {
            let a: MemoryPath = args(input)?;
            permit(&scope.read, "read", &a.path)?;
            to_json(&m.read(&a.path).await?.ok_or(MemoryError::NotFound(a.path))?)
        }
        "memory_list" => {
            let a: MemoryList = args(input)?;
            // Any prefix is fine: only files the caller may read are listed.
            let files: Vec<MemoryEntry> = m
                .list(a.prefix.as_deref())
                .await?
                .into_iter()
                .filter(|f| permit(&scope.read, "read", &f.path).is_ok())
                .collect();
            to_json(&files)
        }
        "memory_history" => {
            let a: MemoryPath = args(input)?;
            permit(&scope.read, "read", &a.path)?;
            to_json(&m.history(&a.path).await?)
        }
        other => Err(OpError::Unknown(other.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_checked() {
        for ok in ["user/profile.md", "agents/dev/github.md", "projects/gs/notes/antenna.md", "agents/dev/INDEX.md"] {
            check_path(ok, true).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "user/../agents/x/a.md",
            "/user/a.md",
            "user//a.md",
            "user/a.txt",
            "user/.md",
            "user/.hidden.md",
            "agents/.git/config.md",
            "agents/dev.md",
            "agents/dev",
            "other/a.md",
            "user/a b.md",
            "user\\a.md",
            "",
        ] {
            assert!(check_path(bad, true).is_err(), "{bad} should be refused");
        }
        assert!(check_path(&format!("user/{}.md", "a".repeat(200)), true).is_err());
        assert!(check_path("user/a/b/c/d/e/f/g/h.md", true).is_err());
        check_path("agents/dev", false).unwrap();
        assert_eq!(namespace_of("agents/dev/x/y.md"), "agents/dev");
        assert_eq!(namespace_of("user/x/y.md"), "user");
    }
}
