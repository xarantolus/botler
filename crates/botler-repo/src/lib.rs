//! The hub-owned bare monorepo.
//!
//! Only the hub writes `refs/heads/main`, and only through [`Repo::advance_main`], a
//! compare-and-swap. Agents work in ordinary clones and push `agents/<instance>/*` branches over
//! the hub's git HTTP endpoint, where the hook installed by [`Repo::install_hooks`] enforces that
//! namespace. The hub never runs git inside an agent-owned checkout: repo-local config there
//! (`core.fsmonitor`, hooks) would execute as the hub.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use botler_core::defs::MemTree;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub const MAIN: &str = "main";
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "botler hub"),
    ("GIT_AUTHOR_EMAIL", "hub@botler.invalid"),
    ("GIT_COMMITTER_NAME", "botler hub"),
    ("GIT_COMMITTER_EMAIL", "hub@botler.invalid"),
];

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("main moved: expected {expected}, found {found}")]
    MainMoved { expected: String, found: String },
    #[error("conflict applying {what}: {detail}")]
    Conflict { what: String, detail: String },
    #[error("{0} already exists")]
    AlreadyExists(PathBuf),
    #[error("the monorepo has no main branch")]
    NoMain,
    #[error("{new} is not a fast-forward of {base}")]
    NotFastForward { base: String, new: String },
    #[error("refusing to write ref {0} outside the hub namespaces")]
    RefNamespace(String),
    #[error("unexpected git output: {0}")]
    Malformed(String),
    #[error("tar failed: {0}")]
    Tar(String),
    #[error("cat-file writer task failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Paths whose contents are needed to load [`botler_core::Definitions`].
fn is_definition_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["botler.toml" | "secrets.toml" | "tools.manifest.json"] => true,
        ["policies", f] => f.ends_with(".toml") || f.ends_with(".md"),
        ["agents", _, "agent.toml" | "AGENTS.md"] => true,
        ["jobs", _, "job.toml" | "Dockerfile"] => true,
        ["triggers" | "events", f] => f.ends_with(".toml"),
        ["images", _, "Dockerfile"] => true,
        _ => false,
    }
}

#[derive(Clone, Debug)]
pub struct Repo {
    git_dir: PathBuf,
    /// Directory for the hub's own short-lived worktrees.
    worktrees: PathBuf,
}

impl Repo {
    pub fn new(git_dir: impl Into<PathBuf>, worktrees: impl Into<PathBuf>) -> Self {
        Self { git_dir: git_dir.into(), worktrees: worktrees.into() }
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// git with a fixed environment: the host's system and user config are ignored so behaviour
    /// is identical on every machine.
    fn base_cmd() -> Command {
        let mut c = Command::new("git");
        c.env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0");
        c.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
        c.envs(IDENTITY);
        c
    }

    fn cmd(&self) -> Command {
        let mut c = Self::base_cmd();
        c.arg("--git-dir").arg(&self.git_dir);
        c
    }

    async fn run(&self, mut c: Command, args: &[&str]) -> Result<String> {
        let out = c.args(args).stdin(Stdio::null()).output().await?;
        if !out.status.success() {
            return Err(Error::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }

    async fn git(&self, args: &[&str]) -> Result<String> {
        self.run(self.cmd(), args).await
    }

    /// Run git inside one of the hub's own short-lived worktrees.
    async fn git_in(&self, worktree: &Path, args: &[&str]) -> Result<String> {
        let mut c = Self::base_cmd();
        c.arg("-C").arg(worktree);
        self.run(c, args).await
    }

    pub fn exists(&self) -> bool {
        self.git_dir.join("HEAD").exists()
    }

    /// Create the bare repo with an initial commit holding `template`'s files.
    pub async fn init_from(&self, template: &Path) -> Result<String> {
        if self.exists() {
            return Err(Error::AlreadyExists(self.git_dir.clone()));
        }
        tokio::fs::create_dir_all(&self.git_dir).await?;
        let result = self.populate_from(template).await;
        if result.is_err()
            && let Err(e) = tokio::fs::remove_dir_all(&self.git_dir).await
        {
            tracing::warn!(dir = %self.git_dir.display(), error = %e, "could not remove the half-made monorepo");
        }
        result
    }

    async fn populate_from(&self, template: &Path) -> Result<String> {
        self.git(&["init", "--bare", "--initial-branch", MAIN]).await?;
        self.git(&["config", "receive.denyDeletes", "true"]).await?;
        self.git(&["config", "http.receivepack", "true"]).await?;
        self.install_hooks().await?;
        // This git runs in `template`, so a relative git dir would resolve against it.
        let mut c = Self::base_cmd();
        c.arg("--git-dir").arg(std::path::absolute(&self.git_dir)?);
        c.arg("--work-tree").arg(template).current_dir(template);
        self.run(c, &["add", "--all", "--force", "."]).await?;
        let tree = self.git(&["write-tree"]).await?;
        let commit = self.git(&["commit-tree", &tree, "-m", "Initial monorepo from template"]).await?;
        self.git(&["update-ref", &format!("refs/heads/{MAIN}"), &commit, ""]).await?;
        // `add` populated the bare repo's index; drop it so it never leaks into later operations.
        remove_file_if_exists(&self.git_dir.join("index")).await?;
        Ok(commit)
    }

    /// The pre-receive hook: pushes are only allowed to `refs/heads/agents/<instance>/...` for the
    /// authenticated instance (`REMOTE_USER`, set by the hub's HTTP endpoint).
    pub async fn install_hooks(&self) -> Result<()> {
        let hooks = self.git_dir.join("hooks");
        tokio::fs::create_dir_all(&hooks).await?;
        let path = hooks.join("pre-receive");
        tokio::fs::write(&path, PRE_RECEIVE).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await?;
        }
        Ok(())
    }

    /// Resolve a revision to a commit id; `None` if it does not exist.
    pub async fn rev(&self, rev: &str) -> Result<Option<String>> {
        let out = self
            .cmd()
            .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
            .stdin(Stdio::null())
            .output()
            .await?;
        match out.status.code() {
            Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_string())),
            // --quiet --verify exits 1 without output when the revision does not exist.
            Some(1) if out.stderr.is_empty() => Ok(None),
            _ => Err(Error::Git {
                args: format!("rev-parse {rev}"),
                stderr: String::from_utf8_lossy(&out.stderr).into(),
            }),
        }
    }

    pub async fn main(&self) -> Result<String> {
        self.rev(&format!("refs/heads/{MAIN}")).await?.ok_or(Error::NoMain)
    }

    pub async fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        let out = self.cmd().args(["merge-base", "--is-ancestor", ancestor, descendant]).output().await?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(Error::Git {
                args: format!("merge-base --is-ancestor {ancestor} {descendant}"),
                stderr: String::from_utf8_lossy(&out.stderr).into(),
            }),
        }
    }

    /// Files changed between two commits (`base..head`).
    pub async fn changed_paths(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let out = self.git(&["diff", "--name-only", "--no-renames", "-z", base, head]).await?;
        Ok(out.split('\0').filter(|s| !s.is_empty()).map(String::from).collect())
    }

    /// Unified diff between two commits, for the reviewer.
    pub async fn diff(&self, base: &str, head: &str) -> Result<String> {
        self.git(&["diff", "--no-color", "--stat", "--patch", base, head]).await
    }

    pub async fn log_subjects(&self, base: &str, head: &str) -> Result<Vec<String>> {
        let out = self.git(&["log", "--format=%h %s", &format!("{base}..{head}")]).await?;
        Ok(out.lines().map(String::from).collect())
    }

    /// Object ids (tree or blob hashes) of `paths` at `commit`. Absent paths are left out.
    pub async fn path_ids(&self, commit: &str, paths: &[&str]) -> Result<BTreeMap<String, String>> {
        if paths.is_empty() {
            return Ok(BTreeMap::new());
        }
        let mut args = vec!["ls-tree", "-z", "--full-tree", commit, "--"];
        args.extend_from_slice(paths);
        let listing = self.git(&args).await?;
        let mut ids = BTreeMap::new();
        for entry in listing.split('\0').filter(|s| !s.is_empty()) {
            // "<mode> <type> <object>\t<path>"
            let malformed = || Error::Malformed(format!("ls-tree entry {entry:?}"));
            let (meta, path) = entry.split_once('\t').ok_or_else(malformed)?;
            let object = meta.split(' ').nth(2).ok_or_else(malformed)?;
            ids.insert(path.to_string(), object.to_string());
        }
        Ok(ids)
    }

    /// Load the files definitions are made of at `commit` into memory.
    pub async fn definition_tree(&self, commit: &str) -> Result<MemTree> {
        let listing = self.git(&["ls-tree", "-r", "-z", "--full-tree", commit]).await?;
        let mut wanted = Vec::new();
        for entry in listing.split('\0').filter(|s| !s.is_empty()) {
            // "<mode> <type> <object>\t<path>"
            let malformed = || Error::Malformed(format!("ls-tree entry {entry:?}"));
            let (meta, path) = entry.split_once('\t').ok_or_else(malformed)?;
            let [_mode, kind, object] = meta.split(' ').collect::<Vec<_>>()[..] else { return Err(malformed()) };
            if kind == "blob" && is_definition_path(path) {
                wanted.push((path.to_string(), object.to_string()));
            }
        }
        let mut tree = MemTree::new();
        if wanted.is_empty() {
            return Ok(tree);
        }
        let mut child = self
            .cmd()
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().ok_or_else(|| Error::Malformed("cat-file has no stdin".into()))?;
        let input: String = wanted.iter().map(|(_, o)| format!("{o}\n")).collect();
        let writer = tokio::spawn(async move {
            stdin.write_all(input.as_bytes()).await?;
            stdin.shutdown().await
        });
        let out = child.wait_with_output().await?;
        writer.await??;
        if !out.status.success() {
            return Err(Error::Git {
                args: "cat-file --batch".into(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        parse_cat_file(&out.stdout, &wanted, &mut tree)?;
        Ok(tree)
    }

    /// Move main from `expected` to `new`. Fails if main moved meanwhile or `new` is not a
    /// fast-forward. This is the only way main changes.
    pub async fn advance_main(&self, expected: &str, new: &str) -> Result<()> {
        if !self.is_ancestor(expected, new).await? {
            return Err(Error::NotFastForward { base: expected.into(), new: new.into() });
        }
        match self.git(&["update-ref", "-m", "botler: transaction", &format!("refs/heads/{MAIN}"), new, expected]).await
        {
            Ok(_) => Ok(()),
            Err(err @ Error::Git { .. }) => {
                let found = self.main().await?;
                if found == expected { Err(err) } else { Err(Error::MainMoved { expected: expected.into(), found }) }
            }
            Err(e) => Err(e),
        }
    }

    /// Rebase `branch_commit` onto main in a throwaway worktree. Returns the new head, or the
    /// unchanged commit if it already descends from main.
    pub async fn rebase_onto_main(&self, branch_commit: &str, label: &str) -> Result<String> {
        let main = self.main().await?;
        if self.is_ancestor(&main, branch_commit).await? {
            return Ok(branch_commit.to_string());
        }
        let wt = self.temp_worktree(branch_commit).await?;
        let result = match self.git_in(&wt, &["rebase", "--no-autostash", &main]).await {
            Ok(_) => self.git_in(&wt, &["rev-parse", "HEAD"]).await,
            Err(Error::Git { stderr, .. }) => {
                self.abort_in(&wt, "rebase").await;
                Err(Error::Conflict { what: label.into(), detail: stderr })
            }
            Err(e) => Err(e),
        };
        self.remove_worktree(&wt).await;
        result
    }

    /// A commit on top of main that reverts `commit`.
    pub async fn revert_commit(&self, commit: &str) -> Result<String> {
        let main = self.main().await?;
        let parents = self.git(&["rev-list", "--parents", "-n", "1", commit]).await?;
        let mut args = vec!["revert", "--no-edit"];
        if parents.split(' ').count() > 2 {
            args.extend(["-m", "1"]);
        }
        args.push(commit);
        let wt = self.temp_worktree(&main).await?;
        let result = match self.git_in(&wt, &args).await {
            Ok(_) => self.git_in(&wt, &["rev-parse", "HEAD"]).await,
            Err(Error::Git { stderr, .. }) => {
                self.abort_in(&wt, "revert").await;
                Err(Error::Conflict { what: format!("revert of {commit}"), detail: stderr })
            }
            Err(e) => Err(e),
        };
        self.remove_worktree(&wt).await;
        result
    }

    /// A commit on top of `base` that writes (`Some`) or deletes (`None`) files. Used by the hub
    /// for changes the user approved directly (e.g. declaring secrets from a filled form).
    pub async fn commit_files(&self, base: &str, files: &[(&str, Option<&str>)], message: &str) -> Result<String> {
        self.commit_changes(base, &[], files, message).await
    }

    /// A commit on top of `base` that first renames paths (files or directories, `git mv`, so
    /// history follows them), then writes or deletes files as [`Repo::commit_files`] does.
    pub async fn commit_changes(
        &self,
        base: &str,
        moves: &[(&str, &str)],
        files: &[(&str, Option<&str>)],
        message: &str,
    ) -> Result<String> {
        let wt = self.temp_worktree(base).await?;
        let result = async {
            for (from, to) in moves {
                if let Some(dir) = wt.join(to).parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                self.git_in(&wt, &["mv", "--", from, to]).await?;
            }
            for (rel, body) in files {
                let path = wt.join(rel);
                match body {
                    Some(b) => {
                        if let Some(dir) = path.parent() {
                            tokio::fs::create_dir_all(dir).await?;
                        }
                        tokio::fs::write(&path, b).await?;
                    }
                    None => remove_file_if_exists(&path).await?,
                }
            }
            self.git_in(&wt, &["add", "--all"]).await?;
            self.git_in(&wt, &["commit", "--quiet", "--allow-empty", "-m", message]).await?;
            self.git_in(&wt, &["rev-parse", "HEAD"]).await
        }
        .await;
        self.remove_worktree(&wt).await;
        result
    }

    /// Read one file at a commit (`None` if absent).
    pub async fn read_file(&self, commit: &str, path: &str) -> Result<Option<String>> {
        let out = self.cmd().args(["show", &format!("{commit}:{path}")]).stdin(Stdio::null()).output().await?;
        if out.status.success() {
            return Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("does not exist") || err.contains("exists on disk, but not in") {
            return Ok(None);
        }
        Err(Error::Git { args: format!("show {commit}:{path}"), stderr: err.trim().to_string() })
    }

    /// Store a commit under a hub-owned ref so it is not garbage collected (proposals).
    pub async fn pin(&self, refname: &str, commit: &str) -> Result<()> {
        if !refname.starts_with("refs/proposals/") && !refname.starts_with("refs/botler/") {
            return Err(Error::RefNamespace(refname.into()));
        }
        self.git(&["update-ref", refname, commit]).await.map(|_| ())
    }

    /// Branches agents pushed for one instance: `(branch, commit)`.
    pub async fn agent_branches(&self, instance: &str) -> Result<Vec<(String, String)>> {
        let out = self
            .git(&[
                "for-each-ref",
                "--format=%(refname:strip=2) %(objectname)",
                &format!("refs/heads/agents/{instance}/"),
            ])
            .await?;
        out.lines()
            .map(|l| {
                l.split_once(' ')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
                    .ok_or_else(|| Error::Malformed(format!("for-each-ref line {l:?}")))
            })
            .collect()
    }

    /// Export a commit's full tree to `dest` (for gate runs).
    pub async fn export(&self, commit: &str, dest: &Path) -> Result<()> {
        self.export_paths(commit, &[], dest).await
    }

    /// Export only `paths` (files or directories, kept at their repo paths) of a commit to
    /// `dest`; the whole tree when `paths` is empty. Every path must exist at `commit`.
    pub async fn export_paths(&self, commit: &str, paths: &[&str], dest: &Path) -> Result<()> {
        tokio::fs::create_dir_all(dest).await?;
        let tar = dest.with_extension("tar");
        let t = tar.to_string_lossy().to_string();
        let mut args = vec!["archive", "--format=tar", "-o", &t, commit, "--"];
        args.extend_from_slice(paths);
        self.git(&args).await?;
        let out = Command::new("tar").arg("-xf").arg(&tar).arg("-C").arg(dest).output().await?;
        remove_file_if_exists(&tar).await?;
        if !out.status.success() {
            return Err(Error::Tar(String::from_utf8_lossy(&out.stderr).trim().to_string()));
        }
        Ok(())
    }

    /// Forget worktrees whose directories are gone.
    pub async fn prune_worktrees(&self) -> Result<()> {
        self.git(&["worktree", "prune"]).await.map(|_| ())
    }

    /// Push main to a mirror remote.
    pub async fn push_mirror(&self, url: &str) -> Result<()> {
        self.git(&["push", "--force", url, &format!("refs/heads/{MAIN}:refs/heads/{MAIN}")]).await.map(|_| ())
    }

    async fn temp_worktree(&self, commit: &str) -> Result<PathBuf> {
        tokio::fs::create_dir_all(&self.worktrees).await?;
        let path = self.worktrees.join(uuid::Uuid::now_v7().to_string());
        let p = path.to_string_lossy().to_string();
        self.git(&["worktree", "add", "--detach", "--force", &p, commit]).await?;
        Ok(path)
    }

    /// Cleanup after a failed rebase/revert. The worktree is force-removed next, so a failed abort
    /// is logged rather than returned: returning it would mask the conflict that caused it.
    async fn abort_in(&self, wt: &Path, op: &str) {
        if let Err(e) = self.git_in(wt, &[op, "--abort"]).await {
            tracing::warn!(op, worktree = %wt.display(), error = %e, "abort failed; worktree will be force-removed");
        }
    }

    /// Remove a short-lived worktree. Failures are logged, not returned: cleanup must not turn a
    /// completed operation into a failed one, and leftovers are pruned on the next removal.
    async fn remove_worktree(&self, path: &Path) {
        let p = path.to_string_lossy().to_string();
        let Err(e) = self.git(&["worktree", "remove", "--force", &p]).await else { return };
        tracing::warn!(path = %p, error = %e, "git worktree remove failed; deleting directory");
        if let Err(e) = tokio::fs::remove_dir_all(path).await
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::error!(path = %p, error = %e, "could not delete temporary worktree");
        }
        if let Err(e) = self.git(&["worktree", "prune"]).await {
            tracing::error!(error = %e, "git worktree prune failed");
        }
    }
}

/// Parse `git cat-file --batch` output ("<object> blob <size>\n<bytes>\n" per object, in request
/// order) into `tree`.
fn parse_cat_file(buf: &[u8], wanted: &[(String, String)], tree: &mut MemTree) -> Result<()> {
    let mut pos = 0;
    for (path, object) in wanted {
        let nl = buf
            .get(pos..)
            .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
            .ok_or_else(|| Error::Malformed(format!("cat-file output ended before {object}")))?;
        let header = String::from_utf8_lossy(&buf[pos..pos + nl]).to_string();
        let size: usize = match header.split(' ').collect::<Vec<_>>()[..] {
            [obj, "blob", size] if obj == object => {
                size.parse().map_err(|_| Error::Malformed(format!("cat-file header {header:?}")))?
            }
            _ => return Err(Error::Malformed(format!("cat-file header {header:?} for {object}"))),
        };
        let start = pos + nl + 1;
        let end = start + size;
        let body =
            buf.get(start..end).ok_or_else(|| Error::Malformed(format!("cat-file body of {object} truncated")))?;
        tree.insert(path, body);
        pos = end + 1;
    }
    Ok(())
}

async fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

const PRE_RECEIVE: &str = r#"#!/bin/sh
# Installed by the botler hub. Agents may only push their own branches.
# REMOTE_USER is the authenticated instance name, set by the hub's git endpoint.
if [ -z "$REMOTE_USER" ]; then
  # Local operations by the hub itself (no HTTP push) are not restricted.
  exit 0
fi
zero=0000000000000000000000000000000000000000
while read old new ref; do
  case "$ref" in
    refs/heads/agents/"$REMOTE_USER"/*) ;;
    *) echo "botler: instance '$REMOTE_USER' may only push refs/heads/agents/$REMOTE_USER/* (got $ref)" >&2; exit 1 ;;
  esac
  if [ "$new" = "$zero" ]; then
    echo "botler: deleting branches is not allowed" >&2; exit 1
  fi
done
exit 0
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn definition_paths() {
        for p in [
            "botler.toml",
            "agents/main/agent.toml",
            "agents/main/AGENTS.md",
            "jobs/x/Dockerfile",
            "triggers/a.toml",
            "images/agent/Dockerfile",
            "policies/reviewer.md",
        ] {
            assert!(is_definition_path(p), "{p}");
        }
        for p in ["jobs/x/src/main.py", "agents/main/notes.md", "README.md", "triggers/sub/a.toml"] {
            assert!(!is_definition_path(p), "{p}");
        }
    }

    #[test]
    fn cat_file_parsing_rejects_mismatch() {
        let wanted = vec![("a".to_string(), "abc".to_string())];
        let mut t = MemTree::new();
        parse_cat_file(b"abc blob 2\nhi\n", &wanted, &mut t).unwrap();
        assert!(matches!(parse_cat_file(b"zzz blob 2\nhi\n", &wanted, &mut t), Err(Error::Malformed(_))));
        assert!(matches!(parse_cat_file(b"abc blob 9\nhi\n", &wanted, &mut t), Err(Error::Malformed(_))));
        assert!(matches!(parse_cat_file(b"abc missing\n", &wanted, &mut t), Err(Error::Malformed(_))));
    }
}
