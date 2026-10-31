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

use futures::future::BoxFuture;
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
    /// An agent image (`images/<name>/`), also used for the gate: its directory plus the code
    /// Python tools run from (baked into `/opt/botler/tools-venv`, so a call installs nothing).
    pub fn agent(image: &str) -> Self {
        let mut inputs = vec![format!("images/{image}")];
        inputs.extend(botler_core::defs::SHARED_INPUTS.map(String::from));
        Self { repository: format!("image-{image}"), dockerfile: format!("images/{image}/Dockerfile"), inputs }
    }

    /// A job image: its own directory plus the uv workspace's shared code and lock.
    pub fn job(job: &str) -> Self {
        let inputs = botler_core::defs::job_inputs(job);
        Self { repository: format!("job-{job}"), dockerfile: format!("jobs/{job}/Dockerfile"), inputs }
    }
}

/// A monorepo image a Dockerfile builds `FROM`: `botler/image-<name>` means `images/<name>/` at
/// the same commit, built first and substituted by its tag.
struct MonorepoBase {
    /// The reference as [`botler_bases`] normalizes it.
    reference: String,
    image: String,
    tag: String,
}

/// What [`resolve`] finds: the tag, the inputs present, and the monorepo images it builds on.
struct Resolved {
    tag: String,
    inputs: Vec<String>,
    monorepo: Vec<MonorepoBase>,
}

/// The tag of `recipe` at `commit`. Every other botler image the Dockerfile builds `FROM` must
/// exist.
fn resolve<'a>(hub: &'a Hub, recipe: &'a Recipe, commit: &'a str) -> BoxFuture<'a, Result<Resolved>> {
    Box::pin(async move {
        let paths: Vec<&str> = recipe.inputs.iter().map(String::as_str).collect();
        let ids = hub.repo.path_ids(commit, &paths).await?;
        let dockerfile = hub.repo.read_file(commit, &recipe.dockerfile).await?.unwrap_or_default();
        let mut bases = Vec::new();
        let mut monorepo = Vec::new();
        for base in botler_bases(&dockerfile) {
            let image = monorepo_image(&base).map(str::to_string);
            let defined = match &image {
                Some(i) => hub.repo.read_file(commit, &format!("images/{i}/Dockerfile")).await?.is_some(),
                None => false,
            };
            if let (Some(image), true) = (image, defined) {
                // Its tag is a content hash already: it stands in for the image id.
                let tag = resolve(hub, &Recipe::agent(&image), commit).await?.tag;
                bases.push((base.clone(), tag.clone()));
                monorepo.push(MonorepoBase { reference: base, image, tag });
                continue;
            }
            let id = hub
                .containers
                .image_id(&base)
                .await?
                .ok_or_else(|| HubError::Undefined { kind: "base image", name: base.clone() })?;
            bases.push((base, id));
        }
        let tag = format!("localhost/botler/{}:{}", recipe.repository, key(recipe, &ids, &bases));
        Ok(Resolved { tag, inputs: ids.into_keys().collect(), monorepo })
    })
}

/// `<name>` of a `localhost/botler/image-<name>[:tag]` reference.
fn monorepo_image(reference: &str) -> Option<&str> {
    let rest = reference.strip_prefix("localhost/botler/image-")?;
    Some(rest.split(':').next().unwrap_or(rest)).filter(|n| !n.is_empty())
}

/// `dockerfile` with the `FROM` references in `to` replaced.
fn rewrite_from(dockerfile: &str, to: &BTreeMap<&str, &str>) -> String {
    dockerfile
        .lines()
        .map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            let Some(i) = words
                .first()
                .filter(|w| w.eq_ignore_ascii_case("FROM"))
                .and_then(|_| words.iter().skip(1).position(|w| !w.starts_with("--")).map(|p| p + 1))
            else {
                return l.to_string();
            };
            let normalized = if words[i].starts_with("localhost/") {
                words[i].to_string()
            } else {
                format!("localhost/{}", words[i])
            };
            match to.get(normalized.as_str()) {
                Some(tag) => {
                    let mut w = words.clone();
                    w[i] = tag;
                    w.join(" ")
                }
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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

/// Build the agent base image if missing (tagged with its content hash), and point `latest`,
/// which monorepo Dockerfiles build `FROM`, at it: a `latest` left from an older hub would
/// otherwise keep their images on the old base.
pub async fn ensure_base(hub: &Hub) -> Result<()> {
    let tag = assets::agent_base_tag();
    let current = || async {
        let id = hub.containers.image_id(&tag).await?;
        Ok::<_, HubError>(id.is_some() && id == hub.containers.image_id(assets::AGENT_BASE_LATEST).await?)
    };
    if current().await? {
        return Ok(());
    }
    let _guard = hub.build_lock.lock().await;
    // Another caller may have done it while this one waited for the lock.
    if current().await? {
        return Ok(());
    }
    if hub.containers.image_id(&tag).await?.is_none() {
        let ctx = hub.paths.assets().join("agent-base");
        tracing::info!(%tag, "building agent base image");
        // Unlabelled: the base belongs to the hub version, not to the monorepo, and is never collected.
        hub.containers.build(&ctx, "Dockerfile", &tag, &BTreeMap::new()).await?;
    }
    tracing::info!(%tag, latest = assets::AGENT_BASE_LATEST, "pointing latest at the agent base image");
    hub.containers.tag(&tag, assets::AGENT_BASE_LATEST).await?;
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
    let Resolved { tag, inputs, monorepo } = resolve(hub, recipe, commit).await?;
    // Before taking the build lock, which building them takes too.
    for base in &monorepo {
        Box::pin(ensure(hub, &Recipe::agent(&base.image), commit)).await?;
    }
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
        if !monorepo.is_empty() {
            let path = ctx.join(&recipe.dockerfile);
            let to: BTreeMap<&str, &str> = monorepo.iter().map(|b| (b.reference.as_str(), b.tag.as_str())).collect();
            let rewritten = rewrite_from(&tokio::fs::read_to_string(&path).await?, &to);
            tokio::fs::write(&path, rewritten).await?;
        }
        tracing::info!(%tag, dockerfile = recipe.dockerfile, "building image");
        let labels = BTreeMap::from([(OWNER_LABEL.to_string(), owner(hub))]);
        Ok::<_, HubError>(hub.containers.build(&ctx, &recipe.dockerfile, &tag, &labels).await?)
    }
    .await;
    remove_dir(&ctx).await;
    built?;
    Ok(tag)
}

pub fn owner(hub: &Hub) -> String {
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
    Ok(resolve(hub, recipe, commit).await?.tag)
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

/// Build the images of each newly applied commit in the background, so a wake or a job start
/// does not wait for a build. A failed build becomes an incident (kind `build`).
pub async fn prebuild_loop(hub: Arc<Hub>) {
    let mut done: Option<String> = None;
    loop {
        let applied = hub.applied().await;
        let paused = match hub.paused().await {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "could not read the pause flag; not building images");
                true
            }
        };
        if !paused && done.as_ref() != Some(&applied.commit) {
            let recipes = applied.defs.images.iter().map(|i| Recipe::agent(i));
            for recipe in recipes.chain(applied.defs.jobs.keys().map(|j| Recipe::job(j))) {
                let Err(e) = ensure(&hub, &recipe, &applied.commit).await else { continue };
                tracing::error!(dockerfile = recipe.dockerfile, error = %e, "image build failed");
                let failure = crate::failures::Failure {
                    runnable: recipe.dockerfile.clone(),
                    kind: "build",
                    exit_code: None,
                    run_id: None,
                    logs_tail: e.to_string(),
                    commit: Some(applied.commit.clone()),
                    event_id: None,
                };
                if let Err(e) = crate::failures::report(&hub, failure).await {
                    tracing::error!(error = %e, "could not report the failed image build");
                }
            }
            done = Some(applied.commit.clone());
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monorepo_bases_are_substituted_by_their_tag() {
        assert_eq!(monorepo_image("localhost/botler/image-browser"), Some("browser"));
        assert_eq!(monorepo_image("localhost/botler/image-browser:latest"), Some("browser"));
        assert_eq!(monorepo_image("localhost/botler/agent-base:latest"), None);
        let to = BTreeMap::from([("localhost/botler/image-browser", "localhost/botler/image-browser:abc")]);
        let df = "FROM botler/image-browser AS b\nRUN echo botler/image-browser\nfrom --platform=x localhost/botler/image-browser\nFROM python:3";
        assert_eq!(
            rewrite_from(df, &to),
            "FROM localhost/botler/image-browser:abc AS b\nRUN echo botler/image-browser\nfrom --platform=x localhost/botler/image-browser:abc\nFROM python:3"
        );
    }

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
