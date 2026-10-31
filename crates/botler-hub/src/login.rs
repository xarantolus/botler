//! Signing in the web app: devices with an HttpOnly session cookie.
//!
//! A new device signs in with a one-time code from a device that is already signed in (or from
//! `botler login-code` on the hub), or with a user token as before. It gets the cookie
//! `botler_session=<device id>.<secret>`, `HttpOnly` and `SameSite=Strict`, so page scripts never
//! see it. Only the secret's hash is stored, and a device can be signed out from any other.
//!
//! Requests carrying only the cookie must also send the `x-botler` header. A browser adds no
//! custom header to a cross-origin request without a CORS preflight, which the hub never
//! allows, so a page on another origin cannot act with the cookie. That matters for apps
//! (agent-authored UIs): they are served from another port of the same host, which browsers
//! count as the same *site* and so send `SameSite=Strict` cookies to.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use botler_store::Device;
use chrono::{DateTime, Duration, Utc};
use rand::Rng;
use sha2::{Digest, Sha256};

use crate::auth::Principal;
use crate::hub::{Hub, Result};

pub const COOKIE: &str = "botler_session";
/// The header cookie-authenticated requests must carry (any value).
pub const HEADER: &str = "x-botler";
/// How long a sign-in code works.
pub const CODE_MINUTES: i64 = 10;
/// How long a device stays signed in without being revoked.
pub const COOKIE_DAYS: i64 = 400;

/// A one-time code: `len` characters without look-alikes (no 0/O, 1/I/L).
pub fn random_code(len: usize) -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    let mut rng = rand::rng();
    (0..len).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// A new sign-in code (8 characters: 31^8 ≈ 10^12, never guessed in its 10 minutes).
pub async fn create_code(hub: &Hub) -> Result<(String, DateTime<Utc>)> {
    let code = random_code(8);
    let expires = Utc::now() + Duration::minutes(CODE_MINUTES);
    hub.store.create_login_code(&hash(&code), expires).await?;
    Ok((code, expires))
}

/// What a device signs in with.
#[derive(Debug, serde::Deserialize, ts_rs::TS)]
#[ts(optional_fields)]
pub struct SignIn {
    /// A one-time code, or a user token from `botler token`.
    pub secret: String,
    /// What to call the device, e.g. "Phone".
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("that code or token is not valid (codes work once, for {CODE_MINUTES} minutes)")]
    Invalid,
    #[error(transparent)]
    Hub(#[from] crate::hub::HubError),
}

/// Sign a device in: the device and its cookie value.
pub async fn sign_in(hub: &Hub, s: &SignIn) -> std::result::Result<(Device, String), SignInError> {
    let secret = s.secret.trim();
    let code = secret.to_ascii_uppercase().replace(['-', ' '], "");
    let valid = if secret.contains('.') {
        hub.tokens.verify(secret, Utc::now()).is_ok_and(|p| p == Principal::User)
    } else {
        hub.store.use_login_code(&hash(&code), Utc::now()).await.map_err(crate::hub::HubError::from)?
    };
    if !valid {
        return Err(SignInError::Invalid);
    }
    let session: String = URL_SAFE_NO_PAD.encode(rand::rng().random::<[u8; 32]>());
    let name = s.name.as_deref().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("Browser");
    let name: String = name.chars().take(60).collect();
    let device = hub.store.create_device(&name, &hash(&session)).await.map_err(crate::hub::HubError::from)?;
    tracing::info!(device = %device.id, name = %device.name, "device signed in");
    let cookie = format!("{}.{session}", device.id);
    Ok((device, cookie))
}

/// The signed-in device a cookie value belongs to.
pub async fn device_of(hub: &Hub, cookie: &str) -> Result<Option<Device>> {
    let Some((id, secret)) = cookie.split_once('.') else { return Ok(None) };
    let Some((device, stored)) = hub.store.device(id).await? else { return Ok(None) };
    if device.revoked_at.is_some() || !constant_eq(stored.as_bytes(), hash(secret).as_bytes()) {
        return Ok(None);
    }
    let now = Utc::now();
    if now - device.last_seen_at > Duration::minutes(5) {
        hub.store.touch_device(&device.id, now).await?;
    }
    Ok(Some(device))
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The session cookie in a `Cookie` header.
pub fn cookie_in(headers: &axum::http::HeaderMap) -> Option<String> {
    cookie_value(headers, COOKIE)
}

/// The value of cookie `name` in the request's `Cookie` headers.
pub fn cookie_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| c.trim().strip_prefix(&format!("{name}=")).map(str::to_owned))
}

/// `Set-Cookie` for a signed-in device (`secure` when the page is served over HTTPS).
pub fn set_cookie(value: &str, secure: bool) -> String {
    let max_age = COOKIE_DAYS * 24 * 3600;
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}")
}

/// `Set-Cookie` that removes it.
pub fn clear_cookie() -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// Whether the browser reached the hub over HTTPS (directly or through a proxy like
/// `tailscale serve`).
pub fn is_https(headers: &axum::http::HeaderMap) -> bool {
    headers.get("x-forwarded-proto").and_then(|v| v.to_str().ok()) == Some("https")
}
