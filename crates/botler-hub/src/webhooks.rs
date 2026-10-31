//! `POST /hooks/<name>`: outside services calling a webhook trigger.
//!
//! Outside user authentication: a call is verified with the trigger's `webhook_secret`, either as
//! `Authorization: Bearer <secret>` or as an HMAC-SHA256 of the body (`X-Hub-Signature-256:
//! sha256=<hex>`, GitHub's form, or `X-Signature-256`). Bodies are limited to [`MAX_BODY`] and
//! calls rate limited per webhook. A verified call becomes `webhook.received` with external
//! provenance, dispatched to the trigger's target.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use botler_core::defs::{Mode, TriggerKind};
use botler_core::payloads::{BuiltinPayload, WebhookReceivedPayload};
use botler_core::{Event, Provenance, Source};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::hub::{Hub, HubError};

/// Largest webhook body accepted.
pub const MAX_BODY: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    #[error("no webhook {0}")]
    Unknown(String),
    #[error("webhook {0} has no secret value yet; enter it in the web app")]
    NoSecret(String),
    #[error("missing or wrong signature or token")]
    Unauthorized,
    #[error("botler is paused")]
    Paused,
    #[error(transparent)]
    Hub(#[from] HubError),
}

impl IntoResponse for WebhookError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Unknown(_) => StatusCode::NOT_FOUND,
            Self::NoSecret(_) | Self::Paused => StatusCode::SERVICE_UNAVAILABLE,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Hub(HubError::RateLimited { .. }) => StatusCode::TOO_MANY_REQUESTS,
            Self::Hub(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self, "webhook failed");
        }
        (status, self.to_string()).into_response()
    }
}

/// Whether `headers` prove the caller knows `secret` for `body`.
pub fn verified(secret: &[u8], headers: &HeaderMap, body: &[u8]) -> bool {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(token) = get(header::AUTHORIZATION.as_str()).and_then(|v| v.strip_prefix("Bearer ")) {
        return constant_eq(token.trim().as_bytes(), secret);
    }
    let Some(sig) = get("x-hub-signature-256").or_else(|| get("x-signature-256")) else { return false };
    let Some(Ok(expected)) = sig.strip_prefix("sha256=").map(hex::decode) else { return false };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret) else { return false };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn receive(
    State(hub): State<Arc<Hub>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, axum::Json<serde_json::Value>), WebhookError> {
    let applied = hub.applied().await;
    let trigger = applied
        .defs
        .triggers
        .values()
        .find(|t| matches!(&t.kind, TriggerKind::Webhook { name: n } if *n == name))
        .ok_or_else(|| WebhookError::Unknown(name.clone()))?;
    let secret_name = trigger.spec.webhook_secret.as_deref().ok_or_else(|| WebhookError::NoSecret(name.clone()))?;
    let secret = hub
        .store
        .secret_bytes(&hub.secret_key, secret_name)
        .await
        .map_err(HubError::from)?
        .ok_or_else(|| WebhookError::NoSecret(name.clone()))?;
    if !verified(&secret, &headers, &body) {
        tracing::warn!(webhook = %name, "rejected an unverified webhook call");
        return Err(WebhookError::Unauthorized);
    }
    hub.rate_limit(&format!("webhook:{name}"), "webhook", botler_core::limits::default_limit("webhook")).await?;
    if hub.paused().await? {
        return Err(WebhookError::Paused);
    }
    let content_type = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(String::from);
    let body = serde_json::from_slice(&body)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&body).into_owned()));
    let payload = BuiltinPayload::WebhookReceived(WebhookReceivedPayload { name: name.clone(), content_type, body });
    let ev = Event::builtin(payload, Source::Webhook(name.clone()), Provenance::External, Utc::now())
        .map_err(|e| HubError::Io(std::io::Error::other(e)))?;
    let id = ev.id;
    if trigger.spec.mode == Mode::Shadow {
        tracing::info!(webhook = %name, "shadow webhook trigger; journaled, not executed");
        hub.emit(ev).await?;
    } else {
        crate::router::fire(&hub, &trigger.target, trigger.spec.prompt.as_deref(), Some(&trigger.id), ev).await?;
    }
    Ok((StatusCode::ACCEPTED, axum::Json(serde_json::json!({ "event_id": id }))))
}
