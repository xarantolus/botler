//! The monorepo template, compiled into the binary so `botler init` needs nothing else, and
//! upgrades of existing monorepos to newer templates.
//!
//! [`botler_repo::TEMPLATE_REF`] holds pristine template snapshots as a linear history. When the
//! embedded template differs from the newest snapshot, the hub commits a new one and merges it
//! into main with git's 3-way merge (base: the previous snapshot, so agents' edits survive). The
//! merge goes through the normal transaction pipeline on branch `hub/template-<snapshot>`, on
//! behalf of the front door. A conflicting merge is committed with its markers on that branch and
//! handed to the front door to resolve and propose.

use std::path::Path;
use std::sync::Arc;

use botler_core::event::kinds;
use botler_core::{Event, FRONT_DOOR, Provenance, Source};
use botler_repo::{HUB_EMAIL, Merge, Repo, TEMPLATE_REF};
use botler_store::{DialogStatus, Proposal, ProposalStatus};
use chrono::Utc;
use include_dir::{Dir, include_dir};

use crate::hub::{Hub, Result, load_at};

static TEMPLATE: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../template");
const BRANCH_PREFIX: &str = "hub/template-";

/// Write the template into `dest` (which must not exist yet).
pub fn write_to(dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    TEMPLATE.extract(dest)?;
    // include_dir drops permissions; the gate script must stay executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let gate = dest.join("gate/run");
        if gate.exists() {
            std::fs::set_permissions(gate, std::fs::Permissions::from_mode(0o755))?;
        }
    }
    Ok(())
}

/// What [`upgrade`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Upgrade {
    /// Main already contains the newest snapshot, or its upgrade was already handled.
    UpToDate,
    /// The template's monorepo origin is unknown, so there is no merge base.
    NoBase,
    /// A proposal for the merge was opened.
    Proposed { branch: String, proposal: String },
    /// The template changes were already on main; main moved to the merge directly.
    AlreadyApplied,
    /// The merge conflicts; the branch holds it with markers and the front door was told.
    Conflicted { branch: String, paths: Vec<String> },
}

/// The tree of the embedded template, stored in `repo`.
pub async fn embedded_tree(repo: &Repo, scratch: &Path) -> Result<String> {
    let dir = scratch.join(format!("template-{}", uuid::Uuid::now_v7()));
    write_to(&dir)?;
    let tree = repo.write_tree_from(&dir).await;
    std::fs::remove_dir_all(&dir)?;
    Ok(tree?)
}

/// The newest snapshot. Monorepos made before the ref existed get it bootstrapped from main's
/// root commit, which `botler init` always made from the pristine template. `None` if the root
/// is not such a commit (the monorepo did not come from `botler init`).
pub async fn snapshot(repo: &Repo) -> Result<Option<String>> {
    if let Some(tip) = repo.rev(TEMPLATE_REF).await? {
        return Ok(Some(tip));
    }
    match &repo.roots(botler_repo::MAIN).await?[..] {
        [(root, email)] if email == HUB_EMAIL => {
            repo.pin(TEMPLATE_REF, root).await?;
            tracing::info!(%root, "template snapshot bootstrapped from main's root commit");
            Ok(Some(root.clone()))
        }
        _ => Ok(None),
    }
}

/// Branch an upgrade to `snapshot` is proposed from.
pub fn branch_for(snapshot: &str) -> String {
    format!("{BRANCH_PREFIX}{}", &snapshot[..snapshot.len().min(12)])
}

/// Upgrade proposals not finished yet.
pub async fn pending(hub: &Hub) -> Result<Vec<Proposal>> {
    let all = hub.store.unfinished_proposals().await?;
    Ok(all.into_iter().filter(|p| p.branch.as_deref().is_some_and(|b| b.starts_with(BRANCH_PREFIX))).collect())
}

/// On hub start: propose the embedded template if it is newer than the monorepo's.
pub async fn upgrade_on_boot(hub: &Arc<Hub>) -> Result<Upgrade> {
    let tree = embedded_tree(&hub.repo, &hub.paths.worktrees()).await?;
    upgrade(hub, &tree).await
}

/// Make `tree` the newest snapshot and get it merged into main. Idempotent: an upgrade whose
/// branch exists was already handled, whatever became of it.
pub async fn upgrade(hub: &Arc<Hub>, tree: &str) -> Result<Upgrade> {
    let repo = &hub.repo;
    let Some(mut tip) = snapshot(repo).await? else {
        tracing::warn!("main's root commit is not a botler template; template upgrades are off for this monorepo");
        return Ok(Upgrade::NoBase);
    };
    let version = env!("CARGO_PKG_VERSION");
    if repo.tree(&tip).await? != tree {
        tip = repo.commit_tree(tree, Some(&tip), &format!("botler template (hub v{version})")).await?;
        repo.pin(TEMPLATE_REF, &tip).await?;
        tracing::info!(snapshot = %tip, "new template snapshot");
    }
    let main = repo.main().await?;
    let branch = branch_for(&tip);
    if repo.is_ancestor(&tip, &main).await? || repo.rev(&format!("refs/heads/{branch}")).await?.is_some() {
        return Ok(Upgrade::UpToDate);
    }
    for old in pending(hub).await? {
        supersede(hub, &old, &branch).await?;
    }
    let title = format!("Upgrade botler template files (hub v{version})");
    // The branch is created last: it marks the upgrade as handled. A crash before that repeats
    // the upgrade on the next boot, superseding a proposal opened just before the crash.
    let (outcome, commit) = match repo.merge_into_main(&tip, &title).await? {
        Merge::Clean(commit) if repo.tree(&commit).await? == repo.tree(&main).await? => {
            // Nothing changes: record the merge so the next upgrade uses this snapshot as base.
            repo.advance_main(&main, &commit).await?;
            hub.apply(load_at(repo, &commit).await?).await?;
            (Upgrade::AlreadyApplied, commit)
        }
        Merge::Clean(commit) => {
            let p = crate::transactions::open(hub, FRONT_DOOR, &branch, &commit, &title).await?;
            (Upgrade::Proposed { branch: branch.clone(), proposal: p.id }, commit)
        }
        Merge::Conflicted { commit, paths } => {
            let text = format!(
                "The botler hub (v{version}) ships newer monorepo template files, and merging them into main \
                 conflicts in: {}. Branch `{branch}` holds that merge, committed with the conflict markers. \
                 Fetch it, resolve the markers (keep both the template's fixes and our own changes), run \
                 `gate/run`, push it as one of your branches (`git push origin HEAD:agents/{FRONT_DOOR}/template-upgrade`) \
                 and call `propose_change` with the title \"{title}\". It touches protected paths, so the user approves it.",
                paths.join(", ")
            );
            let ev = Event::new(kinds::MESSAGE, Source::Hub, Provenance::Trusted, Utc::now())
                .to(FRONT_DOOR)
                .payload(serde_json::json!({ "text": text, "branch": branch, "conflicts": paths }));
            hub.emit(ev).await?;
            (Upgrade::Conflicted { branch: branch.clone(), paths }, commit)
        }
    };
    repo.pin(&format!("refs/heads/{branch}"), &commit).await?;
    tracing::info!(?outcome, "template upgrade");
    Ok(outcome)
}

/// A newer template replaces an upgrade still in flight.
async fn supersede(hub: &Hub, p: &Proposal, by: &str) -> Result<()> {
    let why = format!("superseded by a newer template upgrade ({by})");
    hub.store.set_proposal_outcome(&p.id, ProposalStatus::Failed, &why).await?;
    for d in hub.store.open_dialogs().await?.into_iter().filter(|d| d.proposal_id.as_deref() == Some(&p.id)) {
        hub.store.close_dialog(&d.id, DialogStatus::Aborted, &why).await?;
    }
    tracing::info!(proposal = %p.id, "{why}");
    Ok(())
}
