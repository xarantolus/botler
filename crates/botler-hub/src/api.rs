//! The hub's HTTP surface: MCP for agents, the ops API for the SDK and the web app, user
//! endpoints, and git smart-HTTP for agent clones.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Extension, Json, Router};
use botler_core::event::kinds;
use botler_core::{Event, Provenance, Source};
use botler_store::{DialogStatus, EventQuery};
use chrono::Utc;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::ops::{self, Caller, OpError};

type AppState = Arc<Hub>;

/// Largest request body accepted (git pushes included).
const MAX_BODY: usize = 256 * 1024 * 1024;

pub fn router(hub: Arc<Hub>) -> Router {
    let mcp = {
        let hub = hub.clone();
        let mut config = StreamableHttpServerConfig::default();
        config.legacy_session_mode = false;
        config.json_response = true;
        config.allowed_hosts = vec![]; // Host checks are replaced by bearer-token authentication.
        StreamableHttpService::new(
            move || Ok(crate::mcp::BotlerMcp { hub: hub.clone() }),
            Arc::new(NeverSessionManager::default()),
            config,
        )
    };
    let authed = Router::new()
        .nest_service("/mcp", mcp)
        .route("/api/v1/ops/{name}", post(op))
        .route("/api/v1/status", get(status))
        .route("/api/v1/messages", post(user_message))
        .route("/api/v1/dialogs", get(dialogs))
        .route("/api/v1/dialogs/{id}/answer", post(answer_dialog))
        .route("/api/v1/dialogs/{id}/secrets", post(submit_secrets))
        .route("/api/v1/events", get(events))
        .route("/api/v1/events/{id}/trace", get(trace))
        .route("/api/v1/instances", get(instances))
        .route("/api/v1/sessions/{instance}", get(session))
        .route("/api/v1/chats", post(new_chat))
        .route("/api/v1/chats/{instance}/archive", post(archive_chat))
        .route("/api/v1/chats/{instance}/restore", post(restore_chat))
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/incidents", get(incidents))
        .route("/api/v1/secrets", get(secrets))
        .route("/api/v1/secrets/{name}", put(set_secret))
        .route("/api/v1/proposals", get(proposals))
        .route("/api/v1/revert", post(revert))
        .route("/api/v1/pause", post(pause))
        .route("/api/v1/resume", post(resume))
        .route("/api/v1/breakers/clear", post(clear_breaker))
        .route("/git/monorepo.git/{*rest}", any(git))
        .layer(middleware::from_fn_with_state(hub.clone(), authenticate));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // The web app is public static content; every API call it makes carries the user token.
        .route("/", get(web_index))
        .merge(authed)
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(hub)
}

/// Resolve the bearer token (or git's basic-auth password) to a [`Principal`].
async fn authenticate(State(hub): State<AppState>, mut req: Request, next: Next) -> Response {
    let token = bearer(req.headers());
    let Some(token) = token else {
        let mut r = (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
        // Lets git clients fall back to basic auth (token as password).
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"botler\""));
        return r;
    };
    match hub.tokens.verify(&token, Utc::now()) {
        Ok(p) => {
            req.extensions_mut().insert(p);
            next.run(req).await
        }
        Err(e) => (StatusCode::UNAUTHORIZED, e.to_string()).into_response(),
    }
}

fn bearer(h: &HeaderMap) -> Option<String> {
    use base64::Engine;
    let v = h.get(header::AUTHORIZATION)?.to_str().ok()?;
    if let Some(t) = v.strip_prefix("Bearer ") {
        return Some(t.trim().to_string());
    }
    let b = v.strip_prefix("Basic ")?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b.trim()).ok()?;
    let s = String::from_utf8(decoded).ok()?;
    s.split_once(':').map(|(_, pw)| pw.to_string())
}

/// Errors as JSON with a fitting status code.
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        let code = match &e {
            OpError::BadRequest(_) => StatusCode::BAD_REQUEST,
            OpError::Forbidden(_) => StatusCode::FORBIDDEN,
            OpError::Unknown(_) => StatusCode::NOT_FOUND,
            OpError::InvalidEvent(_) => StatusCode::UNPROCESSABLE_ENTITY,
            OpError::Hub(h) => return h.into(),
        };
        Self(code, e.to_string())
    }
}

impl From<&HubError> for ApiError {
    fn from(e: &HubError) -> Self {
        let code = match e {
            HubError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            HubError::Forbidden(_) => StatusCode::FORBIDDEN,
            HubError::Undefined { .. } | HubError::Store(botler_store::Error::NotFound { .. }) => StatusCode::NOT_FOUND,
            HubError::Store(
                botler_store::Error::DialogClosed { .. } | botler_store::Error::AnswerNotAllowed { .. },
            ) => StatusCode::CONFLICT,
            HubError::Paused => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if code == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %e, "request failed");
        }
        Self(code, e.to_string())
    }
}

impl From<HubError> for ApiError {
    fn from(e: HubError) -> Self {
        (&e).into()
    }
}

impl From<botler_store::Error> for ApiError {
    fn from(e: botler_store::Error) -> Self {
        HubError::from(e).into()
    }
}

type ApiResult<T = Json<Value>> = Result<T, ApiError>;

fn user_only(p: &Principal) -> Result<(), ApiError> {
    if *p == Principal::User { Ok(()) } else { Err(ApiError(StatusCode::FORBIDDEN, "user only".into())) }
}

async fn op(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    body: Option<Json<Value>>,
) -> ApiResult {
    let caller = Caller::resolve(&hub, p).await?;
    let input = body.map_or(Value::Null, |Json(v)| v);
    Ok(Json(ops::call(&hub, &caller, &name, input).await?))
}

async fn status(State(hub): State<AppState>) -> ApiResult {
    let applied = hub.applied().await;
    let info = hub.containers.info();
    Ok(Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": applied.commit,
        "paused": hub.paused().await?,
        "timezone": applied.defs.timezone.name(),
        "runtime": { "binary": info.binary, "version": info.version, "rootless": info.rootless },
        "open_dialogs": hub.store.open_dialogs().await?.len(),
        "open_incidents": hub.store.open_incidents().await?.len(),
        "breakers": hub.store.breakers().await?,
    })))
}

#[derive(Deserialize)]
struct UserMessage {
    #[serde(default = "main_instance")]
    to: String,
    text: String,
}

fn main_instance() -> String {
    botler_core::FRONT_DOOR.into()
}

async fn user_message(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(m): Json<UserMessage>,
) -> ApiResult {
    user_only(&p)?;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(m.to)
        .payload(json!({ "text": m.text }));
    let ev = hub.emit(ev).await?;
    Ok(Json(json!({ "event_id": ev.id })))
}

async fn dialogs(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    let applied = hub.applied().await;
    let open: Vec<Value> = hub
        .store
        .open_dialogs()
        .await?
        .into_iter()
        .map(|d| {
            let mut v = json!(d);
            if d.kind == botler_store::DialogKind::Credentials
                && let Some(req) =
                    d.spec.clone().and_then(|s| serde_json::from_value::<crate::secret_requests::SecretRequest>(s).ok())
            {
                v["form"] = json!(req.form(&applied.defs));
                v["remove"] = json!(req.remove);
                v["group"] = json!(req.group);
                v["new_group"] = json!(!applied.defs.secret_groups.contains_key(&req.group));
            }
            v
        })
        .collect();
    Ok(Json(json!({ "open": open, "recent": hub.store.recent_dialogs(50).await? })))
}

#[derive(Deserialize)]
struct Answer {
    answer: String,
}

async fn answer_dialog(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(a): Json<Answer>,
) -> ApiResult {
    user_only(&p)?;
    if hub.store.dialog(&id).await?.kind == botler_store::DialogKind::Credentials {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "credentials requests are answered with the secrets form".into(),
        ));
    }
    let d = hub.store.answer_dialog(&id, &a.answer).await?;
    debug_assert_eq!(d.status, DialogStatus::Answered);
    if d.kind == botler_store::DialogKind::Approval
        && let Some(pid) = &d.proposal_id
    {
        crate::transactions::on_approval(&hub, pid, a.answer == "approve").await?;
        return Ok(Json(json!({ "dialog": d })));
    }
    let ev = Event::new(kinds::DIALOG_ANSWERED, Source::User, Provenance::Trusted, Utc::now())
        .to(d.instance.clone())
        .payload(json!({ "dialog_id": d.id, "question": d.question, "answer": a.answer }));
    hub.emit(ev).await?;
    Ok(Json(json!({ "dialog": d })))
}

async fn submit_secrets(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(sub): Json<crate::secret_requests::Submission>,
) -> ApiResult {
    user_only(&p)?;
    use crate::secret_requests::RequestError as R;
    match crate::secret_requests::submit(&hub, &id, sub).await {
        Ok(v) => Ok(Json(v)),
        Err(R::Invalid(m)) => Err(ApiError(StatusCode::CONFLICT, m)),
        Err(R::MissingValue(n)) => Err(ApiError(StatusCode::BAD_REQUEST, format!("enter a value for {n}"))),
        Err(R::Toml(m)) => Err(ApiError(StatusCode::CONFLICT, format!("secrets.toml: {m}"))),
        Err(R::Hub(h)) => Err(h.into()),
    }
}

#[derive(Deserialize)]
struct EventsQuery {
    #[serde(rename = "type")]
    kind: Option<String>,
    correlation: Option<String>,
    limit: Option<u32>,
}

async fn events(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<EventsQuery>,
) -> ApiResult {
    user_only(&p)?;
    let q = EventQuery { kind: q.kind, correlation: q.correlation, since: None, limit: q.limit.unwrap_or(50) };
    Ok(Json(json!(hub.store.events(&q).await?)))
}

/// Everything an event caused: which chats it reached, runs it started, its thread.
async fn trace(State(hub): State<AppState>, Extension(p): Extension<Principal>, Path(id): Path<String>) -> ApiResult {
    user_only(&p)?;
    let event = hub.store.event(&id).await?;
    let mut reached = Vec::new();
    for d in hub.store.deliveries_for_event(&id).await? {
        let inst = hub.store.find_instance(&d.instance).await?;
        reached.push(json!({
            "instance": d.instance,
            "title": inst.as_ref().and_then(|i| i.title.clone()),
            "slug": inst.as_ref().and_then(|i| i.slug.clone()),
            // The session entry carrying this delivery id is where the web app scrolls to.
            "session_anchor": { "custom_type": "botler.delivered", "delivery_id": d.id },
            "delivery": d,
        }));
    }
    let thread =
        hub.store.events(&EventQuery { correlation: Some(id.clone()), limit: 50, ..Default::default() }).await?;
    Ok(Json(
        json!({ "event": event, "reached": reached, "runs": hub.store.runs_for_event(&id).await?, "thread": thread }),
    ))
}

async fn web_index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")],
        include_str!("../../../web/index.html"),
    )
}

async fn session(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
) -> ApiResult {
    user_only(&p)?;
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    let busy = hub.agents.get(&inst.name).await.is_some_and(|a| a.busy());
    let pending = hub.store.pending_deliveries(&inst.name).await?.len();
    let phase = hub.agents.phase(&inst.name).await;
    let items = crate::sessions::read(&hub, &inst.name).await?;
    Ok(Json(json!({ "instance": inst, "busy": busy, "pending": pending, "phase": phase, "items": items })))
}

#[derive(Deserialize)]
struct NewChat {
    text: String,
    /// Agent definition; defaults to `chat`.
    #[serde(default)]
    definition: Option<String>,
}

/// Start a new chat with the user's first message.
async fn new_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(c): Json<NewChat>,
) -> ApiResult {
    user_only(&p)?;
    let def = c.definition.unwrap_or_else(|| "chat".into());
    let name = crate::router::new_instance(&hub, &def, None, None).await?;
    let ev = Event::new(kinds::MESSAGE, Source::User, Provenance::Trusted, Utc::now())
        .to(name.clone())
        .payload(json!({ "text": c.text }));
    hub.emit(ev).await?;
    Ok(Json(json!({ "instance": name })))
}

#[derive(Deserialize, Default)]
struct ArchiveRequest {
    /// Archive without waiting for the agent's handover (e.g. it no longer answers).
    #[serde(default)]
    force: bool,
    #[serde(default)]
    reason: Option<String>,
}

async fn archive_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
    body: Option<Json<ArchiveRequest>>,
) -> ApiResult {
    user_only(&p)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    if inst.persistent
        && inst.definition == inst.name
        && hub.applied().await.defs.agents.get(&inst.name).is_some_and(|d| d.spec.always_on)
    {
        return Err(ApiError(StatusCode::CONFLICT, format!("{} is always on and cannot be archived", inst.name)));
    }
    if req.force {
        let report = json!({ "forced": true, "confirmed_by_agent": false, "reason": req.reason, "at": Utc::now() });
        hub.store.archive_instance(&inst.name, &report).await?;
        crate::agents::stop_after_reply(&hub, &inst.name).await;
        return Ok(Json(json!({ "archived": inst.name, "confirmed_by_agent": false })));
    }
    hub.store.request_archive(&inst.name).await?;
    let ev = Event::new(kinds::ARCHIVE_REQUESTED, Source::User, Provenance::Trusted, Utc::now())
        .to(inst.name.clone())
        .payload(json!({ "reason": req.reason }));
    hub.emit(ev).await?;
    Ok(Json(json!({ "requested": inst.name, "note": "The agent will hand over and then archive itself." })))
}

async fn restore_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
) -> ApiResult {
    user_only(&p)?;
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    hub.store.restore_instance(&inst.name).await?;
    Ok(Json(json!({ "restored": inst.name })))
}

async fn instances(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    Ok(Json(json!(hub.store.all_instances(200).await?)))
}

async fn runs(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    Ok(Json(json!(hub.store.recent_runs(100).await?)))
}

async fn incidents(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    Ok(Json(json!(hub.store.open_incidents().await?)))
}

/// Declared secret groups, each field with whether it has a value (never the values), and who
/// is granted the group.
async fn secrets(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    let applied = hub.applied().await;
    let set = hub.store.secret_names().await?;
    let mut grantees: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
    for a in applied.defs.agents.values() {
        for g in &a.spec.secrets {
            grantees.entry(g.as_str()).or_default().push(format!("agent {}", a.name));
        }
    }
    for j in applied.defs.jobs.values() {
        for g in &j.spec.secrets {
            grantees.entry(g.as_str()).or_default().push(format!("job {}", j.name));
        }
    }
    for (r, route) in &applied.defs.instance.models.routes {
        if let Some(sec) = route.api_key_secret.as_ref().and_then(|n| applied.defs.secrets.get(n)) {
            grantees.entry(sec.group.as_str()).or_default().push(format!("model route {r}"));
        }
    }
    let groups: Vec<Value> = applied
        .defs
        .secret_groups
        .values()
        .map(|g| {
            let fields: Vec<Value> = g
                .secrets
                .iter()
                .filter_map(|n| applied.defs.secrets.get(n))
                .map(|s| json!({ "name": s.name, "description": s.description, "file": s.file, "has_value": set.contains(&s.name) }))
                .collect();
            let complete = g.secrets.iter().all(|n| set.contains(n));
            json!({
                "name": g.name, "description": g.description, "class": g.class, "complete": complete,
                "fields": fields, "granted_to": grantees.get(g.name.as_str()).cloned().unwrap_or_default(),
            })
        })
        .collect();
    Ok(Json(json!(groups)))
}

#[derive(Deserialize)]
struct SecretValue {
    value: String,
}

async fn set_secret(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    Json(v): Json<SecretValue>,
) -> ApiResult {
    user_only(&p)?;
    if !hub.applied().await.defs.secrets.contains_key(&name) {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("secret {name} is not declared in secrets.toml")));
    }
    hub.store.set_secret(&hub.secret_key, &name, &v.value).await?;
    // Deliveries waiting on a missing secret can proceed now.
    hub.dispatch_notify.notify_one();
    Ok(Json(json!({ "ok": true })))
}

async fn proposals(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    Ok(Json(json!(hub.store.recent_proposals(50).await?)))
}

#[derive(Deserialize)]
struct RevertRequest {
    commit: String,
}

/// Revert a merged commit. The user is the authority, so this applies directly.
async fn revert(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(r): Json<RevertRequest>,
) -> ApiResult {
    user_only(&p)?;
    let head = crate::transactions::revert(&hub, &r.commit).await?;
    Ok(Json(json!({ "reverted": r.commit, "commit": head })))
}

async fn pause(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    crate::control::pause(&hub).await?;
    Ok(Json(json!({ "paused": true })))
}

async fn resume(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult {
    user_only(&p)?;
    crate::control::resume(&hub).await?;
    Ok(Json(json!({ "paused": false })))
}

#[derive(Deserialize)]
struct ClearBreaker {
    subject: String,
}

/// Resume what a breaker paused (a pair's held messages are delivered).
async fn clear_breaker(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(c): Json<ClearBreaker>,
) -> ApiResult {
    user_only(&p)?;
    let (cleared, released) = crate::loops::clear_breaker(&hub, &c.subject).await?;
    Ok(Json(json!({ "cleared": cleared, "released": released })))
}

/// git smart-HTTP through `git http-backend`. Agents authenticate with their token; the
/// pre-receive hook limits pushes to their own branch namespace via `REMOTE_USER`.
async fn git(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(rest): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let user = match &p {
        Principal::Agent { instance, wake } if hub.agents.is_current(instance, wake).await => instance.clone(),
        Principal::Agent { instance, .. } => {
            return Err(ApiError(StatusCode::FORBIDDEN, format!("token belongs to a stopped session of {instance}")));
        }
        _ => return Err(ApiError(StatusCode::FORBIDDEN, "git access is for agents".into())),
    };
    let method = req.method().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let content_type = req.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let encoding = req.headers().get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let body: Bytes = axum::body::to_bytes(req.into_body(), MAX_BODY)
        .await
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let body = if encoding == "gzip" { gunzip(&body).await? } else { body.to_vec() };

    let mut child = tokio::process::Command::new("git")
        .arg("http-backend")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_PROJECT_ROOT", &hub.paths.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", format!("/repo.git/{rest}"))
        .env("REQUEST_METHOD", method)
        .env("QUERY_STRING", query)
        .env("CONTENT_TYPE", content_type)
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", user)
        .env("REMOTE_ADDR", "127.0.0.1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("git http-backend: {e}")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "no stdin".into()))?;
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(&body).await?;
        stdin.shutdown().await
    });
    let out = child.wait_with_output().await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    match writer.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "writing to git http-backend failed"),
        Err(e) => tracing::warn!(error = %e, "git http-backend writer panicked"),
    }
    if !out.stderr.is_empty() {
        tracing::debug!(stderr = %String::from_utf8_lossy(&out.stderr), "git http-backend");
    }
    cgi_response(&out.stdout)
}

async fn gunzip(data: &[u8]) -> Result<Vec<u8>, ApiError> {
    let mut child = tokio::process::Command::new("gzip")
        .arg("-dc")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("gzip: {e}")))?;
    let mut stdin = child.stdin.take().ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "no stdin".into()))?;
    let data = data.to_vec();
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(&data).await?;
        stdin.shutdown().await
    });
    let out = child.wait_with_output().await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    writer
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, format!("gzip body: {e}")))?;
    if !out.status.success() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid gzip body".into()));
    }
    Ok(out.stdout)
}

/// Turn CGI output (headers, blank line, body) into a response.
fn cgi_response(raw: &[u8]) -> Result<Response, ApiError> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| raw.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2)))
        .ok_or_else(|| ApiError(StatusCode::BAD_GATEWAY, "malformed CGI response".into()))?;
    let head = String::from_utf8_lossy(&raw[..split.0]);
    let mut resp = Response::builder().status(StatusCode::OK);
    for line in head.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let (k, v) = (k.trim(), v.trim());
        if k.eq_ignore_ascii_case("status") {
            let code = v.split(' ').next().and_then(|c| c.parse::<u16>().ok()).unwrap_or(500);
            resp = resp.status(code);
        } else {
            resp = resp.header(k, v);
        }
    }
    resp.body(Body::from(raw[split.0 + split.1..].to_vec()))
        .map_err(|e| ApiError(StatusCode::BAD_GATEWAY, e.to_string()))
}
