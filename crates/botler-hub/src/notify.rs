//! Telling the user something needs them. Live viewers get every dialog; blocking ones are also
//! pushed through ntfy when `botler.toml` configures `[push]`, with hub-written text only (an
//! agent's question never leaves the host this way).

use botler_store::{Dialog, DialogKind, Priority};

use crate::hub::Hub;

#[derive(Clone, Debug, serde::Serialize, ts_rs::TS)]
pub struct DialogNotice {
    pub dialog_id: String,
    pub instance: String,
    pub priority: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("the ntfy topic secret {0} has no value; enter it in the web app")]
    MissingTopic(String),
    #[error("the ntfy topic may only contain letters, digits, `-` and `_`")]
    BadTopic,
    #[error("the ntfy URL {0:?} may not contain quotes, backslashes or line breaks")]
    BadUrl(String),
    #[error("running curl: {0}")]
    Io(#[from] std::io::Error),
    #[error("ntfy refused the push: {0}")]
    Refused(String),
}

pub async fn dialog_opened(hub: &Hub, d: &Dialog) {
    tracing::info!(dialog = %d.id, instance = %d.instance, priority = %d.priority, "dialog opened");
    let notice = crate::hub::Live::Dialog {
        notice: DialogNotice {
            dialog_id: d.id.clone(),
            instance: d.instance.clone(),
            priority: d.priority.to_string(),
        },
    };
    if hub.live.send(notice).is_err() {
        tracing::trace!("no live viewers");
    }
    if let Err(e) = crate::channels::dialog_opened(hub, d).await {
        tracing::error!(dialog = %d.id, error = %e, "could not tell the channel about the request");
    }
    if d.priority == Priority::Blocking
        && let Some(push) = hub.applied().await.defs.instance.push.clone()
    {
        let topic = match hub.store.secret(&hub.secret_key, &push.topic_secret).await {
            Ok(Some(t)) => t,
            Ok(None) => {
                tracing::error!(error = %PushError::MissingTopic(push.topic_secret), "push skipped");
                return;
            }
            Err(e) => {
                tracing::error!(error = %e, "push skipped: cannot read the topic secret");
                return;
            }
        };
        let text = push_text(d.kind);
        // Sent in the background: the request is open whether or not the phone hears about it.
        tokio::spawn(async move {
            if let Err(e) = ntfy(&push.ntfy_url, &topic, text).await {
                tracing::error!(error = %e, "push to ntfy failed");
            }
        });
    }
}

/// What the push says: fixed text per kind, nothing an agent wrote.
fn push_text(kind: DialogKind) -> &'static str {
    match kind {
        DialogKind::Question => "A question is waiting for your answer.",
        DialogKind::Approval => "A change is waiting for your approval.",
        DialogKind::Credentials => "A request for secrets is waiting for you.",
        DialogKind::Access => "A request for access is waiting for you.",
        DialogKind::Visit => "A page is waiting for you to open it.",
    }
}

/// POST `text` to `<url>/<topic>` with curl. The topic (a secret) goes in on stdin, so it never
/// shows in the process list.
pub async fn ntfy(url: &str, topic: &str, text: &str) -> Result<(), PushError> {
    use tokio::io::AsyncWriteExt;
    if topic.is_empty() || !topic.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(PushError::BadTopic);
    }
    if url.contains(['"', '\\', '\n', '\r']) {
        return Err(PushError::BadUrl(url.into()));
    }
    let config = format!(
        "url = \"{}/{topic}\"\ndata = \"{text}\"\nheader = \"Title: botler\"\nheader = \"Priority: high\"\nheader = \"Tags: bell\"\n",
        url.trim_end_matches('/')
    );
    let mut child = tokio::process::Command::new("curl")
        .args(["--fail", "--silent", "--show-error", "--max-time", "15", "--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.as_bytes()).await?;
    }
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        return Err(PushError::Refused(String::from_utf8_lossy(&out.stderr).trim().to_string()));
    }
    Ok(())
}
