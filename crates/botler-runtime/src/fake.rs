//! In-memory container runtime with a scripted pi, for testing hub logic without Podman.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

use crate::container::{
    Attached, ContainerError, ContainerEvent, ContainerInfo, ContainerSpec, Containers, Result, RuntimeInfo,
};

#[derive(Clone, Debug)]
pub struct FakeContainer {
    pub spec: ContainerSpec,
    pub running: bool,
    pub exit_code: Option<i64>,
}

#[derive(Default)]
struct State {
    containers: BTreeMap<String, FakeContainer>,
    images: BTreeSet<String>,
    builds: Vec<String>,
    /// Prompts received by each attached pi, in order (after delivery dedup).
    prompts: BTreeMap<String, Vec<String>>,
    /// Delivery ids each pi has seen, persisted per session file like the real extension.
    seen: BTreeMap<String, HashSet<String>>,
    events: Vec<mpsc::Sender<ContainerEvent>>,
    fail_builds: bool,
    crash_on_start: bool,
}

#[derive(Clone)]
pub struct Fake {
    state: Arc<Mutex<State>>,
    info: Arc<RuntimeInfo>,
}

impl Default for Fake {
    fn default() -> Self {
        Self::new()
    }
}

impl Fake {
    pub fn new() -> Self {
        Self {
            state: Arc::default(),
            info: Arc::new(RuntimeInfo { binary: "fake".into(), version: "0".into(), rootless: true }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic in another test thread poisons the lock; the data is still usable for asserts.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn containers(&self) -> BTreeMap<String, FakeContainer> {
        self.lock().containers.clone()
    }

    pub fn builds(&self) -> Vec<String> {
        self.lock().builds.clone()
    }

    /// Prompt texts the pi in container `name` accepted (duplicates already dropped).
    pub fn prompts(&self, name: &str) -> Vec<String> {
        self.lock().prompts.get(name).cloned().unwrap_or_default()
    }

    pub fn set_fail_builds(&self, fail: bool) {
        self.lock().fail_builds = fail;
    }

    /// Make attached containers exit immediately with code 125, like a failed `podman run`.
    pub fn set_crash_on_start(&self, crash: bool) {
        self.lock().crash_on_start = crash;
    }

    /// Like podman: a bind mount whose source does not exist fails the run.
    fn check_mounts(spec: &ContainerSpec) -> Result<()> {
        for m in &spec.mounts {
            if let crate::container::Mount::Bind { src, .. } = m
                && !src.exists()
            {
                return Err(ContainerError::Command {
                    binary: "fake".into(),
                    args: format!("run {}", spec.name),
                    stderr: format!("statfs {}: no such file or directory", src.display()),
                });
            }
        }
        Ok(())
    }

    /// Simulate a detached container exiting.
    pub fn exit(&self, name: &str, code: i64) {
        let senders = {
            let mut st = self.lock();
            if let Some(c) = st.containers.get_mut(name) {
                c.running = false;
                c.exit_code = Some(code);
            }
            st.events.clone()
        };
        let ev = ContainerEvent { name: name.into(), status: "died".into(), exit_code: Some(code) };
        for tx in senders {
            if tx.try_send(ev.clone()).is_err() {
                tracing::debug!("fake event receiver full or gone");
            }
        }
    }
}

#[async_trait]
impl Containers for Fake {
    fn info(&self) -> &RuntimeInfo {
        &self.info
    }

    async fn image_exists(&self, image: &str) -> Result<bool> {
        Ok(self.lock().images.contains(image))
    }

    async fn build(&self, context: &Path, dockerfile: &str, tag: &str) -> Result<()> {
        let mut st = self.lock();
        if st.fail_builds {
            return Err(ContainerError::Command {
                binary: "fake".into(),
                args: format!("build {tag}"),
                stderr: "build failed".into(),
            });
        }
        st.builds.push(format!("{tag} <- {}:{dockerfile}", context.display()));
        st.images.insert(tag.to_string());
        Ok(())
    }

    async fn run_detached(&self, spec: &ContainerSpec) -> Result<String> {
        Self::check_mounts(spec)?;
        let mut st = self.lock();
        st.containers.insert(spec.name.clone(), FakeContainer { spec: spec.clone(), running: true, exit_code: None });
        Ok(format!("id-{}", spec.name))
    }

    async fn run_attached(&self, spec: &ContainerSpec) -> Result<Attached> {
        Self::check_mounts(spec)?;
        if self.lock().crash_on_start {
            let (hub_w, _pi_r) = tokio::io::duplex(64);
            let (_pi_w, hub_r) = tokio::io::duplex(64);
            let (exit_tx, exited) = oneshot::channel();
            let (kill, _kill_rx) = oneshot::channel::<()>();
            let stderr = crate::container::StderrTail::default();
            stderr.push("Error: simulated start failure".into());
            if exit_tx.send(Some(125)).is_err() {
                tracing::debug!("fake crash exit unobserved");
            }
            return Ok(Attached {
                name: spec.name.clone(),
                stderr,
                stdin: Box::new(hub_w),
                stdout: Box::new(hub_r),
                exited,
                kill,
            });
        }
        {
            let mut st = self.lock();
            st.containers
                .insert(spec.name.clone(), FakeContainer { spec: spec.clone(), running: true, exit_code: None });
        }
        let (hub_w, mut pi_r) = tokio::io::duplex(1 << 16);
        let (mut pi_w, hub_r) = tokio::io::duplex(1 << 16);
        let (exit_tx, exited) = oneshot::channel();
        let (kill, mut kill_rx) = oneshot::channel::<()>();
        let me = self.clone();
        let name = spec.name.clone();
        // Dedup state is keyed by the session file, as the real extension stores it in the session.
        let session =
            spec.command.iter().skip_while(|a| *a != "--session").nth(1).cloned().unwrap_or_else(|| name.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(&mut pi_r).lines();
            loop {
                let line = tokio::select! {
                    l = lines.next_line() => l,
                    _ = &mut kill_rx => break,
                };
                let Ok(Some(line)) = line else { break };
                let Ok(cmd) = serde_json::from_str::<Value>(&line) else { continue };
                for out in me.respond(&name, &session, &cmd) {
                    if pi_w.write_all(format!("{out}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
            }
            {
                let mut st = me.lock();
                if let Some(c) = st.containers.get_mut(&name) {
                    c.running = false;
                    c.exit_code = Some(0);
                }
            }
            if exit_tx.send(Some(0)).is_err() {
                tracing::debug!(container = %name, "fake exit unobserved");
            }
        });
        Ok(Attached {
            name: spec.name.clone(),
            stderr: crate::container::StderrTail::default(),
            stdin: Box::new(hub_w),
            stdout: Box::new(hub_r),
            exited,
            kill,
        })
    }

    async fn inspect(&self, name: &str) -> Result<Option<ContainerInfo>> {
        Ok(self.lock().containers.get(name).map(|c| ContainerInfo {
            name: name.into(),
            id: format!("id-{name}"),
            running: c.running,
            exit_code: c.exit_code,
            oom_killed: false,
            labels: c.spec.labels.clone(),
        }))
    }

    async fn list(&self, labels: &BTreeMap<String, String>) -> Result<Vec<ContainerInfo>> {
        let names: Vec<String> = self
            .lock()
            .containers
            .iter()
            .filter(|(_, c)| labels.iter().all(|(k, v)| c.spec.labels.get(k) == Some(v)))
            .map(|(n, _)| n.clone())
            .collect();
        let mut out = Vec::new();
        for n in names {
            if let Some(i) = self.inspect(&n).await? {
                out.push(i);
            }
        }
        Ok(out)
    }

    async fn stop(&self, name: &str, _grace_secs: u32) -> Result<()> {
        match self.lock().containers.get_mut(name) {
            Some(c) => {
                c.running = false;
                c.exit_code.get_or_insert(143);
                Ok(())
            }
            None => Err(ContainerError::NotFound(name.into())),
        }
    }

    async fn remove(&self, name: &str) -> Result<()> {
        self.lock().containers.remove(name);
        Ok(())
    }

    async fn logs(&self, name: &str, _tail: u32) -> Result<String> {
        Ok(format!("fake logs of {name}\n"))
    }

    async fn events(&self) -> Result<mpsc::Receiver<ContainerEvent>> {
        let (tx, rx) = mpsc::channel(256);
        self.lock().events.push(tx);
        Ok(rx)
    }
}

impl Fake {
    /// The scripted pi: enough of the RPC protocol and the botler extension for hub tests.
    fn respond(&self, name: &str, session: &str, cmd: &Value) -> Vec<Value> {
        let id = cmd.get("id").cloned().unwrap_or(Value::Null);
        let kind = cmd.get("type").and_then(Value::as_str).unwrap_or("");
        let ok = |data: Value| json!({"type": "response", "id": id, "command": kind, "success": true, "data": data});
        match kind {
            "prompt" => {
                let text = cmd.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some(env) = text.strip_prefix("botler:delivery ")
                    && let Ok(env) = serde_json::from_str::<Value>(env)
                    && let Some(did) = env.get("id").and_then(Value::as_str)
                    && !self.lock().seen.entry(session.into()).or_default().insert(did.to_string())
                {
                    return vec![ok(json!({"disposition": "handled"}))];
                }
                self.lock().prompts.entry(name.into()).or_default().push(text);
                vec![
                    ok(json!({"disposition": "started"})),
                    json!({"type": "agent_start"}),
                    json!({"type": "message_end", "message": {"role": "assistant", "model": "fake/model", "usage": {"totalTokens": 100, "cost": {"total": 0.001}}}}),
                    json!({"type": "agent_settled"}),
                ]
            }
            "get_state" => vec![ok(json!({"isStreaming": false, "isCompacting": false}))],
            "abort" => vec![ok(Value::Null)],
            _ => vec![
                json!({"type": "response", "id": id, "command": kind, "success": false, "error": format!("fake pi: unsupported {kind}")}),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pi::{Disposition, PiClient};
    use std::time::Duration;

    #[tokio::test]
    async fn fake_pi_dedups_deliveries_per_session() {
        let fake = Fake::new();
        let spec = ContainerSpec {
            name: "a".into(),
            command: vec!["pi".into(), "--session".into(), "/s/main.jsonl".into()],
            ..Default::default()
        };
        let att = fake.run_attached(&spec).await.unwrap();
        let (pi, _ev) = PiClient::connect(att.stdin, att.stdout, Duration::from_secs(1));
        let env = r#"botler:delivery {"id":"d1","body":"x"}"#;
        assert_eq!(pi.prompt(env, &[], None).await.unwrap(), Disposition::Started);
        assert_eq!(pi.prompt(env, &[], None).await.unwrap(), Disposition::Handled);
        assert_eq!(fake.prompts("a").len(), 1);

        // A new container on the same session file still remembers d1.
        drop(pi);
        let spec_b = ContainerSpec { name: "b".into(), ..spec };
        let att = fake.run_attached(&spec_b).await.unwrap();
        let (pi, _ev) = PiClient::connect(att.stdin, att.stdout, Duration::from_secs(1));
        assert_eq!(pi.prompt(env, &[], None).await.unwrap(), Disposition::Handled);
    }
}
