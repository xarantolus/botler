#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use botler_hub::config::{HubConfig, Paths};
use botler_hub::hub::load_at;
use botler_hub::{Hub, assets};
use botler_runtime::fake::Fake;

pub struct Env {
    pub _dir: tempfile::TempDir,
    pub hub: Arc<Hub>,
    pub fake: Fake,
}

pub fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for e in std::fs::read_dir(src).unwrap() {
        let e = e.unwrap();
        let to = dst.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &to);
        } else {
            std::fs::copy(e.path(), to).unwrap();
        }
    }
}

/// A hub over a fresh monorepo made from the real template, with the fake runtime.
pub async fn env() -> Env {
    let fake = Fake::new();
    env_on(Arc::new(fake.clone()), fake).await
}

/// Like [`env`], but on `runtime` (`fake` is then unused).
pub async fn env_on(runtime: Arc<dyn botler_runtime::Containers>, fake: Fake) -> Env {
    env_with(runtime, fake, HubConfig::new(SocketAddr::from(([127, 0, 0, 1], 0)))).await
}

/// Like [`env_on`], with `config`.
pub async fn env_with(runtime: Arc<dyn botler_runtime::Containers>, fake: Fake, config: HubConfig) -> Env {
    env_at(runtime, fake, config, "data").await
}

/// Like [`env_with`], with the data directory at `data` inside the test's temporary directory.
pub async fn env_at(runtime: Arc<dyn botler_runtime::Containers>, fake: Fake, config: HubConfig, data: &str) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::new(dir.path().join(data));
    paths.ensure().unwrap();
    let tpl = dir.path().join("template");
    copy_dir(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template")), &tpl);
    botler_repo::Repo::new(paths.repo(), paths.worktrees()).init_from(&tpl).await.unwrap();
    assets::unpack(&paths).unwrap();
    let hub = Hub::open(paths, config, runtime).await.unwrap();
    Env { _dir: dir, hub, fake }
}

impl Drop for Env {
    /// The runtime directory (the hub's socket) lives outside the temporary directory.
    fn drop(&mut self) {
        if let Ok(dir) = self.hub.paths.run_dir()
            && let Err(e) = std::fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("could not remove {}: {e}", dir.display());
        }
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

impl Env {
    /// Commit files on top of main, advance main and apply it (what a merged proposal does).
    /// Commit files on top of main and push them as `branch` (what an agent does). Returns the head.
    pub async fn push(&self, branch: &str, files: &[(&str, &str)]) -> String {
        let work = self._dir.path().join(format!("clone-{}", uuid::Uuid::now_v7()));
        git(self._dir.path(), &["clone", "-q", self.hub.repo.git_dir().to_str().unwrap(), work.to_str().unwrap()]);
        for (rel, body) in files {
            let p = work.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "agent change"]);
        git(&work, &["push", "-q", "origin", &format!("+HEAD:refs/heads/{branch}")]);
        self.hub.repo.rev(&format!("refs/heads/{branch}")).await.unwrap().unwrap()
    }

    pub async fn commit(&self, files: &[(&str, &str)]) {
        self.commit_removing(files, &[]).await;
    }

    /// Like [`Env::commit`], also deleting `removed` paths.
    pub async fn commit_removing(&self, files: &[(&str, &str)], removed: &[&str]) {
        let work = self._dir.path().join(format!("clone-{}", uuid::Uuid::now_v7()));
        git(self._dir.path(), &["clone", "-q", self.hub.repo.git_dir().to_str().unwrap(), work.to_str().unwrap()]);
        for (rel, body) in files {
            let p = work.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        for rel in removed {
            git(&work, &["rm", "-r", "-q", rel]);
        }
        git(&work, &["add", "-A"]);
        git(&work, &["commit", "-q", "-m", "test change"]);
        git(&work, &["push", "-q", "origin", "HEAD:refs/heads/agents/test/x"]);
        let base = self.hub.repo.main().await.unwrap();
        let head = self.hub.repo.rev("refs/heads/agents/test/x").await.unwrap().unwrap();
        self.hub.repo.advance_main(&base, &head).await.unwrap();
        let applied = load_at(&self.hub.repo, &head).await.unwrap_or_else(|e| panic!("{e:?}"));
        self.hub.apply(applied).await.unwrap();
    }

    /// Whether `instance`'s container exits within a few seconds. Stopping is asynchronous: the
    /// agent stays live until the hub has seen the exit.
    pub async fn exited(&self, instance: &str) -> bool {
        for _ in 0..200 {
            if self.hub.agents.get(instance).await.is_none() {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        false
    }

    pub async fn route(&self) {
        while botler_hub::router::route_pending(&self.hub).await.unwrap() > 0 {}
    }
}

pub const JOB_DOCKERFILE: &str = "FROM botler/agent-base:latest\n";

pub fn job(name: &str, toml: &str) -> Vec<(String, String)> {
    vec![
        (format!("jobs/{name}/job.toml"), toml.to_string()),
        (format!("jobs/{name}/Dockerfile"), JOB_DOCKERFILE.to_string()),
    ]
}

/// The template's `secrets.toml` with `groups` declared after its own, so the template's jobs
/// keep the groups they are granted.
pub fn secrets_with(groups: &str) -> String {
    let template = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../template/secrets.toml"));
    format!("{template}\n{groups}")
}
