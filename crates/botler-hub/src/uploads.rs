//! Files the user uploads (`PUT /api/v1/uploads`) and attaches to messages.
//!
//! An upload lands in `uploads/<id>/<name>`. Attaching it to a message to a chat links it into
//! that chat's inbox (`inbox/<instance>/<id>/<name>`), which is mounted read-only at
//! [`INBOX`] in the chat's container: a chat only sees what was sent to it.
//!
//! Agents send files the same way ([`from_agent`]): the hub copies a file from the agent's home
//! (or its monorepo clone, or its inbox) into the upload area.

use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::hub::Hub;

/// Where a chat's attachments appear in its container.
pub const INBOX: &str = "/inbox/files";
const MAX_NAME: usize = 200;
/// The largest file an agent can send.
pub const MAX_AGENT_FILE: u64 = 100 * 1024 * 1024;
/// Served for anything not in [`media_type`]'s list: downloaded, never rendered.
const OCTET: &str = "application/octet-stream";

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("file name {0:?} is not allowed: use a plain name without `/`, at most 200 characters")]
    BadName(String),
    #[error("no upload {0}; upload the file first")]
    Missing(String),
    #[error(
        "{0} is not in your home ({home}), your monorepo clone or {INBOX}: the hub reads files only from there (not \
         /tmp); copy it into your home first",
        home = crate::agents::HOME
    )]
    Outside(String),
    #[error("{0} is not a file")]
    NotAFile(String),
    #[error("no file {0}")]
    NotFound(String),
    #[error("{path} is {size} bytes; files of at most {max} bytes can be sent", max = MAX_AGENT_FILE)]
    TooLarge { path: String, size: u64 },
    #[error("storing the upload: {0}")]
    Io(#[from] std::io::Error),
}

/// A stored upload.
#[derive(Clone, Debug, PartialEq, Serialize, TS)]
pub struct Uploaded {
    /// What a message's `files` names: `<id>/<name>`, served at `GET /api/v1/uploads/<file>`.
    pub file: String,
    pub size: u64,
    /// What it is served as ([`media_type`]); `application/octet-stream` is only downloaded.
    pub mime: &'static str,
}

/// The type a file is served as, by its name's extension: images, video, audio, PDFs and plain
/// text are shown inline; anything else (HTML and SVG among them, which can carry script) is an
/// opaque download.
pub fn media_type(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "pdf" => "application/pdf",
        "txt" | "md" | "log" | "csv" | "tsv" | "json" | "jsonl" | "yaml" | "yml" | "toml" => {
            "text/plain; charset=utf-8"
        }
        _ => OCTET,
    }
}

/// `mime` if it is an image type the web app shows inline (not SVG, which can carry script).
pub fn raster_image(mime: &str) -> Option<&'static str> {
    ["image/png", "image/jpeg", "image/gif", "image/webp", "image/avif"].into_iter().find(|m| *m == mime)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.chars().any(char::is_control)
}

/// Store `body` as a new upload called `name`.
pub async fn save(hub: &Hub, name: &str, body: &[u8]) -> Result<Uploaded, UploadError> {
    if !valid_name(name) {
        return Err(UploadError::BadName(name.into()));
    }
    let id = uuid::Uuid::now_v7().simple().to_string();
    let dir = hub.paths.uploads().join(&id);
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join(name), body).await?;
    tracing::info!(upload = %id, name, size = body.len(), "file uploaded");
    Ok(Uploaded { file: format!("{id}/{name}"), size: body.len() as u64, mime: media_type(name) })
}

/// Where a path in an agent's container is on the host, and the host directory it must stay in:
/// `roots` maps container directories to host directories, the most specific first. Relative
/// paths (and `~/`) are in the home. Paths with `..` are refused rather than resolved.
fn host_path(path: &str, roots: &[(&str, &Path)]) -> Option<(PathBuf, PathBuf)> {
    let home = crate::agents::HOME;
    let abs = match path.strip_prefix("~/") {
        Some(rest) => format!("{home}/{rest}"),
        None if path.starts_with('/') => path.to_string(),
        None => format!("{home}/{path}"),
    };
    let abs = Path::new(&abs);
    if abs.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return None;
    }
    roots.iter().find_map(|(dir, host)| {
        let rest = abs.strip_prefix(dir).ok()?;
        (!rest.as_os_str().is_empty()).then(|| (host.to_path_buf(), host.join(rest)))
    })
}

/// Copy the file at `host` (`path` in the agent's container) into the upload area. The file is
/// opened first and then checked by what was opened (`/proc/self/fd`), so a symlink, or one
/// swapped in meanwhile, cannot reach outside `root`.
fn copy_in(uploads: &Path, path: &str, root: &Path, host: &Path) -> Result<Uploaded, UploadError> {
    use std::io::Read;
    let not_found = |e: std::io::Error| match e.kind() {
        std::io::ErrorKind::NotFound => UploadError::NotFound(path.into()),
        _ => UploadError::Io(e),
    };
    let name = host.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !valid_name(&name) {
        return Err(UploadError::BadName(name));
    }
    let src = std::fs::File::open(host).map_err(not_found)?;
    let opened = std::fs::read_link(format!("/proc/self/fd/{}", std::os::fd::AsRawFd::as_raw_fd(&src)))?;
    if !opened.starts_with(std::fs::canonicalize(root).map_err(not_found)?) {
        return Err(UploadError::Outside(path.into()));
    }
    let meta = src.metadata()?;
    if !meta.is_file() {
        return Err(UploadError::NotAFile(path.into()));
    }
    if meta.len() > MAX_AGENT_FILE {
        return Err(UploadError::TooLarge { path: path.into(), size: meta.len() });
    }
    let id = uuid::Uuid::now_v7().simple().to_string();
    let dir = uploads.join(&id);
    std::fs::create_dir_all(&dir)?;
    // The file may grow while it is copied: never more than the limit.
    let copied = std::io::copy(&mut src.take(MAX_AGENT_FILE + 1), &mut std::fs::File::create(dir.join(&name))?)?;
    if copied > MAX_AGENT_FILE {
        std::fs::remove_dir_all(&dir)?;
        return Err(UploadError::TooLarge { path: path.into(), size: copied });
    }
    Ok(Uploaded { file: format!("{id}/{name}"), size: copied, mime: media_type(&name) })
}

/// Store files from `instance`'s container as uploads: `paths` in its home, its monorepo clone
/// or its inbox; any type, at most [`MAX_AGENT_FILE`] each.
pub async fn from_agent(hub: &Hub, instance: &str, paths: &[String]) -> crate::hub::Result<Vec<Uploaded>> {
    let inst = hub.store.instance(instance).await?;
    let env = hub.applied().await.defs.agents.get(&inst.definition).map(|d| d.spec.env.clone()).unwrap_or_default();
    let home = crate::agents::home_dir(hub, &inst, &env);
    let workspace = hub.paths.workspace(&inst.name);
    let inbox = hub.paths.inbox(&inst.name);
    let mut roots: Vec<(&str, &Path)> = Vec::new();
    if env.workspace() == botler_core::defs::Workspace::Monorepo {
        roots.push((crate::agents::MONOREPO, &workspace));
    }
    roots.extend([(crate::agents::HOME, home.as_path()), (INBOX, inbox.as_path())]);
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        let (root, host) = host_path(path, &roots).ok_or_else(|| UploadError::Outside(path.clone()))?;
        let (uploads, p) = (hub.paths.uploads(), path.clone());
        let up = tokio::task::spawn_blocking(move || copy_in(&uploads, &p, &root, &host))
            .await
            .map_err(std::io::Error::other)??;
        tracing::info!(instance, file = %up.file, size = up.size, "agent sent a file");
        out.push(up);
    }
    Ok(out)
}

/// The stored path of `file` (`<id>/<name>`), checked to be an existing upload.
fn stored(hub: &Hub, file: &str) -> Result<PathBuf, UploadError> {
    let missing = || UploadError::Missing(file.into());
    let (id, name) = file.split_once('/').ok_or_else(missing)?;
    let id_ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit());
    if !id_ok || !valid_name(name) {
        return Err(missing());
    }
    let path = hub.paths.uploads().join(id).join(name);
    if !path.is_file() {
        return Err(missing());
    }
    Ok(path)
}

/// An upload as a message shows it (with its size and type), or `None` when it is gone.
pub async fn info(hub: &Hub, file: &str) -> Result<Option<Uploaded>, UploadError> {
    let path = match stored(hub, file) {
        Ok(p) => p,
        Err(UploadError::Missing(_)) => return Ok(None),
        Err(e) => return Err(e),
    };
    let size = tokio::fs::metadata(&path).await?.len();
    Ok(Some(Uploaded { file: file.into(), size, mime: media_type(file) }))
}

/// An upload's bytes and how to serve them: as its [`media_type`], inline unless that is an
/// opaque download.
pub async fn read(hub: &Hub, file: &str) -> Result<(Vec<u8>, &'static str, bool), UploadError> {
    let path = stored(hub, file)?;
    let mime = media_type(file);
    Ok((tokio::fs::read(path).await?, mime, mime != OCTET))
}

/// Give `instance` the uploads `files`; returns where it finds them in its container.
pub async fn attach(hub: &Hub, instance: &str, files: &[String]) -> Result<Vec<String>, UploadError> {
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        let src = stored(hub, file)?;
        let dst = hub.paths.inbox(instance).join(file);
        if let Some(dir) = dst.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        link(&src, &dst).await?;
        out.push(format!("{INBOX}/{file}"));
    }
    Ok(out)
}

/// A hard link (no copy; uploads and inboxes share the data directory), unless already there.
async fn link(src: &Path, dst: &Path) -> std::io::Result<()> {
    match tokio::fs::hard_link(src, dst).await {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["photo.jpg", "Scan 2026-10-30.pdf", ".env.example", "ünïcode.txt"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", ".", "..", "a/b", "..\\x", "a\nb", &"x".repeat(201)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn types() {
        assert_eq!(media_type("demo.MP4"), "video/mp4");
        assert_eq!(media_type("a.pdf"), "application/pdf");
        assert_eq!(media_type("notes.md"), "text/plain; charset=utf-8");
        for active in ["page.html", "x.svg", "x.xhtml", "noext", "a.js"] {
            assert_eq!(media_type(active), OCTET, "{active}");
        }
        assert_eq!(raster_image("image/png"), Some("image/png"));
        assert_eq!(raster_image("image/svg+xml"), None);
    }

    #[test]
    fn container_paths() {
        let (home, repo, inbox) = (Path::new("/data/homes/c"), Path::new("/data/ws/c"), Path::new("/data/inbox/c"));
        let roots = [("/home/agent/monorepo", repo), ("/home/agent", home), (INBOX, inbox)];
        let host = |p| host_path(p, &roots).map(|(root, h)| (root.display().to_string(), h.display().to_string()));
        let at = |root: &str, h: &str| Some((root.to_string(), h.to_string()));
        assert_eq!(host("/home/agent/out/a.png"), at("/data/homes/c", "/data/homes/c/out/a.png"));
        assert_eq!(host("~/a.png"), at("/data/homes/c", "/data/homes/c/a.png"));
        assert_eq!(host("out/a.png"), at("/data/homes/c", "/data/homes/c/out/a.png"));
        assert_eq!(host("/home/agent/monorepo/x.pdf"), at("/data/ws/c", "/data/ws/c/x.pdf"));
        assert_eq!(host("/inbox/files/1/a.jpg"), at("/data/inbox/c", "/data/inbox/c/1/a.jpg"));
        for refused in
            ["/tmp/a.png", "/home/agent/../etc/passwd", "../x", "/home/agentx/a", "/home/agent", "/etc/hosts"]
        {
            assert_eq!(host(refused), None, "{refused}");
        }
    }

    #[test]
    fn copy_in_stays_in_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let (home, uploads) = (dir.path().join("home"), dir.path().join("uploads"));
        std::fs::create_dir_all(home.join("out")).unwrap();
        std::fs::write(home.join("out/demo.mp4"), b"video").unwrap();
        std::fs::write(dir.path().join("secret.txt"), b"secret").unwrap();
        let copy = |name: &str| copy_in(&uploads, name, &home, &home.join(name));

        let up = copy("out/demo.mp4").unwrap();
        assert_eq!((up.size, up.mime), (5, "video/mp4"));
        assert_eq!(std::fs::read(uploads.join(&up.file)).unwrap(), b"video");

        std::os::unix::fs::symlink(dir.path().join("secret.txt"), home.join("link.txt")).unwrap();
        assert!(matches!(copy("link.txt"), Err(UploadError::Outside(_))), "a symlink out of the home");
        std::os::unix::fs::symlink(dir.path(), home.join("up")).unwrap();
        assert!(matches!(copy("up/secret.txt"), Err(UploadError::Outside(_))), "a symlinked directory");
        std::os::unix::fs::symlink(home.join("out/demo.mp4"), home.join("inside.mp4")).unwrap();
        assert!(copy("inside.mp4").is_ok(), "a symlink within the home");
        assert!(matches!(copy("missing.png"), Err(UploadError::NotFound(_))));
        assert!(matches!(copy("out"), Err(UploadError::NotAFile(_))));

        let big = std::fs::File::create(home.join("big.bin")).unwrap();
        big.set_len(MAX_AGENT_FILE + 1).unwrap();
        assert!(matches!(copy("big.bin"), Err(UploadError::TooLarge { .. })));
    }
}
