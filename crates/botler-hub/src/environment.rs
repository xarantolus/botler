//! Turning a runnable's [`Environment`] into a container: how it reaches the hub, its secrets,
//! the SDK, local time, its home, network, published ports and volume. Agents, jobs, services and
//! the gate all start from [`container_spec`] and add what is particular to them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use botler_core::defs::{Environment, Network, SecretClass, Workspace};
use botler_runtime::{ContainerSpec, Mount, NetworkMode, PortMapping};
use botler_store::{Instance, Run, RunKind};
use chrono::{Duration, Utc};

use crate::agents::{HOME, MONOREPO};
use crate::auth::Principal;
use crate::hub::{Hub, HubError, Result};
use crate::jobs::{RUN_LABEL, RUNNABLE_LABEL};

pub const SDK_DIR: &str = "/opt/botler/sdk";
/// Upper bound only: agent tokens die with their wake (see `Agents::wakes`).
const AGENT_TOKEN_TTL_HOURS: i64 = 24 * 7;

/// What runs in an environment.
pub enum Runnable<'a> {
    /// One wake of an agent instance.
    Agent { instance: &'a Instance, wake: &'a str },
    /// A job or service run, whose hub token lives `ttl`.
    Run { run: &'a Run, ttl: Duration },
    /// The gate checking proposal `id`: no hub token, no secrets.
    Gate { id: &'a str },
}

/// The container for `what` in `env`, running `command` in `image`. `extra_secrets` are single
/// secrets besides the environment's groups (a model route's API key).
pub async fn container_spec(
    hub: &Hub,
    env: &Environment,
    what: Runnable<'_>,
    image: String,
    command: Vec<String>,
    extra_secrets: &[String],
) -> Result<ContainerSpec> {
    let mut vars = BTreeMap::from([("PYTHONPATH".to_string(), SDK_DIR.to_string())]);
    let mut mounts = vec![Mount::Bind { src: hub.paths.assets().join("sdk"), dst: SDK_DIR.into(), read_only: true }];
    let mut spec = ContainerSpec { image, command, ..Default::default() };
    let mut service = false;
    let token = match what {
        Runnable::Agent { instance, wake } => {
            spec.name = crate::agents::container_name(hub, &instance.name);
            spec.labels = hub.labels("agent");
            spec.labels.insert("botler.instance".into(), instance.name.clone());
            // The home first: the monorepo clone and file secrets may be mounted inside it.
            let home = crate::agents::home_dir(hub, instance, env);
            mounts.insert(0, Mount::Bind { src: home, dst: HOME.into(), read_only: false });
            if env.workspace() == Workspace::Monorepo {
                // Per instance even when the home is shared: each instance works on its own branch.
                let src = hub.paths.workspace(&instance.name);
                mounts.insert(1, Mount::Bind { src, dst: MONOREPO.into(), read_only: false });
            }
            // Set here as well as in the base image, so images built before homes existed get it too.
            vars.insert("HOME".into(), HOME.into());
            spec.workdir = Some(HOME.into());
            spec.remove_on_exit = true;
            let who = Principal::Agent { instance: instance.name.clone(), wake: wake.into() };
            let secrets = hub.paths.root.join("agent-secrets").join(&instance.name);
            Some((who, Duration::hours(AGENT_TOKEN_TTL_HOURS), secrets))
        }
        Runnable::Run { run, ttl } => {
            service = run.kind == RunKind::Service;
            spec.name = crate::jobs::run_container(hub, run);
            spec.labels = hub.labels(if service { "service" } else { "job" });
            spec.labels.insert(RUN_LABEL.into(), run.id.clone());
            spec.labels.insert(RUNNABLE_LABEL.into(), run.runnable.clone());
            // Agents reach a service by its plain name on the network.
            if service {
                spec.network_aliases = vec![run.runnable.clone()];
            }
            vars.insert("BOTLER_RUN_ID".into(), run.id.clone());
            vars.insert("BOTLER_RUNNABLE".into(), run.runnable.clone());
            if env.volume {
                mounts.push(Mount::Volume { name: format!("botler-job-{}", run.runnable), dst: "/data".into() });
            }
            let secrets = hub.paths.root.join("runs").join(&run.id).join("secrets");
            Some((Principal::Run { run_id: run.id.clone() }, ttl, secrets))
        }
        Runnable::Gate { id } => {
            spec.name = format!("botler-{}-gate-{id}", hub.id);
            spec.labels = hub.labels("gate");
            spec.remove_on_exit = true;
            None
        }
    };
    if let Some((who, ttl, secret_dir)) = token {
        vars.insert("BOTLER_URL".into(), hub.config.container_url.clone());
        vars.insert("BOTLER_TOKEN".into(), hub.tokens.issue(&who, ttl, Utc::now()));
        secrets(hub, env, extra_secrets, &secret_dir, &mut vars, &mut mounts).await?;
    }
    let applied = hub.applied().await;
    crate::localtime::apply(applied.defs.timezone, Path::new(crate::localtime::ZONEINFO), &mut vars, &mut mounts);
    spec.env = vars;
    spec.mounts = mounts;
    spec.network = match env.network() {
        Network::None => NetworkMode::None,
        // Services and the agents granted them meet on the services network.
        Network::Default if service || !env.mcp.is_empty() => {
            hub.containers.ensure_network(&hub.network()).await?;
            NetworkMode::Named(hub.network())
        }
        Network::Default => NetworkMode::Default,
    };
    spec.ports = env
        .publish
        .iter()
        .map(|p| PortMapping { bind: p.bind.clone(), host_port: p.host_port, port: p.port })
        .collect();
    if let Some(p) = spec.ports.iter().find(|p| p.host_port == hub.config.listen.port()) {
        return Err(HubError::Forbidden(format!("host port {} is the hub's own port; publish another", p.host_port)));
    }
    Ok(spec)
}

/// Deliver the secrets of `env`'s groups plus `extra` as env vars and (written to `secret_dir`)
/// files.
async fn secrets(
    hub: &Hub,
    env: &Environment,
    extra: &[String],
    secret_dir: &Path,
    vars: &mut BTreeMap<String, String>,
    mounts: &mut Vec<Mount>,
) -> Result<()> {
    let applied = hub.applied().await;
    for g in &env.secrets {
        if !applied.defs.secret_groups.contains_key(g) {
            return Err(HubError::Undefined { kind: "secret group", name: g.clone() });
        }
    }
    for e in extra {
        if !applied.defs.secrets.contains_key(e) {
            return Err(HubError::Undefined { kind: "secret", name: e.clone() });
        }
    }
    for decl in applied.defs.secrets_for(&env.secrets, extra) {
        let name = &decl.name;
        // Definitions validation already forbids this; checked again because it is the line
        // that keeps the approval guarantee.
        if decl.class != SecretClass::Container {
            return Err(HubError::Forbidden(format!("hub-class secret {name} cannot enter a container")));
        }
        let missing = || HubError::MissingSecret(name.clone());
        match &decl.file {
            None => {
                let value = hub.store.secret(&hub.secret_key, name).await?.ok_or_else(missing)?;
                vars.insert(name.clone(), value);
            }
            Some(dst) => {
                // Files are written byte-exact: keystores and other binary secrets stay intact.
                let value = hub.store.secret_bytes(&hub.secret_key, name).await?.ok_or_else(missing)?;
                tokio::fs::create_dir_all(secret_dir).await?;
                let host = secret_dir.join(name);
                write_private(&host, &value).await?;
                mounts.push(Mount::Bind { src: host, dst: dst.clone(), read_only: true });
            }
        }
    }
    Ok(())
}

async fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let path: PathBuf = path.to_path_buf();
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
pub async fn cleanup_secrets(dir: &Path) {
    match tokio::fs::remove_dir_all(dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::error!(dir = %dir.display(), error = %e, "could not delete secret files"),
    }
}
