//! What an agent's container is made of, as one comparable value. It is computed when a container
//! starts and again after every change to the applied definitions or the chat's grants: when it
//! differs, the agent is restarted between turns (same session file, home and grants), and told
//! what changed. Not part of it: the memory index and the chat's title (the prompt is rendered per
//! wake but these change without a merge) and secret values (entered by the user, read at start).

use botler_core::defs::{AgentDef, Environment, ResourceKind};
use botler_store::Instance;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hub::{Applied, Hub, HubError, Result};
use crate::images::{self, Recipe};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentEnv {
    /// The image tag (content-addressed: covers the Dockerfile, its inputs and the base).
    pub image: String,
    /// The definition's environment plus what the chat holds.
    pub env: Environment,
    /// Grants that change the container (`secret:…`, `service:…`, `mount:…`), sorted.
    pub held: Vec<String>,
    /// Python tools it gets, by name.
    pub tools: Vec<String>,
    /// Tree ids of the tool code (`tools/`, `packages/`, `connectors/`), when it has tools.
    pub tool_code: String,
    /// Tree id of `pi/` (skills, prompts, extensions).
    pub pi: Option<String>,
    /// Hash of `AGENTS.md`.
    pub instructions: String,
    /// Hash of the rest of the definition and its model route.
    pub definition: String,
}

fn hash(v: &impl Serialize) -> Result<String> {
    let json = serde_json::to_vec(v).map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    Ok(hex::encode(&Sha256::digest(&json)[..8]))
}

/// The environment `instance` of `def` gets at `applied`. `image` is the tag when already known
/// (at wake); otherwise it is resolved without building.
pub async fn compute(
    hub: &Hub,
    instance: &Instance,
    def: &AgentDef,
    applied: &Applied,
    image: Option<String>,
) -> Result<AgentEnv> {
    let image = match image {
        Some(i) => i,
        None => images::tag(hub, &Recipe::agent(def.spec.env.image()), &applied.commit).await?,
    };
    let mut held: Vec<String> = hub
        .store
        .live_grants(&instance.name)
        .await?
        .iter()
        .filter(|g| g.kind != ResourceKind::Event)
        .map(|g| g.resource().to_string())
        .collect();
    held.sort();
    let (granted, _) = crate::grants::environment(hub, &instance.name).await?;
    let (env, _) = def.spec.env.extend(&granted);
    let tools: Vec<String> = applied.defs.tools_for(&env).into_iter().map(|t| t.name.clone()).collect();
    let tool_code = if tools.is_empty() {
        String::new()
    } else {
        hash(&hub.repo.path_ids(&applied.commit, &["tools", "packages", "connectors"]).await?)?
    };
    let mut spec = def.spec.clone();
    spec.env = Environment::default();
    spec.triggers.clear(); // triggers route events; they do not change the container
    Ok(AgentEnv {
        image,
        env,
        held,
        tools,
        tool_code,
        pi: images::pi_export_name(hub, &applied.commit).await?,
        instructions: hash(&def.prompt)?,
        definition: hash(&(spec, applied.defs.route_for(&def.spec)))?,
    })
}

/// What changed from `old` to `new`, in words for the agent.
pub fn changes(old: &AgentEnv, new: &AgentEnv) -> Vec<String> {
    fn diff(out: &mut Vec<String>, what: &str, old: &[String], new: &[String]) {
        let added: Vec<&str> = new.iter().filter(|n| !old.contains(n)).map(String::as_str).collect();
        let removed: Vec<&str> = old.iter().filter(|o| !new.contains(o)).map(String::as_str).collect();
        if !added.is_empty() {
            out.push(format!("new {what}: {}", added.join(", ")));
        }
        if !removed.is_empty() {
            out.push(format!("{what} removed: {}", removed.join(", ")));
        }
    }
    let mut out = Vec::new();
    diff(&mut out, "tools", &old.tools, &new.tools);
    if old.tool_code != new.tool_code && !old.tools.is_empty() && !new.tools.is_empty() {
        out.push("tool code updated".into());
    }
    diff(&mut out, "access", &old.held, &new.held);
    diff(&mut out, "secret groups", &old.env.secrets, &new.env.secrets);
    diff(&mut out, "services", &old.env.mcp, &new.env.mcp);
    let mounts = |e: &AgentEnv| e.env.mounts.iter().map(|(n, m)| format!("{n} ({})", m.as_str())).collect::<Vec<_>>();
    diff(&mut out, "shares", &mounts(old), &mounts(new));
    if old.env.mcp_stdio != new.env.mcp_stdio {
        out.push("MCP servers changed".into());
    }
    if old.env.network() != new.env.network() {
        out.push(format!("network: {:?}", new.env.network()).to_lowercase());
    }
    if old.image != new.image {
        out.push("new container image (installed software changed)".into());
    }
    if old.pi != new.pi {
        out.push("pi skills, prompts or extensions updated".into());
    }
    if old.instructions != new.instructions {
        out.push("your instructions (AGENTS.md) changed".into());
    }
    if old.definition != new.definition {
        out.push("your agent definition or model changed".into());
    }
    if out.is_empty() && old != new {
        out.push("your environment changed".into());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_described() {
        let old = AgentEnv { tools: vec!["web.fetch".into()], image: "a".into(), ..Default::default() };
        let mut new = old.clone();
        new.tools.push("weather.forecast".into());
        new.image = "b".into();
        assert_eq!(
            changes(&old, &new),
            ["new tools: weather.forecast", "new container image (installed software changed)"]
        );
        assert!(changes(&old, &old).is_empty());
    }
}
