//! A proposal's diff for the user's review: exactly the commits its approval is bound to (the
//! pinned head against the main it was checked on), per file, as git reports it.

use botler_core::defs::Definitions;
use botler_repo::Repo;
use serde::Serialize;
use ts_rs::TS;

use crate::hub::{Hub, HubError, Result};

/// Most diff lines sent for one file, and for the whole proposal.
pub const FILE_LINES: usize = 1500;
pub const TOTAL_LINES: usize = 15_000;

#[derive(Debug, Serialize, TS)]
pub struct ProposalDiff {
    pub base: String,
    pub head: String,
    pub files: Vec<FileDiff>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Serialize, TS)]
pub struct FileDiff {
    pub path: String,
    /// The path before a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    /// Only the user may approve changes to it (the protection policy).
    pub protected: bool,
    pub hunks: Vec<Hunk>,
    /// Diff lines left out because the diff is too large.
    pub omitted: u32,
}

#[derive(Debug, PartialEq, Serialize, TS)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@ context` line.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    /// Lines with git's prefix: ` ` context, `+` added, `-` removed, `\` a note.
    pub lines: Vec<String>,
}

/// The diff the user approves for proposal `id`.
pub async fn proposal_diff(hub: &Hub, id: &str) -> Result<ProposalDiff> {
    let p = hub.store.proposal(id).await?;
    let (Some(base), Some(head)) = (p.base_commit, p.head_commit) else {
        return Err(HubError::Forbidden(format!("proposal {id} has no commits to compare")));
    };
    diff(&hub.repo, &base, &head, &hub.applied().await.defs).await
}

/// `head` against its merge base with `base`; protection as `defs` define it.
pub async fn diff(repo: &Repo, base: &str, head: &str, defs: &Definitions) -> Result<ProposalDiff> {
    let (numstat, patch) = repo.diff_files(base, head).await?;
    // Fail closed: an unreadable policy protects everything.
    let protected = botler_core::change::protected_paths(defs).ok();
    let files = parse(&numstat, &patch, |path| protected.as_ref().is_none_or(|set| set.is_match(path)))?;
    Ok(ProposalDiff { base: base.into(), head: head.into(), files })
}

fn malformed(what: String) -> HubError {
    botler_repo::Error::Malformed(what).into()
}

/// Combine `git diff --numstat -z` and the patch (same files, same order).
fn parse(numstat: &str, patch: &str, protected: impl Fn(&str) -> bool) -> Result<Vec<FileDiff>> {
    let mut stats = Vec::new();
    let mut tokens = numstat.split('\0').filter(|t| !t.is_empty());
    while let Some(t) = tokens.next() {
        let mut f = t.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (f.next(), f.next(), f.next()) else {
            return Err(malformed(format!("numstat entry {t:?}")));
        };
        // A rename has an empty path here, then the old and new paths.
        let (old, path) = if path.is_empty() {
            let old = tokens.next().ok_or_else(|| malformed("numstat rename without paths".into()))?;
            let new = tokens.next().ok_or_else(|| malformed("numstat rename without new path".into()))?;
            (Some(old.to_string()), new.to_string())
        } else {
            (None, path.to_string())
        };
        let count = |s: &str| s.parse::<u32>().ok();
        stats.push((count(a), count(d), old, path));
    }

    let blocks: Vec<&str> = split_blocks(patch);
    if blocks.len() != stats.len() {
        return Err(malformed(format!("{} files in numstat, {} in the patch", stats.len(), blocks.len())));
    }
    let mut budget = TOTAL_LINES;
    let mut files = Vec::new();
    for ((a, d, old_path, path), block) in stats.into_iter().zip(blocks) {
        let header = block.lines().take_while(|l| !l.starts_with("@@"));
        let mut status = FileStatus::Modified;
        for l in header {
            if l.starts_with("new file mode") {
                status = FileStatus::Added;
            } else if l.starts_with("deleted file mode") {
                status = FileStatus::Deleted;
            } else if l.starts_with("rename from") {
                status = FileStatus::Renamed;
            }
        }
        let cap = FILE_LINES.min(budget);
        let (hunks, shown, omitted) = hunks(block, cap)?;
        budget -= shown;
        let protected = protected(&path) || old_path.as_deref().is_some_and(&protected);
        files.push(FileDiff {
            binary: a.is_none() || d.is_none(),
            additions: a.unwrap_or(0),
            deletions: d.unwrap_or(0),
            status,
            protected,
            path,
            old_path,
            hunks,
            omitted,
        });
    }
    Ok(files)
}

/// The patch split at each `diff --git` line.
fn split_blocks(patch: &str) -> Vec<&str> {
    let mut starts: Vec<usize> = Vec::new();
    let mut at = 0;
    for line in patch.split_inclusive('\n') {
        if line.starts_with("diff --git ") {
            starts.push(at);
        }
        at += line.len();
    }
    starts.iter().enumerate().map(|(i, &s)| &patch[s..starts.get(i + 1).copied().unwrap_or(patch.len())]).collect()
}

/// A file's hunks, at most `cap` lines of them; also how many lines were shown and left out.
fn hunks(block: &str, cap: usize) -> Result<(Vec<Hunk>, usize, u32)> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut shown, mut omitted) = (0usize, 0u32);
    for line in block.lines().skip_while(|l| !l.starts_with("@@")) {
        if line.starts_with("@@") {
            if shown >= cap {
                omitted += 1;
                continue;
            }
            let (old_start, new_start) = hunk_starts(line).ok_or_else(|| malformed(format!("hunk header {line:?}")))?;
            hunks.push(Hunk { header: line.into(), old_start, new_start, lines: Vec::new() });
        } else if shown < cap
            && let Some(h) = hunks.last_mut()
        {
            h.lines.push(line.into());
            shown += 1;
        } else {
            omitted += 1;
        }
    }
    Ok((hunks, shown, omitted))
}

/// `@@ -12,3 +14,5 @@ ...` → (12, 14).
fn hunk_starts(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split(' ');
    let start = |s: Option<&str>, sign: char| s?.strip_prefix(sign)?.split(',').next()?.parse().ok();
    parts.next();
    Some((start(parts.next(), '-')?, start(parts.next(), '+')?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunk_headers() {
        assert_eq!(hunk_starts("@@ -12,3 +14,5 @@ fn x"), Some((12, 14)));
        assert_eq!(hunk_starts("@@ -0,0 +1 @@"), Some((0, 1)));
        assert_eq!(hunk_starts("@@ nonsense"), None);
    }

    #[test]
    fn numstat_and_patch_must_agree() {
        let patch = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(parse("1\t1\tx\0", patch, |_| false).is_ok());
        assert!(parse("1\t1\tx\0", "", |_| false).is_err());
        assert!(parse("garbage\0", patch, |_| false).is_err());
    }
}
