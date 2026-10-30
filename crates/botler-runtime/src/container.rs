//! Containers, driven through the `podman` (or `docker`) CLI.
//!
//! The CLI is used instead of the API socket because it needs nothing beyond the binary on a
//! generic host (the rootless socket needs a user systemd unit). Every container the hub creates
//! carries the label `botler.hub=1`, so the hub can find and adopt them after a restart.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

pub const HUB_LABEL: &str = "botler.hub";

#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    #[error("{binary} {args}: {stderr}")]
    Command { binary: String, args: String, stderr: String },
    #[error("io running {binary}: {source}")]
    Io { binary: String, source: std::io::Error },
    #[error("cannot parse {what}: {detail}")]
    Parse { what: &'static str, detail: String },
    #[error("no container runtime found (looked for {0:?})")]
    NoRuntime(Vec<String>),
    #[error("container {0} not found")]
    NotFound(String),
    #[error("attached container has no {0}")]
    NoPipe(&'static str),
}

pub type Result<T> = std::result::Result<T, ContainerError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mount {
    Bind {
        src: PathBuf,
        dst: String,
        read_only: bool,
    },
    Tmpfs {
        dst: String,
    },
    /// A named volume managed by the runtime.
    Volume {
        name: String,
        dst: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NetworkMode {
    #[default]
    Default,
    None,
    /// A user-defined network (see [`Containers::ensure_network`]); containers on it reach each
    /// other by container name.
    Named(String),
}

/// A container port published on the host (`-p <bind>:<host_port>:<port>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortMapping {
    pub bind: String,
    pub host_port: u16,
    pub port: u16,
}

/// Everything needed to start a container.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub command: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub mounts: Vec<Mount>,
    pub labels: BTreeMap<String, String>,
    pub network: NetworkMode,
    /// Extra names other containers on the (named) network reach this one by.
    pub network_aliases: Vec<String>,
    pub ports: Vec<PortMapping>,
    pub workdir: Option<String>,
    pub memory: Option<String>,
    pub remove_on_exit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerInfo {
    pub name: String,
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i64>,
    pub oom_killed: bool,
    pub labels: BTreeMap<String, String>,
    /// The image reference the container was started from.
    pub image: String,
    /// Host paths mounted into the container.
    pub mounts: Vec<PathBuf>,
}

/// A lifecycle event for a hub-managed container.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerEvent {
    pub name: String,
    pub status: String,
    pub exit_code: Option<i64>,
}

/// A container attached over stdio (`run -i`). Closing stdin ends pi; `kill` stops the
/// attachment; `exited` resolves with the exit code once the container is gone.
pub struct Attached {
    pub name: String,
    /// The last lines the container wrote to stderr (why it died, if it did).
    pub stderr: StderrTail,
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub exited: oneshot::Receiver<Option<i32>>,
    pub kill: oneshot::Sender<()>,
}

/// A bounded buffer of a container's most recent stderr lines.
#[derive(Clone, Debug, Default)]
pub struct StderrTail(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    const KEEP: usize = 50;

    pub fn push(&self, line: String) {
        let mut q = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if q.len() == Self::KEEP {
            q.pop_front();
        }
        q.push_back(line);
    }

    pub fn text(&self) -> String {
        let q = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        q.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInfo {
    pub binary: String,
    pub version: String,
    pub rootless: bool,
}

#[async_trait]
pub trait Containers: Send + Sync {
    fn info(&self) -> &RuntimeInfo;
    /// The image's id, or `None` if there is no such image.
    async fn image_id(&self, image: &str) -> Result<Option<String>>;
    /// Build `dockerfile` (relative to `context`) with `context` as the build context.
    async fn build(&self, context: &Path, dockerfile: &str, tag: &str, labels: &BTreeMap<String, String>)
    -> Result<()>;
    /// Tags (`repository:tag`) of images carrying all `labels`.
    async fn images(&self, labels: &BTreeMap<String, String>) -> Result<Vec<String>>;
    /// Remove one tag; the image goes when its last tag does.
    async fn remove_image(&self, tag: &str) -> Result<()>;
    /// Start detached; returns the container id.
    async fn run_detached(&self, spec: &ContainerSpec) -> Result<String>;
    async fn run_attached(&self, spec: &ContainerSpec) -> Result<Attached>;
    async fn inspect(&self, name: &str) -> Result<Option<ContainerInfo>>;
    /// Hub-managed containers matching all `labels`.
    async fn list(&self, labels: &BTreeMap<String, String>) -> Result<Vec<ContainerInfo>>;
    async fn stop(&self, name: &str, grace_secs: u32) -> Result<()>;
    async fn remove(&self, name: &str) -> Result<()>;
    async fn logs(&self, name: &str, tail: u32) -> Result<String>;
    /// Stream of lifecycle events for hub-managed containers. Ends if the runtime's event
    /// stream ends; callers restart it.
    async fn events(&self) -> Result<mpsc::Receiver<ContainerEvent>>;
    /// Create user-defined network `name` unless it exists.
    async fn ensure_network(&self, name: &str) -> Result<()>;
}

/// The CLI driver.
#[derive(Clone, Debug)]
pub struct Cli {
    info: RuntimeInfo,
}

impl Cli {
    /// Find a runtime: `preferred` if given, else `podman`, else `docker`.
    pub async fn detect(preferred: Option<&str>) -> Result<Self> {
        let candidates: Vec<String> = match preferred {
            Some(p) => vec![p.to_string()],
            None => vec!["podman".into(), "docker".into()],
        };
        for binary in &candidates {
            let out = match Command::new(binary).args(["version", "--format", "{{.Client.Version}}"]).output().await {
                Ok(o) if o.status.success() => o,
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ContainerError::Io { binary: binary.clone(), source }),
            };
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let rootless = match binary.as_str() {
                "podman" => {
                    let o = run(binary, &["info", "--format", "{{.Host.Security.Rootless}}"]).await?;
                    o.trim() == "true"
                }
                _ => false,
            };
            return Ok(Self { info: RuntimeInfo { binary: binary.clone(), version, rootless } });
        }
        Err(ContainerError::NoRuntime(candidates))
    }

    fn cmd(&self) -> Command {
        Command::new(&self.info.binary)
    }

    fn run_args(spec: &ContainerSpec) -> Vec<String> {
        let mut a: Vec<String> = vec!["--name".into(), spec.name.clone(), "--init".into()];
        if spec.remove_on_exit {
            a.push("--rm".into());
        }
        a.extend(["--label".into(), format!("{HUB_LABEL}=1")]);
        for (k, v) in &spec.labels {
            a.extend(["--label".into(), format!("{k}={v}")]);
        }
        for (k, v) in &spec.env {
            a.extend(["--env".into(), format!("{k}={v}")]);
        }
        for m in &spec.mounts {
            match m {
                Mount::Bind { src, dst, read_only } => {
                    let ro = if *read_only { ",ro" } else { "" };
                    a.extend(["--mount".into(), format!("type=bind,src={},dst={dst}{ro}", src.display())]);
                }
                Mount::Tmpfs { dst } => a.extend(["--mount".into(), format!("type=tmpfs,dst={dst}")]),
                Mount::Volume { name, dst } => {
                    a.extend(["--mount".into(), format!("type=volume,src={name},dst={dst}")])
                }
            }
        }
        match &spec.network {
            NetworkMode::Default => {}
            NetworkMode::None => a.extend(["--network".into(), "none".into()]),
            NetworkMode::Named(n) => a.extend(["--network".into(), n.clone()]),
        }
        for alias in &spec.network_aliases {
            a.extend(["--network-alias".into(), alias.clone()]);
        }
        for p in &spec.ports {
            a.extend(["--publish".into(), format!("{}:{}:{}", p.bind, p.host_port, p.port)]);
        }
        if let Some(w) = &spec.workdir {
            a.extend(["--workdir".into(), w.clone()]);
        }
        if let Some(m) = &spec.memory {
            a.extend(["--memory".into(), m.clone()]);
        }
        a.push(spec.image.clone());
        a.extend(spec.command.iter().cloned());
        a
    }
}

async fn run(binary: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|source| ContainerError::Io { binary: binary.into(), source })?;
    if !out.status.success() {
        return Err(ContainerError::Command {
            binary: binary.into(),
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[derive(Deserialize)]
struct InspectState {
    #[serde(rename = "Running")]
    running: bool,
    #[serde(rename = "ExitCode", default)]
    exit_code: Option<i64>,
    #[serde(rename = "OOMKilled", default)]
    oom_killed: bool,
}

#[derive(Deserialize)]
struct InspectConfig {
    #[serde(rename = "Labels", default)]
    labels: Option<BTreeMap<String, String>>,
    #[serde(rename = "Image", default)]
    image: String,
}

#[derive(Deserialize)]
struct InspectMount {
    #[serde(rename = "Source", default)]
    source: PathBuf,
}

#[derive(Deserialize)]
struct Inspect {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "State")]
    state: InspectState,
    #[serde(rename = "Config")]
    config: InspectConfig,
    #[serde(rename = "Mounts", default)]
    mounts: Vec<InspectMount>,
}

impl From<Inspect> for ContainerInfo {
    fn from(i: Inspect) -> Self {
        Self {
            name: i.name.trim_start_matches('/').to_string(),
            id: i.id,
            running: i.state.running,
            exit_code: if i.state.running { None } else { i.state.exit_code },
            oom_killed: i.state.oom_killed,
            labels: i.config.labels.unwrap_or_default(),
            image: i.config.image,
            mounts: i.mounts.into_iter().map(|m| m.source).collect(),
        }
    }
}

#[async_trait]
impl Containers for Cli {
    fn info(&self) -> &RuntimeInfo {
        &self.info
    }

    async fn image_id(&self, image: &str) -> Result<Option<String>> {
        match run(&self.info.binary, &["image", "inspect", "--format", "{{.Id}}", image]).await {
            Ok(id) => Ok(Some(id.trim().to_string())),
            // podman: "image not known"; docker: "No such image".
            Err(ContainerError::Command { stderr, .. })
                if stderr.contains("image not known") || stderr.to_lowercase().contains("no such image") =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    async fn build(
        &self,
        context: &Path,
        dockerfile: &str,
        tag: &str,
        labels: &BTreeMap<String, String>,
    ) -> Result<()> {
        let file = context.join(dockerfile).to_string_lossy().into_owned();
        let mut args = vec!["build".to_string(), "--quiet".into(), "--file".into(), file, "--tag".into(), tag.into()];
        for (k, v) in labels {
            args.extend(["--label".into(), format!("{k}={v}")]);
        }
        args.push(context.to_string_lossy().into_owned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&self.info.binary, &refs).await.map(|_| ())
    }

    async fn images(&self, labels: &BTreeMap<String, String>) -> Result<Vec<String>> {
        let mut args = vec!["images".to_string(), "--format".into(), "{{.Repository}}:{{.Tag}}".into()];
        for (k, v) in labels {
            args.extend(["--filter".into(), format!("label={k}={v}")]);
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&self.info.binary, &refs).await?;
        Ok(out.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains("<none>")).map(String::from).collect())
    }

    async fn remove_image(&self, tag: &str) -> Result<()> {
        run(&self.info.binary, &["rmi", tag]).await.map(|_| ())
    }

    async fn run_detached(&self, spec: &ContainerSpec) -> Result<String> {
        let mut args = vec!["run".to_string(), "--detach".into()];
        args.extend(Self::run_args(spec));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(run(&self.info.binary, &refs).await?.trim().to_string())
    }

    async fn run_attached(&self, spec: &ContainerSpec) -> Result<Attached> {
        let mut args = vec!["run".to_string(), "--interactive".into()];
        args.extend(Self::run_args(spec));
        let mut child = self
            .cmd()
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ContainerError::Io { binary: self.info.binary.clone(), source })?;
        let (exit_tx, exited) = oneshot::channel();
        let (kill, mut kill_rx) = oneshot::channel::<()>();
        let stdin = child.stdin.take().ok_or(ContainerError::NoPipe("stdin"))?;
        let stdout = child.stdout.take().ok_or(ContainerError::NoPipe("stdout"))?;
        let tail = StderrTail::default();
        if let Some(stderr) = child.stderr.take() {
            let name = spec.name.clone();
            let tail = tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(l)) => {
                            tracing::debug!(container = %name, "{l}");
                            tail.push(l);
                        }
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!(container = %name, error = %e, "reading container stderr failed");
                            break;
                        }
                    }
                }
            });
        }
        let name = spec.name.clone();
        tokio::spawn(async move {
            let status = tokio::select! {
                s = child.wait() => s,
                _ = &mut kill_rx => {
                    if let Err(e) = child.kill().await {
                        tracing::warn!(container = %name, error = %e, "killing attached container failed");
                    }
                    child.wait().await
                }
            };
            let code = match status {
                Ok(s) => s.code(),
                Err(e) => {
                    tracing::warn!(container = %name, error = %e, "waiting for attached container failed");
                    None
                }
            };
            // The receiver may be gone if the hub already moved on; nothing to report then.
            if exit_tx.send(code).is_err() {
                tracing::debug!(container = %name, "exit observed after the attachment was dropped");
            }
        });
        Ok(Attached {
            name: spec.name.clone(),
            stderr: tail,
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            exited,
            kill,
        })
    }

    async fn inspect(&self, name: &str) -> Result<Option<ContainerInfo>> {
        let out = self
            .cmd()
            .args(["container", "inspect", name])
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|source| ContainerError::Io { binary: self.info.binary.clone(), source })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("no such container") || stderr.contains("No such container") {
                return Ok(None);
            }
            return Err(ContainerError::Command {
                binary: self.info.binary.clone(),
                args: format!("container inspect {name}"),
                stderr: stderr.trim().to_string(),
            });
        }
        let mut parsed: Vec<Inspect> = serde_json::from_slice(&out.stdout)
            .map_err(|e| ContainerError::Parse { what: "container inspect", detail: e.to_string() })?;
        Ok(parsed.pop().map(Into::into))
    }

    async fn list(&self, labels: &BTreeMap<String, String>) -> Result<Vec<ContainerInfo>> {
        let mut args =
            vec!["ps".to_string(), "--all".into(), "--quiet".into(), "--filter".into(), format!("label={HUB_LABEL}=1")];
        for (k, v) in labels {
            args.extend(["--filter".into(), format!("label={k}={v}")]);
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let ids = run(&self.info.binary, &refs).await?;
        let mut out = Vec::new();
        for id in ids.lines().map(str::trim).filter(|l| !l.is_empty()) {
            // A container can vanish between ps and inspect (--rm); that is not an error.
            if let Some(info) = self.inspect(id).await? {
                out.push(info);
            }
        }
        Ok(out)
    }

    async fn stop(&self, name: &str, grace_secs: u32) -> Result<()> {
        match run(&self.info.binary, &["stop", "--time", &grace_secs.to_string(), name]).await {
            Err(ContainerError::Command { stderr, .. }) if stderr.to_lowercase().contains("no such container") => {
                Err(ContainerError::NotFound(name.into()))
            }
            r => r.map(|_| ()),
        }
    }

    async fn remove(&self, name: &str) -> Result<()> {
        match run(&self.info.binary, &["rm", "--force", name]).await {
            Err(ContainerError::Command { stderr, .. }) if stderr.to_lowercase().contains("no such container") => {
                Ok(())
            }
            r => r.map(|_| ()),
        }
    }

    async fn logs(&self, name: &str, tail: u32) -> Result<String> {
        let out = self
            .cmd()
            .args(["logs", "--tail", &tail.to_string(), name])
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|source| ContainerError::Io { binary: self.info.binary.clone(), source })?;
        if !out.status.success() {
            return Err(ContainerError::Command {
                binary: self.info.binary.clone(),
                args: format!("logs {name}"),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            });
        }
        // Containers write to both streams; the hub treats them as one log.
        let mut s = String::from_utf8_lossy(&out.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok(s)
    }

    async fn events(&self) -> Result<mpsc::Receiver<ContainerEvent>> {
        let mut child = self
            .cmd()
            .args([
                "events",
                "--format",
                "json",
                "--filter",
                "type=container",
                "--filter",
                &format!("label={HUB_LABEL}=1"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| ContainerError::Io { binary: self.info.binary.clone(), source })?;
        let stdout = child.stdout.take().ok_or(ContainerError::NoPipe("stdout"))?;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move {
            let _child = child; // keep the process alive while we read
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = match lines.next_line().await {
                    Ok(Some(l)) => l,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "container event stream failed");
                        break;
                    }
                };
                match parse_event(&line) {
                    Some(ev) => {
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                    None => tracing::debug!(line = %line, "ignoring unrecognised container event"),
                }
            }
        });
        Ok(rx)
    }

    async fn ensure_network(&self, name: &str) -> Result<()> {
        match run(&self.info.binary, &["network", "create", name]).await {
            // podman: "network name x already used: network already exists"; docker: "network
            // with name x already exists".
            Err(ContainerError::Command { stderr, .. }) if stderr.contains("already exists") => Ok(()),
            r => r.map(|_| ()),
        }
    }
}

/// Parse one `events --format json` line from podman or docker.
fn parse_event(line: &str) -> Option<ContainerEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    // podman: {"Name": .., "Status": "died", "ContainerExitCode": 1}
    // docker: {"status": "die", "Actor": {"Attributes": {"name": .., "exitCode": "1"}}}
    let name = v
        .get("Name")
        .and_then(|n| n.as_str())
        .or_else(|| v.pointer("/Actor/Attributes/name").and_then(|n| n.as_str()))?
        .to_string();
    let status = v.get("Status").or_else(|| v.get("status")).and_then(|s| s.as_str())?;
    let status = match status {
        "die" => "died",
        s => s,
    }
    .to_string();
    let exit_code = v
        .get("ContainerExitCode")
        .and_then(serde_json::Value::as_i64)
        .or_else(|| v.pointer("/Actor/Attributes/exitCode").and_then(|c| c.as_str()).and_then(|c| c.parse().ok()));
    Some(ContainerEvent { name, status, exit_code })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_args_shape() {
        let spec = ContainerSpec {
            name: "botler-agent-main".into(),
            image: "localhost/botler/agent:abc".into(),
            command: vec!["pi".into(), "--mode".into(), "rpc".into()],
            env: BTreeMap::from([("PI_OFFLINE".into(), "1".into())]),
            mounts: vec![
                Mount::Tmpfs { dst: "/botler/agent-dir".into() },
                Mount::Bind {
                    src: "/d/models.json".into(),
                    dst: "/botler/agent-dir/models.json".into(),
                    read_only: true,
                },
            ],
            labels: BTreeMap::from([("botler.role".into(), "agent".into())]),
            network: NetworkMode::None,
            remove_on_exit: true,
            ..Default::default()
        };
        let a = Cli::run_args(&spec).join(" ");
        assert!(a.starts_with("--name botler-agent-main --init --rm --label botler.hub=1 --label botler.role=agent"));
        assert!(a.contains("--mount type=tmpfs,dst=/botler/agent-dir --mount type=bind,src=/d/models.json,dst=/botler/agent-dir/models.json,ro"));
        assert!(a.contains("--network none"));
        assert!(a.ends_with("localhost/botler/agent:abc pi --mode rpc"));
    }

    #[test]
    fn parses_podman_and_docker_events() {
        let p = parse_event(r#"{"Name":"botler-job-x","Status":"died","ContainerExitCode":3}"#).unwrap();
        assert_eq!((p.name.as_str(), p.status.as_str(), p.exit_code), ("botler-job-x", "died", Some(3)));
        let d =
            parse_event(r#"{"status":"die","Actor":{"Attributes":{"name":"botler-job-y","exitCode":"137"}}}"#).unwrap();
        assert_eq!((d.name.as_str(), d.status.as_str(), d.exit_code), ("botler-job-y", "died", Some(137)));
        assert!(parse_event("not json").is_none());
    }
}
