//! What every hub-started container gets: how to reach the hub, its secrets, the SDK.

use std::collections::BTreeMap;
use std::path::PathBuf;

use botler_core::defs::SecretClass;
use botler_runtime::Mount;
use chrono::{Duration, Utc};

use crate::auth::Principal;
use crate::hub::{Hub, HubError, Result};

pub const SDK_DIR: &str = "/opt/botler/sdk";

/// Env and mounts for a container acting as `who`, holding `secrets`.
pub struct Plumbing {
    pub env: BTreeMap<String, String>,
    pub mounts: Vec<Mount>,
    /// Host directory holding file secrets; delete when the container is gone.
    pub secret_dir: Option<PathBuf>,
}

pub async fn plumbing(
    hub: &Hub,
    who: &Principal,
    ttl: Duration,
    secrets: &[String],
    secret_dir: PathBuf,
) -> Result<Plumbing> {
    let mut env = BTreeMap::new();
    env.insert("BOTLER_URL".into(), hub.config.container_url.clone());
    env.insert("BOTLER_TOKEN".into(), hub.tokens.issue(who, ttl, Utc::now()));
    env.insert("PYTHONPATH".into(), SDK_DIR.into());
    let mut mounts = vec![Mount::Bind { src: hub.paths.assets().join("sdk"), dst: SDK_DIR.into(), read_only: true }];
    let applied = hub.applied().await;
    let mut wrote_files = false;
    for name in secrets {
        let decl =
            applied.defs.secrets.get(name).ok_or_else(|| HubError::Undefined { kind: "secret", name: name.clone() })?;
        // Definitions validation already forbids this; checked again because it is the line
        // that keeps the approval guarantee.
        if decl.class != SecretClass::Container {
            return Err(HubError::Forbidden(format!("hub-class secret {name} cannot enter a container")));
        }
        let value =
            hub.store.secret(&hub.secret_key, name).await?.ok_or_else(|| HubError::MissingSecret(name.clone()))?;
        match &decl.file {
            None => {
                env.insert(name.clone(), value);
            }
            Some(dst) => {
                tokio::fs::create_dir_all(&secret_dir).await?;
                let host = secret_dir.join(name);
                write_private(&host, value.as_bytes()).await?;
                mounts.push(Mount::Bind { src: host, dst: dst.clone(), read_only: true });
                wrote_files = true;
            }
        }
    }
    Ok(Plumbing { env, mounts, secret_dir: wrote_files.then_some(secret_dir) })
}

async fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let path = path.to_path_buf();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        opts.open(&path)?.write_all(&bytes)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Remove a container's file secrets.
pub async fn cleanup_secrets(dir: &std::path::Path) {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::error!(dir = %dir.display(), error = %e, "could not delete secret files"),
    }
}
