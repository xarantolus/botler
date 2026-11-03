//! What the machine running the hub configures, outside the monorepo: an optional
//! `~/.config/botler/config.toml` and a directory of secret files. Neither is needed; with
//! neither, `botler serve` creates the data directory and monorepo on its first start and the
//! secrets are entered in the web app.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::config::Paths;
use crate::hub::{Hub, Result};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("{path}: {message}")]
    Config { path: PathBuf, message: String },
    #[error("{0} already holds a botler monorepo")]
    Initialised(PathBuf),
    #[error("{path}: no secret {name} is declared in the monorepo's secrets.toml")]
    UndeclaredSecret { path: PathBuf, name: String },
    #[error("{0} is empty")]
    EmptySecret(PathBuf),
}

/// `config.toml`. Command-line flags and `BOTLER_*` environment variables take precedence.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// The data directory (`~` is your home).
    pub data: Option<String>,
    /// Address of the web app and API.
    pub listen: Option<SocketAddr>,
    /// `podman` or `docker`.
    pub runtime: Option<String>,
    /// A git remote the monorepo's main is pushed to after every change (`[mirror] url` in its
    /// `botler.toml`), set when the monorepo is created; change it later in the web app.
    pub mirror: Option<String>,
}

/// `$XDG_CONFIG_HOME/botler` (else `~/.config/botler`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("botler")
}

impl HostConfig {
    /// Read `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| HostError::Config { path: path.into(), message: e.message().into() }.into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// `data` with a leading `~` expanded.
    pub fn data_dir(&self) -> Option<PathBuf> {
        let d = self.data.as_deref()?;
        Some(match (d.strip_prefix("~/"), std::env::var_os("HOME")) {
            (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
            _ => PathBuf::from(d),
        })
    }
}

/// Create the data directory and the monorepo from the built-in template, with `mirror` as its
/// `[mirror] url`. Returns the first commit.
pub async fn init(paths: &Paths, mirror: Option<&str>) -> Result<String> {
    if paths.repo().exists() {
        return Err(HostError::Initialised(paths.root.clone()).into());
    }
    paths.ensure()?;
    let tmp = paths.root.join("template-init");
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    crate::template::write_to(&tmp)?;
    if let Some(url) = mirror {
        let path = tmp.join("botler.toml");
        let mut doc: toml_edit::DocumentMut = std::fs::read_to_string(&path)?
            .parse()
            .map_err(|e: toml_edit::TomlError| HostError::Config { path: path.clone(), message: e.to_string() })?;
        doc["mirror"]["url"] = toml_edit::value(url);
        std::fs::write(&path, doc.to_string())?;
    }
    let commit = botler_repo::Repo::new(paths.repo(), paths.worktrees()).init_from(&tmp).await?;
    std::fs::remove_dir_all(&tmp)?;
    Ok(commit)
}

/// Set each declared secret that has a file named after it in `dir` (e.g. `OPENROUTER_API_KEY`),
/// so the files stay the source of truth: a changed file is picked up at the next start. Returns
/// the names set; a file for a secret the monorepo does not declare is an error.
pub async fn import_secrets(hub: &Hub, dir: &Path) -> Result<Vec<String>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let declared = hub.applied().await.defs.secrets.keys().cloned().collect::<std::collections::BTreeSet<_>>();
    let mut set = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.file_type()?.is_file() {
            continue;
        }
        if !declared.contains(&name) {
            return Err(HostError::UndeclaredSecret { path: entry.path(), name }.into());
        }
        let value = std::fs::read_to_string(entry.path())?;
        let value = value.trim_end_matches(['\n', '\r']);
        if value.is_empty() {
            return Err(HostError::EmptySecret(entry.path()).into());
        }
        hub.store.set_secret(&hub.secret_key, &name, value).await?;
        set.push(name);
    }
    set.sort();
    Ok(set)
}

/// The systemd user unit that runs `exe serve` (`botler service install`).
pub fn systemd_unit(exe: &Path) -> String {
    let dir = exe.parent().map(|d| format!("{}:", d.display())).unwrap_or_default();
    format!(
        "# botler hub as a systemd user service, written by `botler service install`.\n\
         [Unit]\n\
         Description=botler hub\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         # The data directory, address and mirror come from ~/.config/botler/config.toml (optional);\n\
         # secrets from files in ~/.config/botler/secrets/ (optional).\n\
         ExecStart={exe} serve\n\
         # SIGTERM goes to the hub only; it stops its agent containers, then exits. Anything left after\n\
         # the timeout is killed.\n\
         KillMode=mixed\n\
         TimeoutStopSec=90\n\
         Restart=always\n\
         RestartSec=5\n\
         # Podman and git must be on PATH.\n\
         Environment=PATH={dir}%h/.cargo/bin:%h/.local/bin:/usr/local/bin:/usr/bin:/bin\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = exe.display()
    )
}

/// Something the host lacks for the hub, and how to fix it.
#[derive(Debug, PartialEq)]
pub struct Problem {
    pub what: String,
    pub fix: String,
    /// Without it the hub cannot run at all (otherwise it runs, with a drawback).
    pub blocking: bool,
}

/// What this host still needs for the hub: rootless Podman with subordinate ids for the user (or a
/// working Docker when `runtime` is `docker`), git, and lingering (so the service runs while the
/// user is logged out). Empty when it is ready.
pub fn preflight(user: &str, runtime: Option<&str>) -> Vec<Problem> {
    let run = |program: &str, args: &[&str]| {
        std::process::Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let mut out = Vec::new();
    if runtime == Some("docker") {
        if run("docker", &["info", "--format", "{{.ServerVersion}}"]).is_none() {
            out.push(Problem {
                what: "docker is not installed, not running, or you may not use it".into(),
                fix: "install Docker and add yourself to the `docker` group, or use rootless Podman".into(),
                blocking: true,
            });
        }
    } else {
        podman(&run, user, &mut out);
    }
    if run("git", &["--version"]).is_none() {
        out.push(Problem { what: "git is not installed".into(), fix: "install git".into(), blocking: true });
    }
    if run("loginctl", &["show-user", user, "--property=Linger", "--value"]).as_deref() != Some("yes") {
        out.push(Problem {
            what: "lingering is off: the service stops when you log out and does not start at boot".into(),
            fix: format!("sudo loginctl enable-linger {user}"),
            blocking: false,
        });
    }
    out
}

fn podman(run: &dyn Fn(&str, &[&str]) -> Option<String>, user: &str, out: &mut Vec<Problem>) {
    match run("podman", &["info", "--format", "{{.Host.Security.Rootless}}"]).as_deref() {
        Some("true") => {}
        Some(_) => out.push(Problem {
            what: "podman runs as root here; botler runs as your user with rootless Podman".into(),
            fix: "run botler as an ordinary user (not root or with sudo)".into(),
            blocking: true,
        }),
        None => out.push(Problem {
            what: "podman is not installed or does not work".into(),
            fix: "install Podman 4 or newer (e.g. `sudo apt install podman`, `sudo dnf install podman`) and check \
                  `podman info`"
                .into(),
            blocking: true,
        }),
    }
    for file in ["/etc/subuid", "/etc/subgid"] {
        let listed = std::fs::read_to_string(file)
            .map(|s| s.lines().any(|l| l.split(':').next() == Some(user)))
            .unwrap_or(false);
        if !listed {
            out.push(Problem {
                what: format!("{file} has no range for {user} (containers need subordinate ids)"),
                fix: format!(
                    "sudo usermod --add-subuids 100000-165535 --add-subgids 100000-165535 {user} && podman system migrate"
                ),
                blocking: true,
            });
        }
    }
}
