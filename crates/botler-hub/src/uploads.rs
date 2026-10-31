//! Files the user uploads (`PUT /api/v1/uploads`) and attaches to messages.
//!
//! An upload lands in `uploads/<id>/<name>`. Attaching it to a message to a chat links it into
//! that chat's inbox (`inbox/<instance>/<id>/<name>`), which is mounted read-only at
//! [`INBOX`] in the chat's container: a chat only sees what was sent to it.

use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::hub::Hub;

/// Where a chat's attachments appear in its container.
pub const INBOX: &str = "/inbox/files";
const MAX_NAME: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("file name {0:?} is not allowed: use a plain name without `/`, at most 200 characters")]
    BadName(String),
    #[error("no upload {0}; upload the file first")]
    Missing(String),
    #[error("storing the upload: {0}")]
    Io(#[from] std::io::Error),
}

/// A stored upload.
#[derive(Debug, Serialize, TS)]
pub struct Uploaded {
    /// What a message's `files` names: `<id>/<name>`.
    pub file: String,
    pub size: u64,
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
    Ok(Uploaded { file: format!("{id}/{name}"), size: body.len() as u64 })
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

/// An upload's bytes and how to serve them: images (not SVG, which can carry script) inline with
/// their type, anything else as an attachment.
pub async fn read(hub: &Hub, file: &str) -> Result<(Vec<u8>, &'static str, bool), UploadError> {
    let path = stored(hub, file)?;
    let ext = file.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let image = match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "avif" => Some("image/avif"),
        _ => None,
    };
    Ok((tokio::fs::read(path).await?, image.unwrap_or("application/octet-stream"), image.is_some()))
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
}
