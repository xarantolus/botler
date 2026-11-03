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
