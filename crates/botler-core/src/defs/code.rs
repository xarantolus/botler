//! What a job's code is: the repo paths its Dockerfile copies into the image, and of the uv lock
//! the packages it installs. The rest of the build context (other tools, other jobs' packages) is
//! there for other images: changing it does not change this job.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::Tree;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobCode {
    /// Repo paths (files or directories) the job is built from: its directory and what its
    /// Dockerfile copies in, or its whole build context when the Dockerfile copies something this
    /// cannot follow (`.`, a glob, a variable, a bind mount).
    pub paths: Vec<String>,
    /// A hash of the `uv.lock` packages the job installs (its own package and what it depends on),
    /// when it installs from the lock: a lock change elsewhere is then no change of the job.
    pub locked: Option<String>,
}

impl JobCode {
    /// Whether `changed` (repo paths) changes the job, against its code before (`None`: new).
    pub fn touched(&self, before: Option<&JobCode>, changed: &[String]) -> bool {
        let lock_moved = || before.is_none_or(|b| b.locked != self.locked);
        changed.iter().any(|p| {
            self.paths.iter().any(|i| p == i || p.starts_with(&format!("{i}/")))
                && (p != "uv.lock" || self.locked.is_none() || lock_moved())
        })
    }
}

/// The code of job `name` (with a Dockerfile, or `from` an upstream image without one).
pub fn job_code(tree: &dyn Tree, name: &str) -> JobCode {
    let dir = format!("jobs/{name}");
    let dockerfile = match tree.read(&format!("{dir}/Dockerfile")) {
        Ok(Some(bytes)) => String::from_utf8(bytes).ok(),
        // An upstream image: only its job directory is the hub's.
        Ok(None) => return JobCode { paths: vec![dir], locked: None },
        Err(_) => None,
    };
    let Some(paths) = dockerfile.as_deref().and_then(copied) else {
        return JobCode { paths: super::job_inputs(name), locked: None };
    };
    let mut paths: Vec<String> = paths.into_iter().collect();
    if !paths.contains(&dir) {
        paths.push(dir.clone());
    }
    let locked = if paths.iter().any(|p| p == "uv.lock") { locked(tree, &dir) } else { None };
    JobCode { paths, locked }
}

/// The repo paths a Dockerfile copies in (and the monorepo images it builds on), or `None` when
/// it copies something that cannot be told from the text.
fn copied(dockerfile: &str) -> Option<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let joined = dockerfile.replace("\\\r\n", " ").replace("\\\n", " ");
    for line in joined.lines().map(str::trim) {
        let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match word.to_ascii_uppercase().as_str() {
            "FROM" => {
                let image = rest.split_whitespace().find(|w| !w.starts_with("--")).unwrap_or("");
                if let Some(base) = image.strip_prefix("botler/image-") {
                    out.insert(format!("images/{}", base.split([':', '@']).next().unwrap_or(base)));
                }
            }
            "COPY" | "ADD" => {
                // Its flags, then the sources and the destination (words, or a JSON array).
                let mut rest = rest;
                while let Some(flag) = rest.strip_prefix("--") {
                    if flag.starts_with("from") {
                        // From another stage or image, not the repo.
                        rest = "";
                        break;
                    }
                    rest = flag.split_once(char::is_whitespace).map_or("", |(_, r)| r.trim_start());
                }
                if rest.is_empty() {
                    continue;
                }
                let mut args: Vec<String> = if rest.starts_with('[') {
                    serde_json::from_str(rest).ok()?
                } else {
                    rest.split_whitespace().map(String::from).collect()
                };
                args.pop();
                for src in args {
                    if src.contains("://") {
                        continue;
                    }
                    let p = src.trim_start_matches("./").trim_end_matches('/');
                    if p.is_empty() || p == "." || p.contains(['*', '?', '[', '$']) || p.split('/').any(|s| s == "..") {
                        return None;
                    }
                    out.insert(p.to_string());
                }
            }
            "RUN" if rest.contains("--mount") && (rest.contains("type=bind") || rest.contains("source=")) => {
                return None;
            }
            _ => {}
        }
    }
    Some(out)
}

/// A hash of the `uv.lock` entries of the package in `dir` and everything it depends on.
fn locked(tree: &dyn Tree, dir: &str) -> Option<String> {
    let read = |path: &str| tree.read(path).ok().flatten().and_then(|b| String::from_utf8(b).ok());
    let project: toml::Table = toml::from_str(&read(&format!("{dir}/pyproject.toml"))?).ok()?;
    let name = project.get("project")?.get("name")?.as_str()?.to_string();
    let lock: toml::Table = toml::from_str(&read("uv.lock")?).ok()?;
    let packages = lock.get("package")?.as_array()?;
    // Package names compare normalized, as uv writes them.
    let norm = |n: &str| n.to_ascii_lowercase().replace(['_', '.'], "-");
    let mut seen = BTreeSet::new();
    let mut todo = vec![norm(&name)];
    let mut entries = Vec::new();
    while let Some(n) = todo.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        for p in packages.iter().filter(|p| p.get("name").and_then(|v| v.as_str()).map(norm) == Some(n.clone())) {
            entries.push(serde_json::to_string(&serde_json::to_value(p).ok()?).ok()?);
            let deps = p.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten();
            for d in deps {
                let Some(dep) = d.get("name").and_then(|v| v.as_str()) else { continue };
                todo.push(norm(dep));
                // An extra it asks for brings that extra's dependencies.
                let extras = d.get("extra").and_then(|e| e.as_array()).into_iter().flatten().filter_map(|e| e.as_str());
                for extra in extras {
                    let target =
                        packages.iter().filter(|q| q.get("name").and_then(|v| v.as_str()).map(norm) == Some(norm(dep)));
                    for q in target {
                        let more = q.get("optional-dependencies").and_then(|o| o.get(extra)).and_then(|v| v.as_array());
                        todo.extend(more.into_iter().flatten().filter_map(|m| m.get("name")?.as_str()).map(norm));
                    }
                }
            }
        }
    }
    entries.sort();
    Some(hex::encode(Sha256::digest(entries.join("\n").as_bytes())))
}
