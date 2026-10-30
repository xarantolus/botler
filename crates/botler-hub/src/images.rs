//! Building images from the monorepo, and the exports agents mount.
//!
//! An image's tag is a hash of its declared inputs (the git tree hashes of the paths in its build
//! context, the id of the botler base image it builds `FROM`, and its Dockerfile path). A commit
//! that leaves those unchanged maps to the same tag, so nothing is rebuilt, and a running
//! container's image never changes under it. The build context holds only the inputs, at their
//! repo paths, so a Dockerfile cannot depend on anything the tag does not cover.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::assets;
use crate::hub::{Hub, HubError, Result};

/// Label on every image the hub builds from the monorepo: the data directory that owns it, so
/// garbage collection only ever touches its own images.
pub const OWNER_LABEL: &str = "botler.data";
/// Bumped when the meaning of a tag's inputs changes, so old tags are never reused.
const FORMAT: &str = "botler-image v1";

/// What an image is built from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// Repository name under `localhost/botler/`.
    pub repository: String,
    /// Dockerfile path, relative to the repo root (and so to the build context).
    pub dockerfile: String,
    /// Repo paths copied into the build context; absent ones are skipped.
    pub inputs: Vec<String>,
}

impl Recipe {
    /// An agent image (`images/<name>/`), also used for the gate.
    pub fn agent(image: &str) -> Self {
        Self {
            repository: format!("image-{image}"),
            dockerfile: format!("images/{image}/Dockerfile"),
            inputs: vec![format!("images/{image}")],
        }
    }

    /// A job image: its own directory plus the uv workspace's shared code and lock.
    pub fn job(job: &str) -> Self {
        Self {
            repository: format!("job-{job}"),
            dockerfile: format!("jobs/{job}/Dockerfile"),
            inputs: [&format!("jobs/{job}"), "packages", "tools", "connectors", "pyproject.toml", "uv.lock"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// The tag of `recipe` at `commit`, and the inputs present there. Every botler image the
/// Dockerfile builds `FROM` must exist.
async fn resolve(hub: &Hub, recipe: &Recipe, commit: &str) -> Result<(String, Vec<String>)> {
    let paths: Vec<&str> = recipe.inputs.iter().map(String::as_str).collect();
    let ids = hub.repo.path_ids(commit, &paths).await?;
    let dockerfile = hub.repo.read_file(commit, &recipe.dockerfile).await?.unwrap_or_default();
    let mut bases = Vec::new();
    for base in botler_bases(&dockerfile) {
        let id = hub
            .containers
            .image_id(&base)
            .await?
            .ok_or_else(|| HubError::Undefined { kind: "base image", name: base.clone() })?;
        bases.push((base, id));
    }
    let tag = format!("localhost/botler/{}:{}", recipe.repository, key(recipe, &ids, &bases));
    Ok((tag, ids.into_keys().collect()))
}

/// The hash a tag carries: everything that determines the image.
fn key(recipe: &Recipe, ids: &BTreeMap<String, String>, bases: &[(String, String)]) -> String {
    let mut h = Sha256::new();
    h.update(format!("{FORMAT}\ndockerfile {}\n", recipe.dockerfile));
    for input in &recipe.inputs {
        h.update(format!("input {input} {}\n", ids.get(input).map_or("-", String::as_str)));
    }
    for (base, id) in bases {
        h.update(format!("base {base} {id}\n"));
    }
    hex::encode(h.finalize())[..16].to_string()
}

/// Botler images a Dockerfile builds `FROM`, as full local references.
fn botler_bases(dockerfile: &str) -> BTreeSet<String> {
    dockerfile
        .lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            words.next().filter(|w| w.eq_ignore_ascii_case("FROM"))?;
            words.find(|w| !w.starts_with("--"))
        })
        .filter_map(|r| match r.strip_prefix("localhost/") {
            Some(rest) if rest.starts_with("botler/") => Some(r.to_string()),
            None if r.starts_with("botler/") => Some(format!("localhost/{r}")),
            _ => None,
        })
        .collect()
}

/// Build the agent base image if missing (tagged with its content hash and `latest`).
pub async fn ensure_base(hub: &Hub) -> Result<()> {
    let tag = assets::agent_base_tag();
    let exists = |t: String| async move { hub.containers.image_id(&t).await.map(|id| id.is_some()) };
    if exists(tag.clone()).await? && exists(assets::AGENT_BASE_LATEST.into()).await? {
        return Ok(());
    }
    let _guard = hub.build_lock.lock().await;
    // Another caller may have built it while this one waited for the lock.
    if exists(tag.clone()).await? && exists(assets::AGENT_BASE_LATEST.into()).await? {
        return Ok(());
    }
    let ctx = hub.paths.assets().join("agent-base");
    tracing::info!(%tag, "building agent base image");
    // Unlabelled: the base belongs to the hub version, not to the monorepo, and is never collected.
    hub.containers.build(&ctx, "Dockerfile", &tag, &BTreeMap::new()).await?;
    hub.containers.build(&ctx, "Dockerfile", assets::AGENT_BASE_LATEST, &BTreeMap::new()).await?;
    Ok(())
}

/// Export `paths` of `commit` once, as `exports/<name>`.
async fn export(hub: &Hub, name: &str, commit: &str, paths: &[&str]) -> Result<PathBuf> {
    let dir = hub.paths.exports().join(name);
    if dir.join(".complete").exists() {
        return Ok(dir);
    }
    let tmp = hub.paths.exports().join(format!("{name}.partial-{}", uuid::Uuid::now_v7()));
    hub.repo.export_paths(commit, paths, &tmp).await?;
    tokio::fs::write(tmp.join(".complete"), b"").await?;
    match tokio::fs::rename(&tmp, &dir).await {
        Ok(()) => Ok(dir),
        // Another export of the same content won the race; use it and drop ours.
        Err(_) if dir.join(".complete").exists() => {
            remove_dir(&tmp).await;
            Ok(dir)
        }
        Err(e) => Err(e.into()),
    }
}

/// The whole tree of `commit`, exported once (Python tools run from it, see `agents.rs`).
pub async fn applied_tree(hub: &Hub, commit: &str) -> Result<PathBuf> {
    export(hub, &format!("applied-{commit}"), commit, &[]).await
}

async fn remove_dir(dir: &Path) {
    if let Err(e) = tokio::fs::remove_dir_all(dir).await {
        tracing::warn!(dir = %dir.display(), error = %e, "could not remove export");
    }
}

/// The tag of `recipe` at `commit`, built if missing.
pub async fn ensure(hub: &Hub, recipe: &Recipe, commit: &str) -> Result<String> {
    ensure_base(hub).await?;
    let (tag, inputs) = resolve(hub, recipe, commit).await?;
    if hub.containers.image_id(&tag).await?.is_some() {
        return Ok(tag);
    }
    let _guard = hub.build_lock.lock().await;
    if hub.containers.image_id(&tag).await?.is_some() {
        return Ok(tag);
    }
    let ctx = hub.paths.exports().join(format!("build-{}", uuid::Uuid::now_v7()));
    let inputs: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let built = async {
        hub.repo.export_paths(commit, &inputs, &ctx).await?;
        tracing::info!(%tag, dockerfile = recipe.dockerfile, "building image");
        let labels = BTreeMap::from([(OWNER_LABEL.to_string(), owner(hub))]);
        Ok::<_, HubError>(hub.containers.build(&ctx, &recipe.dockerfile, &tag, &labels).await?)
    }
    .await;
    remove_dir(&ctx).await;
    built?;
    Ok(tag)
}

fn owner(hub: &Hub) -> String {
    crate::config::path_str(&hub.paths.root)
}

pub async fn ensure_agent_image(hub: &Hub, image: &str, commit: &str) -> Result<String> {
    let applied = hub.applied().await;
    if !applied.defs.images.contains(image) {
        return Err(HubError::Undefined { kind: "image", name: image.into() });
    }
    ensure(hub, &Recipe::agent(image), commit).await
}

pub async fn ensure_job_image(hub: &Hub, job: &str, commit: &str) -> Result<String> {
    ensure(hub, &Recipe::job(job), commit).await
}

/// The tag `recipe` has at `commit`, without building it.
pub async fn tag(hub: &Hub, recipe: &Recipe, commit: &str) -> Result<String> {
    Ok(resolve(hub, recipe, commit).await?.0)
}

const PI: &str = "pi";

/// Export name of the pi resources at `commit`, keyed by their tree hash so commits that leave
/// `pi/` alone share one export. `None` if the commit has no `pi/`.
pub async fn pi_export_name(hub: &Hub, commit: &str) -> Result<Option<String>> {
    Ok(hub.repo.path_ids(commit, &[PI]).await?.remove(PI).map(|tree| format!("pi-{tree}")))
}

/// Agent-authored pi resources (`pi/`) at `commit`, exported if needed.
pub async fn pi_resources(hub: &Hub, commit: &str) -> Result<Option<PathBuf>> {
    let Some(name) = pi_export_name(hub, commit).await? else { return Ok(None) };
    Ok(Some(export(hub, &name, commit, &[PI]).await?.join(PI)))
}

/// Exports this young are kept even when unreferenced: they may be about to be mounted.
const EXPORT_GRACE: Duration = Duration::from_secs(3600);

/// Remove this hub's image tags and exports that nothing references: not the applied commit, an
/// unfinished proposal, an active run or any hub container. Holds the build lock, so no build
/// races it.
pub async fn collect_garbage(hub: &Hub) -> Result<()> {
    let _guard = hub.build_lock.lock().await;
    let applied = hub.applied().await;
    let mut tags = BTreeSet::new();
    let mut exports = BTreeSet::new();
    for image in &applied.defs.images {
        tags.insert(tag(hub, &Recipe::agent(image), &applied.commit).await?);
    }
    for job in applied.defs.jobs.keys() {
        tags.insert(tag(hub, &Recipe::job(job), &applied.commit).await?);
    }
    exports.extend(pi_export_name(hub, &applied.commit).await?);
    for p in hub.store.unfinished_proposals().await? {
        let Some(head) = p.head_commit else { continue };
        for image in &applied.defs.images {
            tags.insert(tag(hub, &Recipe::agent(image), &head).await?);
        }
    }
    for run in hub.store.active_runs().await? {
        if let Some(commit) = &run.commit_id {
            tags.insert(tag(hub, &Recipe::job(&run.runnable), commit).await?);
        }
    }
    let root = hub.paths.exports();
    for c in hub.containers.list(&BTreeMap::new()).await? {
        tags.insert(c.image);
        for m in c.mounts {
            if let Ok(rel) = m.strip_prefix(&root)
                && let Some(first) = rel.iter().next()
            {
                exports.insert(first.to_string_lossy().into_owned());
            }
        }
    }

    let owned = hub.containers.images(&BTreeMap::from([(OWNER_LABEL.to_string(), owner(hub))])).await?;
    for t in owned.iter().filter(|t| !tags.contains(*t)) {
        match hub.containers.remove_image(t).await {
            Ok(()) => tracing::info!(tag = %t, "removed unused image"),
            Err(e) => tracing::warn!(tag = %t, error = %e, "could not remove unused image"),
        }
    }
    let mut entries = tokio::fs::read_dir(&root).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if exports.contains(&name) {
            continue;
        }
        let young = entry.metadata().await?.modified()?.elapsed().is_ok_and(|age| age < EXPORT_GRACE);
        if !young {
            remove_dir(&entry.path()).await;
        }
    }
    Ok(())
}

/// Collect garbage after the applied commit changes (checked every few minutes, first shortly
/// after boot). Failures are logged and retried on the next round.
pub async fn collect_garbage_loop(hub: Arc<Hub>) {
    let mut done: Option<String> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(300)).await;
        let commit = hub.applied().await.commit.clone();
        if done.as_ref() == Some(&commit) {
            continue;
        }
        match collect_garbage(&hub).await {
            Ok(()) => done = Some(commit),
            Err(e) => tracing::warn!(error = %e, "image and export garbage collection failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_botler_bases() {
        let df = "# c\nFROM botler/agent-base:latest AS b\nfrom --platform=linux/amd64 localhost/botler/x:1\nFROM python:3.12\nRUN FROM botler/no\n";
        assert_eq!(
            botler_bases(df).into_iter().collect::<Vec<_>>(),
            ["localhost/botler/agent-base:latest", "localhost/botler/x:1"]
        );
    }

    #[test]
    fn key_covers_every_input() {
        let r = Recipe::agent("dev");
        let ids = BTreeMap::from([("images/dev".to_string(), "t1".to_string())]);
        let bases = [("localhost/botler/agent-base:latest".to_string(), "b1".to_string())];
        let k = key(&r, &ids, &bases);
        assert_eq!(k, key(&r, &ids, &bases));
        assert_ne!(k, key(&r, &BTreeMap::from([("images/dev".to_string(), "t2".to_string())]), &bases));
        assert_ne!(k, key(&r, &ids, &[("localhost/botler/agent-base:latest".to_string(), "b2".to_string())]));
        assert_ne!(k, key(&Recipe { dockerfile: "images/dev/Other".into(), ..r.clone() }, &ids, &bases));
    }
}
