//! Grants: resources a chat holds beyond its definition, and `policies/grants.toml`, which says
//! per resource whether a chat may get it without asking the user.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Environment;

/// What kind of thing a grant gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    /// A secret group from `secrets.toml` (container class).
    Secret,
    /// A network share, mounted into the container.
    Mount,
    /// A service's MCP server.
    Service,
    /// Subscribing to a `restricted` event type.
    Event,
}

impl ResourceKind {
    pub const ALL: [Self; 4] = [Self::Secret, Self::Mount, Self::Service, Self::Event];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Mount => "mount",
            Self::Service => "service",
            Self::Event => "event",
        }
    }
}

/// A grantable resource, written `<kind>:<name>`, e.g. `secret:github`, `event:mail.received`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Resource {
    pub kind: ResourceKind,
    pub name: String,
}

impl Resource {
    pub fn new(kind: ResourceKind, name: impl Into<String>) -> Self {
        Self { kind, name: name.into() }
    }
}

impl fmt::Display for Resource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.name)
    }
}

impl FromStr for Resource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let kinds = || ResourceKind::ALL.map(|k| format!("`{}:<name>`", k.as_str())).join(", ");
        let (kind, name) = s.split_once(':').ok_or_else(|| format!("resource {s:?} must be one of {}", kinds()))?;
        let kind = ResourceKind::ALL
            .into_iter()
            .find(|k| k.as_str() == kind)
            .ok_or_else(|| format!("unknown resource kind {kind:?}; use {}", kinds()))?;
        if name.is_empty() {
            return Err(format!("resource {s:?} names nothing"));
        }
        Ok(Self { kind, name: name.into() })
    }
}

/// How a share is mounted (`ro` < `rw`).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema, ts_rs::TS,
)]
pub enum MountMode {
    #[default]
    #[serde(rename = "ro")]
    ReadOnly,
    #[serde(rename = "rw")]
    ReadWrite,
}

impl MountMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "ro",
            Self::ReadWrite => "rw",
        }
    }
}

/// Whether a chat gets a resource without asking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GrantMode {
    /// The user decides each request (the default).
    #[default]
    Ask,
    /// Granted without asking; still recorded and shown.
    Auto,
}

/// `policies/grants.toml` (protected).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GrantPolicy {
    #[serde(default = "super::model::default_version")]
    pub version: u32,
    /// `"<kind>:<name>" = "ask" | "auto"`; resources not listed are `ask`.
    #[serde(default)]
    pub resources: BTreeMap<String, GrantMode>,
}

impl GrantPolicy {
    pub fn mode(&self, r: &Resource) -> GrantMode {
        self.resources.get(&r.to_string()).copied().unwrap_or_default()
    }
}

/// The environment additions a set of held resources (with their mount modes) make: secret
/// groups, services and mounts (event subscriptions do not change the container).
pub fn environment_of<'a>(held: impl IntoIterator<Item = (&'a Resource, Option<MountMode>)>) -> Environment {
    let mut env = Environment::default();
    for (r, mode) in held {
        match r.kind {
            ResourceKind::Secret => env.secrets.push(r.name.clone()),
            // A grant reaches the service; it is no MCP server unless the definition says so.
            ResourceKind::Service => env.services.push(r.name.clone()),
            ResourceKind::Mount => {
                let m = env.mounts.entry(r.name.clone()).or_default();
                *m = (*m).max(mode.unwrap_or_default());
            }
            ResourceKind::Event => {}
        }
    }
    env
}

/// Whether environment `env` (a definition's) already gives `r` (a mount at least in `mode`).
pub fn env_gives(env: &Environment, r: &Resource, mode: Option<MountMode>) -> bool {
    match r.kind {
        ResourceKind::Secret => env.secrets.contains(&r.name),
        ResourceKind::Service => env.reached().contains(&r.name),
        ResourceKind::Mount => env.mounts.get(&r.name).is_some_and(|m| *m >= mode.unwrap_or_default()),
        ResourceKind::Event => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_parse_and_print() {
        let r: Resource = "event:mail.received".parse().unwrap();
        assert_eq!(r, Resource::new(ResourceKind::Event, "mail.received"));
        assert_eq!(r.to_string(), "event:mail.received");
        assert!("github".parse::<Resource>().unwrap_err().contains("`secret:<name>`"));
        assert!("disk:x".parse::<Resource>().unwrap_err().contains("unknown resource kind"));
        assert!("secret:".parse::<Resource>().is_err());
    }

    #[test]
    fn policy_defaults_to_ask_and_env_collects_secrets_services_and_mounts() {
        let p: GrantPolicy = toml::from_str("[resources]\n\"secret:nas\" = \"auto\"\n").unwrap();
        assert_eq!(p.mode(&"secret:nas".parse().unwrap()), GrantMode::Auto);
        assert_eq!(p.mode(&"secret:github".parse().unwrap()), GrantMode::Ask);
        let held: Vec<Resource> =
            ["secret:a", "service:notes", "mount:nas", "event:x.y"].iter().map(|s| s.parse().unwrap()).collect();
        let ro = Some(MountMode::ReadOnly);
        let env = environment_of(held.iter().map(|r| (r, ro)));
        assert_eq!(env.secrets, ["a"]);
        assert_eq!((env.mcp.len(), env.services.clone()), (0, vec!["notes".to_string()]), "a grant is no MCP server");
        assert_eq!(env.mounts, BTreeMap::from([("nas".to_string(), MountMode::ReadOnly)]));
        assert!(env_gives(&env, &held[0], None) && env_gives(&env, &held[2], None));
        assert!(!env_gives(&env, &held[2], Some(MountMode::ReadWrite)) && !env_gives(&env, &held[3], None));
        // A read-write grant of the same share wins.
        let both = [(&held[2], ro), (&held[2], Some(MountMode::ReadWrite))];
        assert_eq!(environment_of(both).mounts["nas"], MountMode::ReadWrite);
    }
}
