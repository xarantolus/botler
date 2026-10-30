//! A read-only view of a file tree, so definitions load the same way from a git commit, a
//! worktree on disk, or memory (tests).

use std::collections::BTreeMap;
use std::io;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TreeEntry {
    pub name: String,
    pub is_dir: bool,
}

pub trait Tree {
    /// Read a file by `/`-separated path relative to the repo root. `Ok(None)` if absent.
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>>;
    /// Direct children of a directory, sorted by name. Empty if the directory is absent.
    fn list(&self, dir: &str) -> io::Result<Vec<TreeEntry>>;
}

/// In-memory tree keyed by full path.
#[derive(Clone, Debug, Default)]
pub struct MemTree {
    files: BTreeMap<String, Vec<u8>>,
}

impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, path: &str, contents: impl AsRef<[u8]>) -> Self {
        self.insert(path, contents);
        self
    }

    pub fn insert(&mut self, path: &str, contents: impl AsRef<[u8]>) {
        self.files.insert(path.trim_matches('/').to_string(), contents.as_ref().to_vec());
    }

    pub fn remove(&mut self, path: &str) {
        self.files.remove(path);
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }
}

impl Tree for MemTree {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.files.get(path.trim_matches('/')).cloned())
    }

    fn list(&self, dir: &str) -> io::Result<Vec<TreeEntry>> {
        let dir = dir.trim_matches('/');
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let mut out: BTreeMap<String, bool> = BTreeMap::new();
        for path in self.files.keys() {
            if let Some(rest) = path.strip_prefix(&prefix) {
                match rest.split_once('/') {
                    Some((child, _)) => {
                        out.insert(child.to_string(), true);
                    }
                    None => {
                        out.entry(rest.to_string()).or_insert(false);
                    }
                }
            }
        }
        Ok(out.into_iter().map(|(name, is_dir)| TreeEntry { name, is_dir }).collect())
    }
}

/// A tree rooted at a directory on disk.
#[derive(Clone, Debug)]
pub struct FsTree {
    root: std::path::PathBuf,
}

impl FsTree {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Tree for FsTree {
    fn read(&self, path: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.root.join(path)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::IsADirectory => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn list(&self, dir: &str) -> io::Result<Vec<TreeEntry>> {
        let rd = match std::fs::read_dir(self.root.join(dir)) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            out.push(TreeEntry { name, is_dir: entry.file_type()?.is_dir() });
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memtree_lists_children() {
        let t = MemTree::new().with("a/b/c.toml", "").with("a/d.toml", "").with("e", "");
        assert_eq!(
            t.list("a").unwrap(),
            vec![TreeEntry { name: "b".into(), is_dir: true }, TreeEntry { name: "d.toml".into(), is_dir: false }]
        );
        assert_eq!(t.list("").unwrap().len(), 2);
        assert!(t.list("missing").unwrap().is_empty());
    }
}
