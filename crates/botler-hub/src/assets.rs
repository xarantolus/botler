//! Files shipped inside the binary and unpacked into the data directory at startup, so the
//! base image, pi extension and Python SDK always match the running hub.

use std::path::Path;

use crate::config::Paths;

pub const AGENT_BASE_DOCKERFILE: &str = include_str!("../../../images/agent-base/Dockerfile");

/// Image tag of the agent base image built from [`AGENT_BASE_DOCKERFILE`].
pub fn agent_base_tag() -> String {
    format!("localhost/botler/agent-base:{}", env!("CARGO_PKG_VERSION"))
}

/// The tag monorepo images build `FROM`.
pub const AGENT_BASE_LATEST: &str = "localhost/botler/agent-base:latest";

/// Write all shipped assets under `paths.assets()`. Idempotent.
pub fn unpack(paths: &Paths) -> std::io::Result<()> {
    let root = paths.assets();
    write(&root.join("agent-base/Dockerfile"), AGENT_BASE_DOCKERFILE)?;
    // Mount sources must exist even while a package ships no files.
    std::fs::create_dir_all(root.join("extension"))?;
    std::fs::create_dir_all(root.join("sdk"))?;
    for (rel, body) in crate::embedded::EXTENSION {
        write(&root.join("extension").join(rel), body)?;
    }
    for (rel, body) in crate::embedded::SDK {
        write(&root.join("sdk").join(rel), body)?;
    }
    Ok(())
}

fn write(path: &Path, body: &str) -> std::io::Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == body) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(tmp, path)
}
