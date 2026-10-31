//! Network shares in containers (`mounts/<name>.toml`): rclone FUSE mounts set up by the
//! `botler-mounts` entrypoint (`images/mounts/botler-mounts`, a hub asset bind-mounted into the
//! container). Agents never fail to start over a share: what could not be mounted is reported as
//! an incident and told to the agent with its next delivery. Jobs fail instead.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use botler_core::defs::{Definitions, Environment, MountMode};
use botler_runtime::{ContainerSpec, Containers, Mount};

use crate::hub::{Hub, HubError, Result};

/// Where the entrypoint is mounted in the container.
pub const ENTRYPOINT: &str = "/opt/botler/botler-mounts";
/// The list of shares and their status files, in the container.
const STATUS_DST: &str = "/botler/mounts";
/// How long `botler-mounts down` may take to upload pending writes before a stop goes ahead.
const DRAIN_SECS: u64 = 330;

/// How the entrypoint treats the command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Agents: mount what can be mounted, then `exec` the command (the hub drains before a stop).
    Up,
    /// Jobs and services: every share must mount; drain and unmount when the command ends.
    Run,
}

/// The host directory with a container's share list and status files.
pub fn status_dir(hub: &Hub, container: &str) -> PathBuf {
    hub.paths.root.join("mounts").join(container)
}

/// Give `spec` the shares of `env`: FUSE, the entrypoint around its command, the list, and each
/// share's connection from its secret group (as `RCLONE_CONFIG_<REMOTE>_*`). A share that cannot
/// be set up (undefined, a secret without a value) is an error for [`Style::Run`]; for
/// [`Style::Up`] it is recorded as failed for [`report`] and left out.
pub async fn apply(
    hub: &Hub,
    defs: &Definitions,
    env: &Environment,
    style: Style,
    spec: &mut ContainerSpec,
) -> Result<()> {
    if env.mounts.is_empty() {
        return Ok(());
    }
    let dir = status_dir(hub, &spec.name);
    match tokio::fs::remove_dir_all(&dir).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    tokio::fs::create_dir_all(&dir).await?;
    let mut list = String::new();
    for (name, mode) in &env.mounts {
        match share_env(hub, defs, name).await {
            Ok((def, vars)) => {
                // A grant made before the share became read-only gets read-only.
                let mode = if def.spec.read_only { MountMode::ReadOnly } else { *mode };
                list.push_str(&def.list_line(mode));
                list.push('\n');
                spec.env.extend(vars);
            }
            Err(e) if style == Style::Up => {
                tokio::fs::write(dir.join(format!("{name}.state")), "failed\n").await?;
                tokio::fs::write(dir.join(format!("{name}.error")), format!("{e}\n")).await?;
            }
            Err(e) => return Err(e),
        }
    }
    tokio::fs::write(dir.join("list"), list).await?;
    spec.mounts.push(Mount::Bind { src: dir, dst: STATUS_DST.into(), read_only: false });
    let script = hub.paths.assets().join("mounts/botler-mounts");
    spec.mounts.push(Mount::Bind { src: script, dst: ENTRYPOINT.into(), read_only: true });
    spec.devices.push("/dev/fuse".into());
    spec.caps.push("SYS_ADMIN".into());
    let verb = match style {
        Style::Up => "up",
        Style::Run => "run",
    };
    let mut command = vec!["sh".to_string(), ENTRYPOINT.into(), verb.into(), "--".into()];
    command.append(&mut spec.command);
    spec.command = command;
    Ok(())
}

/// Share `name`'s definition and the rclone variables that connect to it.
async fn share_env<'a>(
    hub: &Hub,
    defs: &'a Definitions,
    name: &str,
) -> Result<(&'a botler_core::defs::MountDef, BTreeMap<String, String>)> {
    let def = defs.mounts.get(name).ok_or_else(|| HubError::Undefined { kind: "mount", name: name.into() })?;
    let group = defs
        .secret_groups
        .get(&def.spec.secrets)
        .ok_or_else(|| HubError::Undefined { kind: "secret group", name: def.spec.secrets.clone() })?;
    let mut vars = BTreeMap::from([(def.env_var("TYPE"), def.spec.kind.as_str().to_string())]);
    for secret in &group.secrets {
        // Validation guarantees every secret of the group names an option.
        let option =
            def.option_for(secret).ok_or_else(|| HubError::Undefined { kind: "share option", name: secret.clone() })?;
        let value =
            hub.store.secret(&hub.secret_key, secret).await?.ok_or_else(|| HubError::MissingSecret(secret.clone()))?;
        vars.insert(def.env_var(option), value);
    }
    Ok((def, vars))
}

/// The shares of a container that could not be mounted, as `(name, why)`, from the status files
/// the entrypoint wrote. Call once the main command runs (the entrypoint is done by then).
pub async fn failures(hub: &Hub, container: &str, env: &Environment) -> Result<Vec<(String, String)>> {
    let dir = status_dir(hub, container);
    let mut out = Vec::new();
    for name in env.mounts.keys() {
        let state = read(&dir.join(format!("{name}.state"))).await?;
        let why = match state.as_deref().map(str::trim) {
            Some("ok") => continue,
            Some(_) => read(&dir.join(format!("{name}.error"))).await?.unwrap_or_default().trim().to_string(),
            None => "the container reported nothing (was it started through botler-mounts?)".into(),
        };
        out.push((name.clone(), why));
    }
    Ok(out)
}

async fn read(path: &Path) -> Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Report shares `instance`'s container could not mount: one incident per share, and the lines
/// its next delivery carries ("mount nas-photos unavailable: ...").
pub async fn report(hub: &Hub, instance: &str, failed: &[(String, String)]) -> Result<Vec<String>> {
    let mut notes = Vec::new();
    for (name, why) in failed {
        tracing::warn!(instance, mount = %name, %why, "share unavailable");
        crate::failures::report(
            hub,
            crate::failures::Failure {
                runnable: format!("mount:{name}"),
                kind: "mount",
                exit_code: None,
                run_id: None,
                logs_tail: format!("chat {instance}: {why}"),
                commit: Some(hub.applied().await.commit.clone()),
                event_id: None,
            },
        )
        .await?;
        notes.push(format!("mount {name} unavailable: {why}"));
    }
    Ok(notes)
}

/// Upload what is pending and unmount, before `container` is stopped. A failure is logged: the
/// stop goes ahead either way.
pub async fn drain(containers: &Arc<dyn Containers>, container: &str) {
    let cmd = ["sh".to_string(), ENTRYPOINT.into(), "down".into()];
    match tokio::time::timeout(std::time::Duration::from_secs(DRAIN_SECS), containers.exec(container, &cmd)).await {
        Ok(Ok(_)) => tracing::debug!(container, "shares drained and unmounted"),
        Ok(Err(e)) => tracing::error!(container, error = %e, "draining shares failed; pending writes may be lost"),
        Err(_) => tracing::error!(container, "draining shares timed out; pending writes may be lost"),
    }
}
