//! Boot-time migrations of an existing data directory. Each step checks what is on disk (or in
//! the store or repo) and only acts when the old layout is present, so running them on every
//! boot is safe and a fresh data directory is never touched.

use std::path::Path;

use crate::config::Paths;

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

/// Rename `from` to `to` unless `to` already exists (then both are kept and the conflict is
/// reported, because merging two directories is not something to do silently).
fn move_dir(from: &Path, to: &Path) -> std::io::Result<()> {
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
