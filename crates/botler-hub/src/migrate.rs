//! Boot-time migrations of an existing data directory. Each step checks what is on disk (or in
//! the store or repo) and only acts when the old layout is present, so running them on every
//! boot is safe and a fresh data directory is never touched.

use std::path::Path;

use botler_core::FRONT_DOOR;
use botler_core::defs::{MemTree, Tree};
use botler_repo::Repo;
use botler_store::Store;

use crate::config::Paths;
use crate::hub::{HubError, Result};

/// The front door's name before it became [`FRONT_DOOR`].
const OLD_FRONT_DOOR: &str = "main";

/// Homes replaced the scratch spaces (`/scratch`) of earlier versions, and the hub's own
/// temporary worktrees moved from `scratch/` to `worktrees/`.
///
/// - `scratch-spaces/chat-<instance>` (an instance's own) becomes `homes/<instance>`;
/// - `scratch-spaces/<name>` (shared, `scratch = "<name>"`) becomes `shared-homes/<name>`;
/// - `scratch/` only ever held temporary git worktrees (a `.git` file each), which are useless
///   after a restart: they are deleted. Anything else found there is left alone and reported.
///
/// Returns whether stale worktrees were deleted (the repo should then prune them).
pub fn layout(paths: &Paths) -> std::io::Result<bool> {
    let spaces = paths.root.join("scratch-spaces");
    if spaces.is_dir() {
        for entry in std::fs::read_dir(&spaces)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let to = match name.strip_prefix("chat-") {
                Some(instance) => paths.home(instance),
                None => paths.shared_home(&name),
            };
            move_dir(&entry.path(), &to)?;
        }
        remove_if_empty(&spaces)?;
    }

    let old_worktrees = paths.root.join("scratch");
    let mut pruned = false;
    if old_worktrees.is_dir() {
        for entry in std::fs::read_dir(&old_worktrees)? {
            let entry = entry?;
            if entry.path().join(".git").is_file() {
                tracing::info!(path = %entry.path().display(), "deleting a stale temporary worktree");
                std::fs::remove_dir_all(entry.path())?;
                pruned = true;
            } else {
                tracing::warn!(path = %entry.path().display(), "unexpected entry in the old worktree directory; left in place");
            }
        }
        remove_if_empty(&old_worktrees)?;
    }
    Ok(pruned)
}

/// The front-door chat was called `main`, which agents confused with the monorepo's main
/// branch. It is now [`FRONT_DOOR`]. Two independent, idempotent steps:
///
/// 1. Monorepo: if `agents/main/agent.toml` exists on main and `agents/botler` does not, a
///    hub-authored commit moves the directory and retargets `resume = "main"` triggers. It is
///    only committed if it introduces no new validation errors; otherwise boot stops with the
///    reason, rather than running with a front door that does not exist.
/// 2. Runtime state: if instance `main` (definition `main`) exists, no `botler` instance does, and
///    main now defines `botler`, its directories (session, home, monorepo clone) move and every
///    live reference in SQLite is renamed. The routed event journal keeps the old name. The
///    session file keeps its name: pi records container paths only, and those do not change.
pub async fn front_door(paths: &Paths, store: &Store, repo: &Repo) -> Result<()> {
    rename_definition(repo).await?;
    let head = repo.main().await?;
    if repo.read_file(&head, &format!("agents/{FRONT_DOOR}/agent.toml")).await?.is_none() {
        return Ok(());
    }
    let Some(old) = store.find_instance(OLD_FRONT_DOOR).await? else { return Ok(()) };
    if old.definition != OLD_FRONT_DOOR || store.find_instance(FRONT_DOOR).await?.is_some() {
        return Ok(());
    }
    // Directories first: a crash in between leaves `main` in SQLite and the directories already
    // moved, which the next boot completes (moving a missing directory is a no-op).
    for (from, to) in [
        (paths.sessions(OLD_FRONT_DOOR), paths.sessions(FRONT_DOOR)),
        (paths.home(OLD_FRONT_DOOR), paths.home(FRONT_DOOR)),
        (paths.workspace(OLD_FRONT_DOOR), paths.workspace(FRONT_DOOR)),
    ] {
        move_dir(&from, &to)?;
    }
    rename_clone_branch(&paths.workspace(FRONT_DOOR)).await;
    if store.rename_instance(OLD_FRONT_DOOR, FRONT_DOOR).await? {
        tracing::info!(from = OLD_FRONT_DOOR, to = FRONT_DOOR, "renamed the front-door instance");
    }
    Ok(())
}

async fn rename_definition(repo: &Repo) -> Result<()> {
    let head = repo.main().await?;
    let (old_dir, new_dir) = (format!("agents/{OLD_FRONT_DOOR}"), format!("agents/{FRONT_DOOR}"));
    if repo.read_file(&head, &format!("{old_dir}/agent.toml")).await?.is_none()
        || repo.read_file(&head, &format!("{new_dir}/agent.toml")).await?.is_some()
    {
        return Ok(());
    }
    let before = repo.definition_tree(&head).await?;
    let mut after = MemTree::new();
    let mut edits: Vec<(String, String)> = Vec::new();
    for path in before.paths() {
        let Some(bytes) = before.read(path)? else { continue };
        let moved = match path.strip_prefix(&format!("{old_dir}/")) {
            Some(rest) => format!("{new_dir}/{rest}"),
            None => path.to_string(),
        };
        let text = String::from_utf8_lossy(&bytes);
        let edited = if path.ends_with(".toml") {
            retarget_resume(&text, OLD_FRONT_DOOR, FRONT_DOOR)
        } else if moved == format!("{new_dir}/AGENTS.md") {
            let heading = format!("# You are `{OLD_FRONT_DOOR}`");
            text.contains(&heading).then(|| text.replacen(&heading, &format!("# You are `{FRONT_DOOR}`"), 1))
        } else {
            None
        };
        match edited {
            Some(t) => {
                after.insert(&moved, &t);
                edits.push((moved, t));
            }
            None => after.insert(&moved, bytes),
        }
    }
    let errors = |t: &MemTree| botler_core::load(t).errors().count();
    let (was, now) = (errors(&before), errors(&after));
    if now > was {
        let report = botler_core::load(&after);
        let problems: Vec<String> = report.errors().map(ToString::to_string).collect();
        return Err(HubError::Migration(format!(
            "renaming agents/{OLD_FRONT_DOOR} to agents/{FRONT_DOOR} on the monorepo's main would add validation \
             errors; rename it by hand (and update references to `{OLD_FRONT_DOOR}`):\n{}",
            problems.join("\n")
        )));
    }
    let files: Vec<(&str, Option<&str>)> = edits.iter().map(|(p, t)| (p.as_str(), Some(t.as_str()))).collect();
    let message = format!(
        "Rename the front-door agent `{OLD_FRONT_DOOR}` to `{FRONT_DOOR}`\n\n\
         `{OLD_FRONT_DOOR}` now only means the main branch. Done by the hub at boot: moves \
         {old_dir}/ to {new_dir}/ and retargets `resume = \"{OLD_FRONT_DOOR}\"` triggers."
    );
    let commit = repo.commit_changes(&head, &[(&old_dir, &new_dir)], &files, &message).await?;
    repo.advance_main(&head, &commit).await?;
    tracing::info!(%commit, "renamed the front-door definition on the monorepo's main");
    Ok(())
}

/// `resume = "<old>"` (any spacing, either quote) becomes `resume = "<new>"`. `None` if absent.
fn retarget_resume(text: &str, old: &str, new: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(i) = rest.find("resume") {
        let (head, tail) = rest.split_at(i + "resume".len());
        out.push_str(head);
        rest = tail;
        let after_eq = tail.trim_start_matches([' ', '\t']);
        let Some(after_eq) = after_eq.strip_prefix('=') else { continue };
        let value = after_eq.trim_start_matches([' ', '\t']);
        for q in ['"', '\''] {
            if let Some(v) = value.strip_prefix(q).and_then(|v| v.strip_prefix(old)).and_then(|v| v.strip_prefix(q)) {
                out.push_str(&format!(" = \"{new}\""));
                rest = v;
                changed = true;
                break;
            }
        }
    }
    out.push_str(rest);
    changed.then_some(out)
}

/// Move the clone's work branch into the renamed namespace (pushes are only accepted under
/// `agents/<instance>/`). The checkout belongs to the agent, so git runs with hooks and
/// fsmonitor disabled and no user or system config; a failure only means the agent creates
/// the new branch itself, so it is logged.
async fn rename_clone_branch(clone: &Path) {
    let git_dir = clone.join(".git");
    if !git_dir.is_dir() {
        return;
    }
    let (old, new) = (crate::agents::branch_for(OLD_FRONT_DOOR), crate::agents::branch_for(FRONT_DOOR));
    let out = tokio::process::Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "--git-dir"])
        .arg(&git_dir)
        .args(["branch", "-m", &old, &new])
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => tracing::info!(%old, %new, "renamed the front door's work branch"),
        Ok(o) => {
            tracing::warn!(%old, %new, stderr = %String::from_utf8_lossy(&o.stderr).trim(), "could not rename the front door's work branch; it can create the new one itself")
        }
        Err(e) => tracing::warn!(%old, %new, error = %e, "could not run git to rename the front door's work branch"),
    }
}

/// Rename `from` to `to` unless `to` already exists (then both are kept and the conflict is
/// reported, because merging two directories is not something to do silently). A missing
/// `from` is a no-op.
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    if !from.exists() {
        return Ok(());
    }
    if to.exists() {
        tracing::warn!(from = %from.display(), to = %to.display(), "both the old and the new directory exist; keeping both, merge by hand");
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::info!(from = %from.display(), to = %to.display(), "migrating directory");
    std::fs::rename(from, to)
}

fn remove_if_empty(dir: &Path) -> std::io::Result<()> {
    if std::fs::read_dir(dir)?.next().is_none() {
        std::fs::remove_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_targets_are_retargeted() {
        let t = "[[trigger]]\ntarget = { resume = \"main\" }\n[[trigger]]\ntarget = {resume='main'}\n# resume = \"mainframe\"\n";
        let out = retarget_resume(t, "main", "botler").unwrap();
        assert_eq!(out.matches("resume = \"botler\"").count(), 2, "{out}");
        assert!(out.contains("\"mainframe\""), "{out}");
        assert!(retarget_resume("target = { spawn = \"main\" }", "main", "botler").is_none());
    }

    #[test]
    fn scratch_spaces_become_homes_once() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let root = dir.path();
        std::fs::create_dir_all(root.join("scratch-spaces/chat-main")).unwrap();
        std::fs::write(root.join("scratch-spaces/chat-main/notes.md"), "n").unwrap();
        std::fs::create_dir_all(root.join("scratch-spaces/chat-chat-1a2b")).unwrap();
        std::fs::create_dir_all(root.join("scratch-spaces/groundstation")).unwrap();
        std::fs::create_dir_all(root.join("scratch/0192-wt")).unwrap();
        std::fs::write(root.join("scratch/0192-wt/.git"), "gitdir: x").unwrap();

        assert!(layout(&paths).unwrap());
        assert_eq!(std::fs::read_to_string(paths.home("main").join("notes.md")).unwrap(), "n");
        assert!(paths.home("chat-1a2b").is_dir());
        assert!(paths.shared_home("groundstation").is_dir());
        assert!(!root.join("scratch-spaces").exists() && !root.join("scratch").exists());

        // Idempotent: a second boot finds nothing to do.
        assert!(!layout(&paths).unwrap());
        assert!(paths.home("main").join("notes.md").exists());
    }

    #[test]
    fn conflicts_and_unknown_entries_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(dir.path().join("scratch-spaces/chat-a")).unwrap();
        std::fs::create_dir_all(paths.home("a")).unwrap();
        std::fs::create_dir_all(dir.path().join("scratch/not-a-worktree")).unwrap();
        assert!(!layout(&paths).unwrap());
        assert!(dir.path().join("scratch-spaces/chat-a").exists());
        assert!(dir.path().join("scratch/not-a-worktree").exists());
    }
}
