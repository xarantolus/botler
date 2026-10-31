//! Apps: web UIs that services serve (`[app]` in job.toml), opened inside the web app.
//!
//! Agent-authored UI never runs on the web app's origin, where it could use the user's session
//! or draw approval dialogs. The hub serves apps from its apps port (the web app's port + 1) at
//! `/apps/<name>/...`, proxying to the service's port, which is published on 127.0.0.1 only
//! (rootless Podman: the host cannot reach container addresses). The web app shows an app in an
//! iframe.
//!
//! Signing in: the web app asks for a ticket (`POST /api/v1/apps/<name>/ticket`, a signed name
//! and expiry valid for a minute) and opens `/apps/<name>/__botler/enter?ticket=...` on the apps
//! port. That sets a cookie for `/apps/<name>/` only, holding a session token for that app, and
//! redirects to the app. Requests without a valid cookie get a page saying to open the app from
//! botler. Apps share the apps origin, so they are separated from the web app, not from each
//! other; all of them come from the user's monorepo.

use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use sha2::Sha256;

use crate::hub::Hub;

/// The path a ticket is redeemed at, under the app's prefix.
const ENTER: &str = "__botler/enter";
const TICKET_SECONDS: i64 = 60;
const SESSION_HOURS: i64 = 12;

/// The host port a service's app is published on (127.0.0.1): fixed per hub and app, so it
/// survives restarts and two hubs on one host do not collide.
pub fn host_port(hub_id: &str, app: &str) -> u16 {
    // FNV-1a: stable across builds, unlike the std hasher.
    let mut h: u32 = 0x811c_9dc5;
    for b in format!("{hub_id}/{app}").bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    20_000 + u16::try_from(h % 20_000).unwrap_or(0)
}

/// The apps port: the web app's port + 1 (any free port when the web app's is, in tests).
pub fn port(hub: &Hub) -> u16 {
    match hub.config.listen.port() {
        0 => 0,
        p => p.wrapping_add(1),
    }
}

fn mac(hub: &Hub, msg: &str) -> String {
    let key = hub.secret_key.derive("apps");
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("HMAC accepts any key length");
    m.update(msg.as_bytes());
    URL_SAFE_NO_PAD.encode(m.finalize().into_bytes())
}

/// A signed `<kind>` token for `app`, valid for `ttl`.
fn sign(hub: &Hub, kind: &str, app: &str, ttl: Duration) -> String {
    let exp = (Utc::now() + ttl).timestamp();
    format!("{exp}.{}", mac(hub, &format!("{kind}/{app}/{exp}")))
}

fn verify(hub: &Hub, kind: &str, app: &str, token: &str) -> bool {
    let Some((exp, sig)) = token.split_once('.') else { return false };
    let Ok(e) = exp.parse::<i64>() else { return false };
    let want = mac(hub, &format!("{kind}/{app}/{e}"));
    e > Utc::now().timestamp()
        && want.len() == sig.len()
        && want.bytes().zip(sig.bytes()).fold(0, |a, (x, y)| a | (x ^ y)) == 0
}

/// A one-minute ticket that signs the user in to `app` on the apps port.
pub fn ticket(hub: &Hub, app: &str) -> String {
    sign(hub, "ticket", app, Duration::seconds(TICKET_SECONDS))
}

/// Where the web app sends the iframe to redeem a ticket (a path on the apps port).
pub fn enter_path(app: &str, ticket: &str) -> String {
    format!("/apps/{app}/{ENTER}?ticket={ticket}")
}

fn cookie_name(app: &str) -> String {
    format!("botler_app_{}", app.replace(|c: char| !c.is_ascii_alphanumeric(), "_"))
}

/// The apps listener's routes.
pub fn router(hub: Arc<Hub>) -> Router {
    Router::new()
        .route("/apps/{name}", any(root))
        .route("/apps/{name}/", any(root))
        .route("/apps/{name}/{*rest}", any(proxy))
        .fallback(|| async { (StatusCode::NOT_FOUND, "no such app") })
        .with_state(hub)
}

async fn root(state: State<Arc<Hub>>, Path(name): Path<String>, req: Request) -> Response {
    forward(state, name, String::new(), req).await
}

async fn proxy(state: State<Arc<Hub>>, Path((name, rest)): Path<(String, String)>, req: Request) -> Response {
    forward(state, name, rest, req).await
}

fn page(status: StatusCode, text: &str) -> Response {
    let html = format!(
        "<!doctype html><meta charset=utf-8><title>botler</title>\
         <body style=\"font-family:system-ui;padding:2rem;color:#555\"><p>{text}</p>"
    );
    (status, [(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

async fn forward(State(hub): State<Arc<Hub>>, name: String, rest: String, mut req: Request) -> Response {
    let applied = hub.applied().await;
    let Some(job) = applied.defs.jobs.get(&name).filter(|j| j.spec.app.is_some() && j.spec.is_service()) else {
        return page(StatusCode::NOT_FOUND, &format!("There is no app <code>{name}</code>."));
    };
    let app = job.spec.app.clone().unwrap_or_else(|| unreachable!("filtered above"));
    drop(applied);

    if rest == ENTER {
        let ticket = req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("ticket=")));
        if !ticket.is_some_and(|t| verify(&hub, "ticket", &name, t)) {
            return page(StatusCode::UNAUTHORIZED, "This link has expired. Open the app from botler again.");
        }
        let session = sign(&hub, "session", &name, Duration::hours(SESSION_HOURS));
        let cookie = format!(
            "{}={session}; Path=/apps/{name}/; HttpOnly; SameSite=Strict; Max-Age={}",
            cookie_name(&name),
            SESSION_HOURS * 3600
        );
        let to = format!("/apps/{name}{}", app.path.as_deref().unwrap_or("/"));
        let mut r = (StatusCode::SEE_OTHER, [(header::LOCATION, to)]).into_response();
        match HeaderValue::from_str(&cookie) {
            Ok(v) => drop(r.headers_mut().insert(header::SET_COOKIE, v)),
            Err(e) => return page(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
        }
        return r;
    }

    let cookie = cookie_name(&name);
    let session = crate::login::cookie_value(req.headers(), &cookie);
    if !session.is_some_and(|s| verify(&hub, "session", &name, &s)) {
        return page(StatusCode::UNAUTHORIZED, "Open this app from botler (Apps in the menu).");
    }

    // To the service: without botler's cookies or credentials, with the prefix it is served under.
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    let target = format!("http://127.0.0.1:{}/{rest}{query}", host_port(&hub.id, &name));
    let uri: Uri = match target.parse() {
        Ok(u) => u,
        Err(e) => return page(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    *req.uri_mut() = uri;
    let h = req.headers_mut();
    h.remove(header::COOKIE);
    h.remove(header::AUTHORIZATION);
    h.remove(crate::login::HEADER);
    h.insert(
        "x-forwarded-prefix",
        HeaderValue::from_str(&format!("/apps/{name}")).unwrap_or(HeaderValue::from_static("")),
    );
    static CLIENT: OnceLock<Client<HttpConnector, Body>> = OnceLock::new();
    let client = CLIENT.get_or_init(|| Client::builder(TokioExecutor::new()).build_http());
    match client.request(req).await {
        Ok(resp) => {
            let (mut parts, body) = resp.into_parts();
            // The app's own cookies stay under its prefix (and never replace botler's).
            let cookies: Vec<HeaderValue> = parts
                .headers
                .get_all(header::SET_COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok())
                .filter(|v| !v.trim_start().starts_with("botler_"))
                .filter_map(|v| HeaderValue::from_str(&scoped_cookie(v, &name)).ok())
                .collect();
            parts.headers.remove(header::SET_COOKIE);
            for c in cookies {
                parts.headers.append(header::SET_COOKIE, c);
            }
            Response::from_parts(parts, Body::new(body))
        }
        Err(e) => {
            tracing::warn!(app = %name, error = %e, "app not answering");
            page(
                StatusCode::BAD_GATEWAY,
                &format!("App <code>{name}</code> is not answering (is its service running?)."),
            )
        }
    }
}

/// A `Set-Cookie` from an app with its path moved under the app's prefix.
fn scoped_cookie(v: &str, app: &str) -> String {
    let mut parts: Vec<String> = v.split(';').map(|p| p.trim().to_owned()).collect();
    let prefix = format!("/apps/{app}");
    let mut had = false;
    for p in parts.iter_mut().skip(1) {
        if p.len() >= 5 && p[..5].eq_ignore_ascii_case("path=") {
            *p = format!("Path={prefix}{}", &p[5..]);
            had = true;
        }
    }
    if !had {
        parts.push(format!("Path={prefix}/"));
    }
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_and_ports() {
        assert_eq!(scoped_cookie("sid=1; HttpOnly", "home"), "sid=1; HttpOnly; Path=/apps/home/");
        assert_eq!(scoped_cookie("sid=1; path=/x", "home"), "sid=1; Path=/apps/home/x");
        assert_eq!(host_port("a", "home"), host_port("a", "home"));
        assert_ne!(host_port("a", "home"), host_port("b", "home"), "two hubs on one host");
        assert!((20_000..40_000).contains(&host_port("a", "home")));
    }
}
