//! In-memory container runtime with a scripted pi, for testing hub logic without Podman.

use std::collections::{BTreeMap, HashSet, VecDeque};
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
    /// Tag -> (image id, labels).
    images: BTreeMap<String, (String, BTreeMap<String, String>)>,
    builds: Vec<String>,
    /// Prompt texts received by each attached pi, in order (after delivery dedup).
    prompts: BTreeMap<String, Vec<String>>,
    /// Quiet deliveries (notes) each pi added to its session without starting a turn.
    notes: BTreeMap<String, Vec<String>>,
    /// What `get_last_assistant_text` answers, per container (default "fake reply").
    last_reply: BTreeMap<String, String>,
    /// When set, a prompt starts a turn but does not end it: the test ends it with
    /// [`Fake::pi_event`] (`agent_settled`), after doing things "during" the turn.
    hold_turns: bool,
    /// Containers whose pi is in a held turn (`get_state` reports it streaming).
    busy: HashSet<String>,
    /// The `streamingBehavior` of each accepted prompt, per container.
    behaviors: BTreeMap<String, Vec<Option<String>>>,
    /// Extra pi events to write to an attached pi's stdout, per container.
    inject: BTreeMap<String, mpsc::UnboundedSender<Value>>,
    /// Errors the next turns of an instance's pi end with, keyed by `BOTLER_INSTANCE`.
    fail_turns: BTreeMap<String, VecDeque<String>>,
    /// `compact` commands each instance's pi received.
    compactions: BTreeMap<String, u32>,
    /// Extension commands (`/botler-goto ...`) each pi received, per container.
    commands: BTreeMap<String, Vec<String>>,
    /// The context size `get_session_stats` reports, keyed by `BOTLER_INSTANCE`.
    context_tokens: BTreeMap<String, u64>,
    /// Delivery ids each pi has seen, persisted per session file like the real extension.
    seen: BTreeMap<String, HashSet<String>>,
    events: Vec<mpsc::Sender<ContainerEvent>>,
    fail_builds: bool,
    /// Builds of tags containing this take this long (a slow image).
    slow_builds: Option<(String, std::time::Duration)>,
    crash_on_start: bool,
    networks: HashSet<String>,
    /// Attached containers that are not pi (e.g. the gate) print this and exit with this code.
    script: (String, i32),
    /// `exec`s run, as (container, command).
    execs: Vec<(String, Vec<String>)>,
    /// Shares the fake mount entrypoint reports as failed, with the error.
    mount_failures: BTreeMap<String, String>,
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
        let state = State { script: ("gate: ok\n".into(), 0), ..State::default() };
        Self {
            state: Arc::new(Mutex::new(state)),
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

    /// Networks created with [`Containers::ensure_network`].
    pub fn networks(&self) -> HashSet<String> {
        self.lock().networks.clone()
    }

    pub fn builds(&self) -> Vec<String> {
        self.lock().builds.clone()
    }

    /// Prompt texts the pi in container `name` accepted (duplicates already dropped).
    pub fn prompts(&self, name: &str) -> Vec<String> {
        self.lock().prompts.get(name).cloned().unwrap_or_default()
    }

    /// Notes (quiet deliveries) the pi in container `name` added without starting a turn.
    pub fn notes(&self, name: &str) -> Vec<String> {
        self.lock().notes.get(name).cloned().unwrap_or_default()
    }

    /// The final assistant text the pi in container `name` reports.
    pub fn set_last_reply(&self, name: &str, text: &str) {
        self.lock().last_reply.insert(name.into(), text.into());
    }

    /// End the next turns of `instance`'s pi with these model errors (after pi's own retries), one
    /// per turn; later turns succeed.
    pub fn fail_turns(&self, instance: &str, errors: &[&str]) {
        self.lock().fail_turns.entry(instance.into()).or_default().extend(errors.iter().map(|e| e.to_string()));
    }

    /// The extension commands (`/botler-...`) the pi in container `name` ran.
    pub fn commands(&self, name: &str) -> Vec<String> {
        self.lock().commands.get(name).cloned().unwrap_or_default()
    }

    /// Make `instance`'s pi report a context of `tokens`.
    pub fn set_context_tokens(&self, instance: &str, tokens: u64) {
        self.lock().context_tokens.insert(instance.into(), tokens);
    }

    /// How often `instance`'s pi was asked to compact its context.
    pub fn compactions(&self, instance: &str) -> u32 {
        self.lock().compactions.get(instance).copied().unwrap_or(0)
    }

    /// Keep turns open until the test sends `agent_settled` with [`Fake::pi_event`].
    pub fn set_hold_turns(&self, hold: bool) {
        self.lock().hold_turns = hold;
    }

    /// Make the pi in container `name` emit `event` (e.g. a tool start, or the end of a turn).
    pub fn pi_event(&self, name: &str, event: Value) -> bool {
        let mut st = self.lock();
        if event.get("type").and_then(Value::as_str) == Some("agent_settled") {
            st.busy.remove(name);
        }
        st.inject.get(name).is_some_and(|tx| tx.send(event).is_ok())
    }

    /// The `streamingBehavior` of each prompt the pi in container `name` accepted (`None`: idle).
    pub fn prompt_behaviors(&self, name: &str) -> Vec<Option<String>> {
        self.lock().behaviors.get(name).cloned().unwrap_or_default()
    }

    pub fn set_fail_builds(&self, fail: bool) {
        self.lock().fail_builds = fail;
    }

    /// Builds of tags containing `tag` take `took`.
    pub fn set_slow_builds(&self, tag: &str, took: std::time::Duration) {
        self.lock().slow_builds = Some((tag.into(), took));
    }

    /// What attached non-pi containers (the gate) print and their exit code.
    pub fn set_script_result(&self, output: &str, code: i32) {
        self.lock().script = (output.to_string(), code);
    }

    pub fn execs(&self) -> Vec<(String, Vec<String>)> {
        self.lock().execs.clone()
    }

    /// The mount entrypoint reports share `name` as unavailable with `error` (until cleared).
    pub fn set_mount_failure(&self, name: &str, error: Option<&str>) {
        let mut st = self.lock();
        match error {
            Some(e) => st.mount_failures.insert(name.into(), e.into()),
            None => st.mount_failures.remove(name),
        };
    }

    /// The command after the mount entrypoint (`sh .../botler-mounts up -- <command>`), and, if
    /// there is one, play the entrypoint: report each listed share in the status directory
    /// mounted at `/botler/mounts`, as ok or as set by [`Fake::set_mount_failure`].
    fn unwrap_mounts(&self, spec: &ContainerSpec) -> Result<Vec<String>> {
        let Some(dashes) = spec.command.iter().position(|c| c == "--") else { return Ok(spec.command.clone()) };
        if !spec.command.iter().any(|c| c.ends_with("botler-mounts")) {
            return Ok(spec.command.clone());
        }
        let dir = spec.mounts.iter().find_map(|m| match m {
            crate::container::Mount::Bind { src, dst, .. } if dst == "/botler/mounts" => Some(src.clone()),
            _ => None,
        });
        let io = |e: std::io::Error| ContainerError::Io { binary: "fake".into(), source: e };
        if let Some(dir) = dir {
            let list = std::fs::read_to_string(dir.join("list")).map_err(io)?;
            let failures = self.lock().mount_failures.clone();
            for name in list.lines().filter_map(|l| l.split('|').next()).filter(|n| !n.is_empty()) {
                match failures.get(name) {
                    Some(e) => {
                        std::fs::write(dir.join(format!("{name}.state")), "failed\n").map_err(io)?;
                        std::fs::write(dir.join(format!("{name}.error")), format!("{e}\n")).map_err(io)?;
                    }
                    None => std::fs::write(dir.join(format!("{name}.state")), "ok\n").map_err(io)?,
                }
            }
        }
        Ok(spec.command[dashes + 1..].to_vec())
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

    async fn image_id(&self, image: &str) -> Result<Option<String>> {
        Ok(self.lock().images.get(image).map(|(id, _)| id.clone()))
    }

    async fn build(
        &self,
        context: &Path,
        dockerfile: &str,
        tag: &str,
        labels: &BTreeMap<String, String>,
    ) -> Result<()> {
        let slow = self.lock().slow_builds.clone().filter(|(t, _)| tag.contains(t.as_str()));
        if let Some((_, took)) = slow {
            tokio::time::sleep(took).await;
        }
        let mut st = self.lock();
        // Like podman, the Dockerfile must be inside the context.
        if st.fail_builds || !context.join(dockerfile).is_file() {
            return Err(ContainerError::Command {
                binary: "fake".into(),
                args: format!("build {tag}"),
                stderr: "build failed".into(),
            });
        }
        st.builds.push(format!("{tag} <- {}:{dockerfile}", context.display()));
        let id = format!("fake-image-{}", st.builds.len());
        st.images.insert(tag.to_string(), (id, labels.clone()));
        Ok(())
    }

    async fn images(&self, labels: &BTreeMap<String, String>) -> Result<Vec<String>> {
        Ok(self
            .lock()
            .images
            .iter()
            .filter(|(_, (_, l))| labels.iter().all(|(k, v)| l.get(k) == Some(v)))
            .map(|(t, _)| t.clone())
            .collect())
    }

    async fn tag(&self, source: &str, target: &str) -> Result<()> {
        let mut st = self.lock();
        let image = st.images.get(source).cloned().ok_or_else(|| ContainerError::NotFound(source.into()))?;
        st.images.insert(target.into(), image);
        Ok(())
    }

    async fn remove_image(&self, tag: &str) -> Result<()> {
        match self.lock().images.remove(tag) {
            Some(_) => Ok(()),
            None => Err(ContainerError::NotFound(tag.into())),
        }
    }

    async fn image_size(&self, tag: &str) -> Result<u64> {
        match self.lock().images.contains_key(tag) {
            true => Ok(1 << 20),
            false => Err(ContainerError::NotFound(tag.into())),
        }
    }

    async fn run_detached(&self, spec: &ContainerSpec) -> Result<String> {
        Self::check_mounts(spec)?;
        self.unwrap_mounts(spec)?;
        let mut st = self.lock();
        let fail = |stderr: String| ContainerError::Command {
            binary: "fake".into(),
            args: format!("run {}", spec.name),
            stderr,
        };
        if st.containers.contains_key(&spec.name) {
            return Err(fail(format!("the container name {:?} is already in use", spec.name)));
        }
        if let crate::container::NetworkMode::Named(n) = &spec.network
            && !st.networks.contains(n)
        {
            return Err(fail(format!("unable to find network with name or ID {n}: network not found")));
        }
        for p in &spec.ports {
            let taken =
                st.containers.values().any(|c| c.running && c.spec.ports.iter().any(|o| o.host_port == p.host_port));
            if taken {
                return Err(fail(format!(
                    "rootlessport listen tcp {}:{}: bind: address already in use",
                    p.bind, p.host_port
                )));
            }
        }
        st.containers.insert(spec.name.clone(), FakeContainer { spec: spec.clone(), running: true, exit_code: None });
        Ok(format!("id-{}", spec.name))
    }

    async fn run_attached(&self, spec: &ContainerSpec) -> Result<Attached> {
        Self::check_mounts(spec)?;
        let command = self.unwrap_mounts(spec)?;
        // pi runs directly or behind the agent forwarder (`node forward.mjs pi ...`).
        if !command.iter().take(3).any(|c| c == "pi") {
            let (output, code) = self.lock().script.clone();
            let (hub_w, _pi_r) = tokio::io::duplex(64);
            let (mut pi_w, hub_r) = tokio::io::duplex(1 << 16);
            let (exit_tx, exited) = oneshot::channel();
            let (kill, _kill_rx) = oneshot::channel::<()>();
            tokio::spawn(async move {
                if pi_w.write_all(output.as_bytes()).await.is_err() {
                    tracing::debug!("fake script output unread");
                }
                drop(pi_w);
                if exit_tx.send(Some(code)).is_err() {
                    tracing::debug!("fake script exit unobserved");
                }
            });
            return Ok(Attached {
                name: spec.name.clone(),
                stderr: crate::container::StderrTail::default(),
                stdin: Box::new(hub_w),
                stdout: Box::new(hub_r),
                exited,
                kill,
            });
        }
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
        let (inject_tx, mut inject_rx) = mpsc::unbounded_channel::<Value>();
        self.lock().inject.insert(spec.name.clone(), inject_tx);
        let me = self.clone();
        let name = spec.name.clone();
        let instance = spec.env.get("BOTLER_INSTANCE").cloned().unwrap_or_else(|| name.clone());
        // Dedup state is keyed by the session file, as the real extension stores it in the session.
        let session =
            spec.command.iter().skip_while(|a| *a != "--session").nth(1).cloned().unwrap_or_else(|| name.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(&mut pi_r).lines();
            loop {
                let line = tokio::select! {
                    l = lines.next_line() => l,
                    Some(ev) = inject_rx.recv() => {
                        if pi_w.write_all(format!("{ev}\n").as_bytes()).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    _ = &mut kill_rx => break,
                };
                let Ok(Some(line)) = line else { break };
                let Ok(cmd) = serde_json::from_str::<Value>(&line) else { continue };
                for out in me.respond(&name, &instance, &session, &cmd) {
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
            image: c.spec.image.clone(),
            mounts: c
                .spec
                .mounts
                .iter()
                .filter_map(|m| match m {
                    crate::container::Mount::Bind { src, .. } => Some(src.clone()),
                    _ => None,
                })
                .collect(),
            // A stable fake address per container on its named network.
            ips: match &c.spec.network {
                crate::container::NetworkMode::Named(n) if c.running => {
                    let octet = name.bytes().fold(0u8, |a, b| a.wrapping_mul(31).wrapping_add(b)) % 250 + 2;
                    BTreeMap::from([(n.clone(), format!("10.89.0.{octet}"))])
                }
                _ => BTreeMap::new(),
            },
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

    async fn ensure_network(&self, name: &str) -> Result<()> {
        self.lock().networks.insert(name.into());
        Ok(())
    }

    async fn exec(&self, name: &str, command: &[String]) -> Result<String> {
        let mut st = self.lock();
        if !st.containers.get(name).is_some_and(|c| c.running) {
            return Err(ContainerError::NotFound(name.into()));
        }
        st.execs.push((name.into(), command.to_vec()));
        Ok(String::new())
    }
}

impl Fake {
    /// The scripted pi: enough of the RPC protocol and the botler extension for hub tests.
    fn respond(&self, name: &str, instance: &str, session: &str, cmd: &Value) -> Vec<Value> {
        let id = cmd.get("id").cloned().unwrap_or(Value::Null);
        let kind = cmd.get("type").and_then(Value::as_str).unwrap_or("");
        let ok = |data: Value| json!({"type": "response", "id": id, "command": kind, "success": true, "data": data});
        match kind {
            "prompt" => {
                let text = cmd.get("message").and_then(Value::as_str).unwrap_or("").to_string();
                // Like the extension's commands: handled, no turn.
                if text.starts_with("/botler-") {
                    self.lock().commands.entry(name.into()).or_default().push(text);
                    return vec![ok(json!({"disposition": "handled"}))];
                }
                let envelope =
                    text.strip_prefix("botler:delivery ").and_then(|env| serde_json::from_str::<Value>(env).ok());
                if let Some(env) = &envelope
                    && let Some(did) = env.get("id").and_then(Value::as_str)
                    && !self.lock().seen.entry(session.into()).or_default().insert(did.to_string())
                {
                    return vec![ok(json!({"disposition": "handled"}))];
                }
                // Like the extension: a quiet delivery is appended to the session, no turn.
                if envelope.as_ref().and_then(|e| e.get("quiet")).and_then(Value::as_bool) == Some(true) {
                    self.lock().notes.entry(name.into()).or_default().push(text);
                    return vec![ok(json!({"disposition": "handled"}))];
                }
                let (hold, error) = {
                    let mut st = self.lock();
                    st.prompts.entry(name.into()).or_default().push(text);
                    let behavior = cmd.get("streamingBehavior").and_then(Value::as_str).map(String::from);
                    st.behaviors.entry(name.into()).or_default().push(behavior);
                    if st.hold_turns {
                        st.busy.insert(name.into());
                    }
                    let error = st.fail_turns.get_mut(instance).and_then(VecDeque::pop_front);
                    (st.hold_turns, error)
                };
                let mut message = json!({"role": "assistant", "model": "fake/model", "provider": "openrouter", "stopReason": "stop", "usage": {"totalTokens": 100, "cost": {"total": 0.001}}});
                if let Some(e) = error {
                    message["stopReason"] = json!("error");
                    message["errorMessage"] = json!(e);
                    message["usage"] = json!({"totalTokens": 0, "cost": {"total": 0.0}});
                }
                let mut out = vec![
                    ok(json!({"disposition": "started"})),
                    json!({"type": "agent_start"}),
                    json!({"type": "message_end", "message": message}),
                ];
                if !hold {
                    out.push(json!({"type": "agent_settled"}));
                }
                out
            }
            "get_state" => vec![ok(json!({"isStreaming": self.lock().busy.contains(name), "isCompacting": false}))],
            "get_session_stats" => {
                let tokens = self.lock().context_tokens.get(instance).copied().unwrap_or(1000);
                vec![ok(json!({"sessionFile": session, "contextUsage": {"tokens": tokens, "contextWindow": 200000}}))]
            }
            "get_last_assistant_text" => {
                let text = self.lock().last_reply.get(name).cloned().unwrap_or_else(|| "fake reply".into());
                vec![ok(json!({"text": text}))]
            }
            // Like pi: aborting ends a running (held) turn, its model call failing with the
            // abort; when idle it does nothing.
            "abort" => match self.lock().busy.remove(name) {
                true => vec![
                    ok(Value::Null),
                    json!({"type": "message_end", "message": {"role": "assistant", "stopReason": "error", "errorMessage": "This operation was aborted", "model": "fake/model", "provider": "openrouter"}}),
                    json!({"type": "agent_settled"}),
                ],
                false => vec![ok(Value::Null)],
            },
            "compact" => {
                *self.lock().compactions.entry(instance.into()).or_default() += 1;
                vec![ok(json!({"summary": "fake summary"}))]
            }
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
