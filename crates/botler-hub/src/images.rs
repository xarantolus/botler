//! Building images from a monorepo commit. Tags carry the commit, so an image is built once per
//! commit and a new commit never silently changes a running container's image.

use std::path::PathBuf;

use crate::assets;
use crate::hub::{Hub, HubError, Result};

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

pub fn agent_image_tag(image: &str, commit: &str) -> String {
    format!("localhost/botler/image-{image}:{}", short(commit))
}

pub fn job_image_tag(job: &str, commit: &str) -> String {
    format!("localhost/botler/job-{job}:{}", short(commit))
}

/// Build the agent base image if missing (tagged with the hub version and `latest`).
pub async fn ensure_base(hub: &Hub) -> Result<()> {
    let tag = assets::agent_base_tag();
    if hub.containers.image_exists(&tag).await? && hub.containers.image_exists(assets::AGENT_BASE_LATEST).await? {
        return Ok(());
    }
    let _guard = hub.build_lock.lock().await;
    let ctx = hub.paths.assets().join("agent-base");
    tracing::info!(%tag, "building agent base image");
    hub.containers.build(&ctx, "Dockerfile", &tag).await?;
    hub.containers.build(&ctx, "Dockerfile", assets::AGENT_BASE_LATEST).await?;
    Ok(())
}

/// The full tree of `commit`, exported once and reused as a build context.
async fn export(hub: &Hub, commit: &str) -> Result<PathBuf> {
    let dir = hub.paths.exports().join(commit);
    if dir.join(".complete").exists() {
        return Ok(dir);
    }
    let tmp = hub.paths.exports().join(format!("{commit}.partial-{}", uuid::Uuid::now_v7()));
    hub.repo.export(commit, &tmp).await?;
    tokio::fs::write(tmp.join(".complete"), b"").await?;
    match tokio::fs::rename(&tmp, &dir).await {
        Ok(()) => Ok(dir),
        // Another export of the same commit won the race; use it and drop ours.
        Err(_) if dir.join(".complete").exists() => {
            if let Err(e) = tokio::fs::remove_dir_all(&tmp).await {
                tracing::warn!(dir = %tmp.display(), error = %e, "could not remove duplicate export");
            }
            Ok(dir)
        }
        Err(e) => Err(e.into()),
    }
}

async fn ensure(hub: &Hub, commit: &str, dockerfile: &str, tag: &str) -> Result<String> {
    if hub.containers.image_exists(tag).await? {
        return Ok(tag.to_string());
    }
    ensure_base(hub).await?;
    let _guard = hub.build_lock.lock().await;
    if hub.containers.image_exists(tag).await? {
        return Ok(tag.to_string());
    }
    let ctx = export(hub, commit).await?;
    tracing::info!(%tag, dockerfile, "building image");
    hub.containers.build(&ctx, dockerfile, tag).await?;
    Ok(tag.to_string())
}

pub async fn ensure_agent_image(hub: &Hub, image: &str, commit: &str) -> Result<String> {
    let applied = hub.applied().await;
    if !applied.defs.images.contains(image) {
        return Err(HubError::Undefined { kind: "image", name: image.into() });
    }
    ensure(hub, commit, &format!("images/{image}/Dockerfile"), &agent_image_tag(image, commit)).await
}

pub async fn ensure_job_image(hub: &Hub, job: &str, commit: &str) -> Result<String> {
    ensure(hub, commit, &format!("jobs/{job}/Dockerfile"), &job_image_tag(job, commit)).await
}
