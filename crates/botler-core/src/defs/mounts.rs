//! Network shares (`mounts/<name>.toml`), mounted with rclone (FUSE) inside the containers that
//! hold them. The connection details come from a secret group whose secret names end in the rclone
//! option they fill (`NAS_HOST`, `NAS_USER`, `NAS_PASS`).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::MountMode;

/// The rclone backend a share is reached with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MountType {
    Smb,
    Sftp,
    Webdav,
}

impl MountType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smb => "smb",
            Self::Sftp => "sftp",
            Self::Webdav => "webdav",
        }
    }

    /// The rclone options its secret group may fill (secret name suffixes), the required one first.
    pub fn options(self) -> &'static [&'static str] {
        match self {
            Self::Smb => &["HOST", "USER", "PASS", "PORT", "DOMAIN", "SPN"],
            Self::Sftp => &["HOST", "USER", "PASS", "PORT", "KEY_PEM", "PUBKEY", "KNOWN_HOSTS_FILE"],
            Self::Webdav => &["URL", "USER", "PASS", "VENDOR", "BEARER_TOKEN"],
        }
    }
}

/// A value in a mount's `options`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

/// `mounts/<name>.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MountFile {
    #[serde(default = "super::model::default_version")]
    pub version: u32,
    #[serde(rename = "type")]
    pub kind: MountType,
    /// The secret group with the connection details: each secret's name ends in the rclone option
    /// it fills, e.g. `NAS_HOST`, `NAS_USER`, `NAS_PASS` (smb), `..._URL` (webdav).
    pub secrets: String,
    /// The path on the remote: for smb the share and a folder in it (`photos/2024`).
    #[serde(default)]
    pub remote_path: String,
    /// Where it appears in the container, e.g. `/mnt/nas-photos`.
    pub path: String,
    /// `true` (the default): it is only ever mounted read-only. `false` lets chats be granted
    /// `rw`, which the user approves.
    #[serde(default = "yes")]
    pub read_only: bool,
    /// Extra `rclone mount` flags without the dashes, e.g. `{ vfs_cache_max_size = "2G" }`.
    #[serde(default)]
    pub options: BTreeMap<String, OptionValue>,
}

fn yes() -> bool {
    true
}

/// Options the entrypoint sets itself.
const RESERVED: &[&str] = &["read_only", "daemon", "daemon_wait", "rc", "config", "log_file", "vfs_cache_mode"];
/// Mount points that would hide what the container needs.
const RESERVED_PATHS: &[&str] = &["/botler", "/opt", "/proc", "/sys", "/dev", "/run", "/etc", "/usr", "/bin", "/lib"];

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MountDef {
    pub name: String,
    pub spec: MountFile,
}

impl MountDef {
    /// The rclone remote name (rclone reads `RCLONE_CONFIG_<REMOTE>_*`, which allows no `-`).
    pub fn remote(&self) -> String {
        self.name.replace('-', "_")
    }

    /// The env var that sets rclone `option` for this share.
    pub fn env_var(&self, option: &str) -> String {
        format!("RCLONE_CONFIG_{}_{option}", self.remote().to_uppercase())
    }

    /// The rclone option secret `name` fills: the longest option it ends in.
    pub fn option_for(&self, secret: &str) -> Option<&'static str> {
        self.spec
            .kind
            .options()
            .iter()
            .filter(|o| secret == **o || secret.ends_with(&format!("_{o}")))
            .max_by_key(|o| o.len())
            .copied()
    }

    /// The `rclone mount` flags of `options`.
    pub fn flags(&self) -> Vec<String> {
        self.spec
            .options
            .iter()
            .map(|(k, v)| {
                let k = k.replace('_', "-");
                match v {
                    OptionValue::Bool(b) => format!("--{k}={b}"),
                    OptionValue::Int(i) => format!("--{k}={i}"),
                    OptionValue::Str(s) => format!("--{k}={s}"),
                }
            })
            .collect()
    }

    /// One line of the list the container's entrypoint mounts:
    /// `name|remote|remote path|mount point|ro or rw|flags`.
    pub fn list_line(&self, mode: MountMode) -> String {
        let p = &self.spec;
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.name,
            self.remote(),
            p.remote_path,
            p.path,
            mode.as_str(),
            self.flags().join(" ")
        )
    }

    /// Problems with this definition on its own (the secret group is checked by the loader).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        let p = &self.spec;
        let path = p.path.trim_end_matches('/');
        if !p.path.starts_with('/') || path.is_empty() || p.path.contains("..") {
            out.push(format!("path {:?} must be an absolute directory such as /mnt/{}", p.path, self.name));
        } else if let Some(r) = RESERVED_PATHS.iter().find(|r| path == **r || path.starts_with(&format!("{r}/"))) {
            out.push(format!("path {:?} is inside {r}, which the container needs; use /mnt/{}", p.path, self.name));
        }
        for field in [&p.path, &p.remote_path] {
            if field.contains('|') || field.contains('\n') {
                out.push(format!("{field:?} may not contain `|` or line breaks"));
            }
        }
        for (k, v) in &p.options {
            if k.is_empty() || !k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                out.push(format!("option {k:?} must be snake_case, e.g. vfs_cache_max_size"));
            } else if RESERVED.contains(&k.as_str()) {
                out.push(format!("option {k:?} is set by botler (use `read_only` and grants for the mode)"));
            }
            if matches!(v, OptionValue::Str(s) if s.contains(['|', '\n', ' '])) {
                out.push(format!("option {k:?} may not contain spaces, `|` or line breaks"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(toml: &str) -> MountDef {
        MountDef { name: "nas-photos".into(), spec: toml::from_str(toml).unwrap() }
    }

    #[test]
    fn secrets_map_to_options_and_the_line_is_rendered() {
        let d = def(
            "type = \"smb\"\nsecrets = \"nas\"\nremote_path = \"photos\"\npath = \"/mnt/photos\"\noptions = { vfs_cache_max_size = \"2G\", dir_cache_time = 30 }\n",
        );
        assert!(d.spec.read_only);
        assert_eq!(d.remote(), "nas_photos");
        assert_eq!(d.env_var("HOST"), "RCLONE_CONFIG_NAS_PHOTOS_HOST");
        assert_eq!(d.option_for("NAS_PASS"), Some("PASS"));
        assert_eq!(d.option_for("HOST"), Some("HOST"));
        assert_eq!(d.option_for("NAS_PASSWORD"), None);
        assert_eq!(
            d.list_line(MountMode::ReadOnly),
            "nas-photos|nas_photos|photos|/mnt/photos|ro|--dir-cache-time=30 --vfs-cache-max-size=2G"
        );
        assert!(d.problems().is_empty(), "{:?}", d.problems());
        let s = def("type = \"sftp\"\nsecrets = \"s\"\npath = \"/mnt/x\"\n");
        assert_eq!(s.option_for("BOX_KNOWN_HOSTS_FILE"), Some("KNOWN_HOSTS_FILE"));
    }

    #[test]
    fn bad_paths_and_options_are_named() {
        let d =
            def("type = \"webdav\"\nsecrets = \"d\"\npath = \"/botler/x\"\noptions = { daemon = true, \"Bad\" = 1 }\n");
        let p = d.problems();
        assert_eq!(p.len(), 3, "{p:?}");
        assert!(p[0].contains("inside /botler"));
        assert!(def("type = \"smb\"\nsecrets = \"d\"\npath = \"mnt\"\n").problems()[0].contains("absolute"));
        assert!(toml::from_str::<MountFile>("type = \"nfs\"\nsecrets = \"d\"\npath = \"/mnt/x\"\n").is_err());
    }
}
