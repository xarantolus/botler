//! The hub's HTTP surface: MCP for agents, the ops API for the SDK and the web app, user
//! endpoints, and git smart-HTTP for agent clones.

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post, put};
use axum::{Extension, Json, Router};
use botler_core::payloads::{BuiltinPayload, MessagePayload, SecretsOutcome, SendMode};
use botler_core::{Event, Source};
use botler_store::{DialogStatus, EventQuery, Incident, Instance, Proposal, Run};
use chrono::Utc;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::Value;
use tower_http::compression::CompressionLayer;

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::ops::{self, Caller, OpError};
use crate::wire::{
    ApiErrorBody, AppTicket, AppView, ArchiveOutcome, BreakerCleared, BudgetView, Cancelled, ChatCreated, ChatRestored,
    DeviceRevoked, DeviceView, DialogAnswer, DialogView, Dialogs, EventPosted, HubStatus, LoginCode, PauseState,
    ProposalView, QueuedMessage, ReachedChat, Reverted, RuntimeInfo, Saved, SecretFieldView, SecretGroupView,
    SessionAnchor, SessionView, SignedIn, Trace, Usage,
};

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
        .route("/api/v1/usage", get(usage))
        .route("/api/v1/costs", get(costs))
        .route("/api/v1/openrouter/key", get(openrouter_key))
        .route("/api/v1/messages", post(user_message))
        .route("/api/v1/uploads", put(upload))
        .route("/api/v1/uploads/{id}/{name}", get(get_upload))
        .route("/api/v1/sessions/{instance}/images/{entry}/{index}", get(session_image))
        .route("/api/v1/messages/{id}", delete(cancel_message))
        .route("/api/v1/dialogs", get(dialogs))
        .route("/api/v1/dialogs/{id}/answer", post(answer_dialog))
        .route("/api/v1/dialogs/{id}/secrets", post(submit_secrets))
        .route("/api/v1/events", get(events))
        .route("/api/v1/events/{id}/trace", get(trace))
        .route("/api/v1/search", get(search))
        .route("/api/v1/instances", get(instances))
        .route("/api/v1/sessions/{instance}", get(session))
        .route("/api/v1/chats", post(new_chat))
        .route("/api/v1/chats/{instance}/archive", post(archive_chat))
        .route("/api/v1/chats/{instance}/restore", post(restore_chat))
        .route("/api/v1/runs", get(runs))
        .route("/api/v1/system", get(system))
        .route("/api/v1/incidents", get(incidents))
        .route("/api/v1/secrets", get(secrets))
        .route("/api/v1/secrets/{name}", put(set_secret))
        .route("/api/v1/proposals", get(proposals))
        .route("/api/v1/proposals/{id}", get(proposal))
        .route("/api/v1/proposals/{id}/diff", get(proposal_diff))
        .route("/api/v1/proposals/{id}/decision", post(decide_proposal))
        .route("/api/v1/revert", post(revert))
        .route("/api/v1/pause", post(pause))
        .route("/api/v1/resume", post(resume))
        .route("/api/v1/breakers/clear", post(clear_breaker))
        .route("/api/v1/settings", get(settings).put(save_settings))
        .route("/api/v1/models", get(models))
        .route("/api/v1/login/codes", post(login_code))
        .route("/api/v1/devices", get(devices))
        .route("/api/v1/devices/{id}", delete(revoke_device))
        .route("/api/v1/logout", post(logout))
        .route(crate::gateway::PATH, post(chat_completions))
        .route("/api/v1/apps", get(apps))
        .route("/api/v1/apps/{name}/ticket", post(app_ticket))
        .route("/api/v1/channels", get(channels))
        .route("/api/v1/channels/codes", post(channel_code))
        .route("/api/v1/channels/bindings/{id}", delete(revoke_binding))
        // JSON shrinks about tenfold; git and the live socket stay as they are.
        .route_layer(CompressionLayer::new())
        .route("/git/monorepo.git/{*rest}", any(git))
        .layer(middleware::from_fn_with_state(hub.clone(), authenticate));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // Signing in is how a device gets its cookie.
        .route("/api/v1/login", post(login))
        // The live stream authenticates itself (browsers cannot set headers on WebSockets).
        .route("/api/v1/live", get(crate::live::socket))
        // Webhooks verify callers with their trigger's secret, never the user's token.
        .route(
            "/hooks/{name}",
            post(crate::webhooks::receive).layer(axum::extract::DefaultBodyLimit::max(crate::webhooks::MAX_BODY)),
        )
        .merge(authed)
        // The web app is public static content; every API call it makes carries the user token.
        .route("/legacy", get(crate::web::legacy).layer(CompressionLayer::new()))
        .fallback_service(any(crate::web::app).layer(CompressionLayer::new()))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
        .with_state(hub)
}

/// Resolve the bearer token (or git's basic-auth password) to a [`Principal`].
/// The signed-in device a cookie-authenticated request comes from.
#[derive(Clone, Debug)]
pub struct CurrentDevice(pub String);

async fn authenticate(State(hub): State<AppState>, mut req: Request, next: Next) -> Response {
    let token = bearer(req.headers());
    if token.is_none()
        && let Some(cookie) = crate::login::cookie_in(req.headers())
    {
        if !req.headers().contains_key(crate::login::HEADER) {
            let why =
                format!("a request signed in with the session cookie must send the `{}` header", crate::login::HEADER);
            return (StatusCode::FORBIDDEN, why).into_response();
        }
        return match crate::login::device_of(&hub, &cookie).await {
            Ok(Some(d)) => {
                req.extensions_mut().insert(Principal::User);
                req.extensions_mut().insert(CurrentDevice(d.id));
                next.run(req).await
            }
            Ok(None) => (StatusCode::UNAUTHORIZED, "this device was signed out; sign in again").into_response(),
            Err(e) => ApiError::from(&e).into_response(),
        };
    }
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
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.1, self.0)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ApiErrorBody { error: self.1, conflict: None })).into_response()
    }
}

impl From<OpError> for ApiError {
    fn from(e: OpError) -> Self {
        let code = match &e {
            OpError::BadRequest(_) => StatusCode::BAD_REQUEST,
            OpError::Forbidden(_) => StatusCode::FORBIDDEN,
            OpError::Unknown(_) => StatusCode::NOT_FOUND,
            OpError::Conflict(_) => StatusCode::CONFLICT,
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
            HubError::DefinitionGone { .. } => StatusCode::CONFLICT,
            HubError::Memory(m) => {
                use crate::memory::MemoryError as M;
                match m {
                    M::InvalidPath { .. } | M::TooLarge { .. } | M::Invalid(_) => StatusCode::BAD_REQUEST,
                    M::Forbidden { .. } => StatusCode::FORBIDDEN,
                    M::NotFound(_) => StatusCode::NOT_FOUND,
                    M::Conflict(_) => StatusCode::CONFLICT,
                    M::Git { .. } | M::Malformed(_) | M::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
                }
            }
            HubError::Upload(u) => upload_status(u),
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
) -> ApiResult<Response> {
    let caller = Caller::resolve(&hub, p).await?;
    let input = body.map_or(Value::Null, |Json(v)| v);
    match ops::call(&hub, &caller, &name, input).await {
        Ok(v) => Ok(Json(v).into_response()),
        // A memory conflict carries the current version, so the client can merge.
        Err(OpError::Hub(HubError::Memory(crate::memory::MemoryError::Conflict(c)))) => {
            let body = ApiErrorBody { error: c.to_string(), conflict: Some(*c) };
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
        Err(e) => Err(e.into()),
    }
}

async fn status(State(hub): State<AppState>) -> ApiResult<Json<HubStatus>> {
    Ok(Json(status_view(&hub).await?))
}

/// The hub at a glance (`GET /status`; the live stream pushes it when it changes).
pub async fn status_view(hub: &Hub) -> Result<HubStatus, ApiError> {
    let applied = hub.applied().await;
    let info = hub.containers.info();
    let services = crate::services::status(hub).await?;
    Ok(HubStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        commit: applied.commit.clone(),
        paused: hub.paused().await?,
        front_door: botler_core::FRONT_DOOR.into(),
        timezone: applied.defs.timezone.name().into(),
        runtime: RuntimeInfo { binary: info.binary.clone(), version: info.version.clone(), rootless: info.rootless },
        definition_problems: applied.problems.clone(),
        skipped_definitions: applied.skipped.clone(),
        open_dialogs: hub.store.open_dialogs().await?.len(),
        open_incidents: hub.store.open_incidents().await?.len(),
        breakers: hub.store.breakers().await?,
        attention: crate::system::attention(hub, &services).await?,
        services,
        jobs: applied.defs.jobs.keys().cloned().collect(),
        agents: applied.defs.agents.keys().cloned().collect(),
        secret_groups: applied.defs.secret_groups.keys().cloned().collect(),
    })
}

/// Model spend per day, chat and model over a span of days (`GET /costs`).
async fn costs(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<crate::costs::CostsQuery>,
) -> ApiResult<Json<crate::costs::Costs>> {
    user_only(&p)?;
    if let Some(problem) = q.problem() {
        return Err(ApiError(StatusCode::BAD_REQUEST, problem));
    }
    Ok(Json(crate::costs::costs(&hub, &q).await?))
}

/// What OpenRouter counts for the model key (`null` without one).
async fn openrouter_key(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<Option<crate::gateway::ProviderKey>>> {
    user_only(&p)?;
    crate::gateway::openrouter_key(&hub).await.map(Json).map_err(|e| match e {
        crate::gateway::GatewayError::Hub(e) => e.into(),
        e => ApiError(StatusCode::BAD_GATEWAY, e.to_string()),
    })
}

/// Model spend today (in the hub's timezone) and all time, per chat.
async fn usage(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Usage>> {
    user_only(&p)?;
    Ok(Json(usage_view(&hub).await?))
}

/// Today's spend (`GET /usage`; pushed when it changes).
pub async fn usage_view(hub: &Hub) -> Result<Usage, ApiError> {
    let applied = hub.applied().await;
    let tz = applied.defs.timezone;
    let now = Utc::now();
    let day_start = now
        .with_timezone(&tz)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(tz).earliest())
        .map_or(now - chrono::Duration::hours(24), |t| t.with_timezone(&Utc));
    let budget = &applied.defs.instance.defaults.budget;
    Ok(Usage {
        day_start,
        timezone: tz.name().into(),
        today: hub.store.usage_since(None, day_start).await?,
        today_by_instance: hub.store.usage_by_instance(day_start).await?,
        last_day_by_instance: hub.store.usage_by_instance(now - chrono::Duration::hours(24)).await?,
        activity_by_instance: crate::activity::by_instance(hub, now).await?,
        total_by_instance: hub.store.usage_by_instance(chrono::DateTime::<Utc>::UNIX_EPOCH).await?,
        default_budget: BudgetView { usd_per_day: budget.usd_per_day, tokens_per_day: budget.tokens_per_day },
    })
}

#[derive(Deserialize, ts_rs::TS)]
pub struct UserMessage {
    /// The chat; defaults to the front door.
    #[serde(default = "main_instance")]
    #[ts(optional, as = "Option<String>")]
    to: String,
    text: String,
    /// While the agent works: wait for its run to end (`queue`, the default) or reach it once
    /// its running tool calls are done (`steer`).
    #[serde(default)]
    #[ts(optional, as = "Option<SendMode>")]
    mode: SendMode,
    /// Uploads to attach (`file` from `PUT /api/v1/uploads`); the chat reads them at
    /// `/inbox/files/<file>`.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    files: Vec<String>,
}

#[derive(Deserialize, ts_rs::TS)]
pub struct UploadQuery {
    /// The file's name, e.g. `photo.jpg`.
    name: String,
}

fn upload_status(e: &crate::uploads::UploadError) -> StatusCode {
    use crate::uploads::UploadError as U;
    match e {
        U::BadName(_) | U::Outside(_) | U::NotAFile(_) | U::TooLarge { .. } => StatusCode::BAD_REQUEST,
        U::Missing(_) | U::NotFound(_) => StatusCode::NOT_FOUND,
        U::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

impl From<crate::uploads::UploadError> for ApiError {
    fn from(e: crate::uploads::UploadError) -> Self {
        HubError::from(e).into()
    }
}

/// An upload, back to the user (thumbnails, players, downloads): images, video, audio, PDFs and
/// plain text inline, anything else as an opaque attachment (`uploads::media_type`).
async fn get_upload(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((id, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    user_only(&p)?;
    let (bytes, content_type, inline) = crate::uploads::read(&hub, &format!("{id}/{name}")).await?;
    let name: String = name.chars().filter(|c| c.is_ascii() && *c != '"' && *c != '\\').collect();
    let disposition = format!("{}; filename=\"{name}\"", if inline { "inline" } else { "attachment" });
    file_response(bytes, content_type, &disposition, "private, max-age=86400")
}

/// Stored bytes for the user: never sniffed, and sandboxed should a browser render one directly.
fn file_response(
    bytes: Vec<u8>,
    content_type: &'static str,
    disposition: &str,
    cache: &'static str,
) -> ApiResult<Response> {
    let disposition =
        HeaderValue::from_str(disposition).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    let headers = [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (header::CONTENT_DISPOSITION, disposition),
        (header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
        (header::CONTENT_SECURITY_POLICY, HeaderValue::from_static("sandbox")),
        (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
    ];
    Ok((headers, bytes).into_response())
}

/// An image block of a session entry (a screenshot a tool returned), by its place in the entry.
async fn session_image(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path((instance, entry, index)): Path<(String, String, usize)>,
    Query(q): Query<SessionQuery>,
) -> ApiResult<Response> {
    user_only(&p)?;
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    let (bytes, mime) = crate::sessions::image(&hub, &inst.name, q.file.as_deref(), &entry, index).await?;
    // Session entries never change once written.
    file_response(bytes, mime, "inline", "private, max-age=31536000, immutable")
}

/// Store the raw request body as a new upload.
async fn upload(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<UploadQuery>,
    body: Bytes,
) -> ApiResult<Json<crate::uploads::Uploaded>> {
    user_only(&p)?;
    Ok(Json(crate::uploads::save(&hub, &q.name, &body).await?))
}

fn main_instance() -> String {
    botler_core::FRONT_DOOR.into()
}

async fn user_message(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(m): Json<UserMessage>,
) -> ApiResult<Json<EventPosted>> {
    user_only(&p)?;
    let files = crate::uploads::attach(&hub, &m.to, &m.files).await?;
    let payload = MessagePayload { text: m.text, files, mode: Some(m.mode), via: None };
    let ev = user_event(BuiltinPayload::Message(payload))?.to(m.to);
    let ev = hub.emit(ev).await?;
    Ok(Json(EventPosted { event_id: ev.id }))
}

/// Take back a message of the user that is not delivered yet (queued while the agent works).
async fn cancel_message(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(delivery): Path<String>,
) -> ApiResult<Json<Cancelled>> {
    user_only(&p)?;
    let d = hub.store.delivery(&delivery).await?;
    if hub.store.event(&d.event_id).await?.source != Source::User {
        return Err(ApiError(StatusCode::FORBIDDEN, "only your own messages can be taken back".into()));
    }
    if !hub.store.cancel_pending(&delivery, "taken back by the user").await? {
        return Err(ApiError(StatusCode::CONFLICT, "the message was already delivered".into()));
    }
    hub.dispatch_notify.notify_one();
    Ok(Json(Cancelled { delivery }))
}

/// The user's messages not in the session (`items`) yet: those handed to pi as steers in its
/// current run (it adds them once the running tool calls are done), then those the hub holds.
async fn queued(
    hub: &Hub,
    instance: &str,
    in_session: &std::collections::HashSet<String>,
) -> Result<Vec<QueuedMessage>, ApiError> {
    let steered = match hub.agents.get(instance).await {
        Some(a) if a.busy() => a.steered(),
        _ => Vec::new(),
    };
    let mut deliveries = Vec::new();
    for id in steered.iter().filter(|id| !in_session.contains(id.as_str())) {
        deliveries.push((hub.store.delivery(id).await?, true));
    }
    deliveries.extend(hub.store.pending_deliveries(instance).await?.into_iter().map(|d| (d, false)));
    let mut out = Vec::new();
    for (d, handed_over) in deliveries {
        let ev = hub.store.event(&d.event_id).await?;
        if ev.source != Source::User || ev.kind != botler_core::event::kinds::MESSAGE {
            continue;
        }
        let m: MessagePayload = serde_json::from_value(ev.payload.clone()).map_err(|e| {
            ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("message {} is unreadable: {e}", ev.id))
        })?;
        out.push(QueuedMessage {
            delivery: d.id,
            text: m.text,
            at: d.created_at,
            mode: m.mode.unwrap_or(SendMode::Steer),
            handed_over,
        });
    }
    Ok(out)
}

/// An event from the user.
fn user_event(payload: BuiltinPayload) -> Result<Event, ApiError> {
    Ok(crate::hub::builtin(payload, Source::User)?)
}

async fn dialogs(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Dialogs>> {
    user_only(&p)?;
    Ok(Json(dialogs_view(&hub).await?))
}

/// Open and recent requests (`GET /dialogs`; pushed when they change).
pub async fn dialogs_view(hub: &Hub) -> Result<Dialogs, ApiError> {
    let applied = hub.applied().await;
    let mut open = Vec::new();
    for d in hub.store.open_dialogs().await? {
        let mut view = DialogView { dialog: d, form: None, remove: None, group: None, new_group: None, visit: None };
        if view.dialog.kind == botler_store::DialogKind::Visit {
            let spec = view.dialog.spec.clone().unwrap_or_default();
            view.visit = Some(serde_json::from_value(spec).map_err(|e| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("visit request {} is unreadable: {e}", view.dialog.id),
                )
            })?);
        }
        if view.dialog.kind == botler_store::DialogKind::Credentials {
            let spec = view.dialog.spec.clone().unwrap_or_default();
            let req: crate::secret_requests::SecretRequest = serde_json::from_value(spec).map_err(|e| {
                ApiError(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("credentials request {} is unreadable: {e}", view.dialog.id),
                )
            })?;
            view.form = Some(req.form(&applied.defs));
            view.new_group = Some(!applied.defs.secret_groups.contains_key(&req.group));
            view.remove = Some(req.remove);
            view.group = Some(req.group);
        }
        open.push(view);
    }
    Ok(Dialogs { open, recent: hub.store.recent_dialogs(50).await? })
}

#[derive(Deserialize, ts_rs::TS)]
pub struct Answer {
    answer: String,
    /// Approvals: why the user rejects (required) or a comment on approving. The agent reads it.
    #[serde(default)]
    #[ts(optional)]
    reason: Option<String>,
    /// Questions with a form: the field values (file fields hold uploads, `<id>/<name>`).
    #[serde(default)]
    #[ts(optional, type = "Record<string, unknown>")]
    values: Option<serde_json::Value>,
}

/// A question's form, if it has one.
fn form_of(d: &botler_store::Dialog) -> Result<Option<serde_json::Value>, ApiError> {
    if d.kind != botler_store::DialogKind::Question {
        return Ok(None);
    }
    let Some(spec) = d.spec.clone() else { return Ok(None) };
    let q: crate::ops::QuestionSpec = serde_json::from_value(spec).map_err(|e| {
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("question {} has an unreadable form: {e}", d.id))
    })?;
    Ok(Some(q.form))
}

async fn answer_dialog(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(a): Json<Answer>,
) -> ApiResult<Json<DialogAnswer>> {
    user_only(&p)?;
    match hub.store.dialog(&id).await?.kind {
        botler_store::DialogKind::Credentials => {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "credentials requests are answered with the secrets form".into(),
            ));
        }
        botler_store::DialogKind::Access => {
            return Ok(Json(DialogAnswer { dialog: crate::grants::answer(&hub, &id, &a.answer).await? }));
        }
        _ => {}
    }
    let d = hub.store.dialog(&id).await?;
    if d.kind == botler_store::DialogKind::Approval
        && d.status == DialogStatus::Open
        && let Some(pid) = &d.proposal_id
    {
        // Answers the dialog too.
        decide(&hub, pid, a.answer == "approve", a.reason.as_deref()).await?;
        return Ok(Json(DialogAnswer { dialog: hub.store.dialog(&id).await? }));
    }
    let (answer, values) = match form_of(&d)? {
        Some(form) => {
            let bad = |m: String| ApiError(StatusCode::BAD_REQUEST, m);
            let values = a.values.ok_or_else(|| bad("this question is a form; send its `values`".into()))?;
            crate::forms::check_values(&form, &values).map_err(bad)?;
            if d.status != DialogStatus::Open {
                // Let the store say why (answered, withdrawn, expired).
                hub.store.answer_dialog(&id, "").await?;
            }
            let uploads = crate::forms::files(&form, &values);
            let paths = crate::uploads::attach(&hub, &d.instance, &uploads).await.map_err(HubError::from)?;
            let map: Vec<_> = uploads.into_iter().zip(paths).collect();
            (crate::forms::summary(&form, &values), Some(crate::forms::with_paths(&values, &map)))
        }
        None => (a.answer, None),
    };
    let d = crate::ops::answer_question(&hub, &id, answer, values).await?;
    Ok(Json(DialogAnswer { dialog: d }))
}

async fn submit_secrets(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(sub): Json<crate::secret_requests::Submission>,
) -> ApiResult<Json<SecretsOutcome>> {
    user_only(&p)?;
    use crate::secret_requests::RequestError as R;
    match crate::secret_requests::submit(&hub, &id, sub).await {
        Ok(v) => Ok(Json(v)),
        Err(R::Invalid(m)) => Err(ApiError(StatusCode::CONFLICT, m)),
        Err(R::Input(e)) => Err(ApiError(StatusCode::BAD_REQUEST, e.to_string())),
        Err(R::MissingValue(n)) => Err(ApiError(StatusCode::BAD_REQUEST, format!("enter a value for {n}"))),
        Err(R::Toml(m)) => Err(ApiError(StatusCode::CONFLICT, format!("secrets.toml: {m}"))),
        Err(R::Hub(h)) => Err(h.into()),
    }
}

#[derive(Deserialize, ts_rs::TS)]
pub struct EventsQuery {
    #[serde(rename = "type")]
    #[ts(optional)]
    kind: Option<String>,
    #[ts(optional)]
    correlation: Option<String>,
    #[ts(optional)]
    limit: Option<u32>,
}

async fn events(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<EventsQuery>,
) -> ApiResult<Json<Vec<Event>>> {
    user_only(&p)?;
    let q = EventQuery { kind: q.kind, correlation: q.correlation, since: None, limit: q.limit.unwrap_or(50) };
    Ok(Json(hub.store.events(&q).await?))
}

async fn search(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<crate::search::SearchQuery>,
) -> ApiResult<Json<crate::search::SearchResults>> {
    let caller = Caller::resolve(&hub, p).await?;
    let args = q.into_search().map_err(OpError::from)?;
    Ok(Json(crate::search::search(&hub, &caller, args).await?))
}

/// Everything an event caused: which chats it reached, runs it started, its thread.
async fn trace(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<Trace>> {
    user_only(&p)?;
    let event = hub.store.event(&id).await?;
    let mut reached = Vec::new();
    for d in hub.store.deliveries_for_event(&id).await? {
        let inst = hub.store.find_instance(&d.instance).await?;
        reached.push(ReachedChat {
            instance: d.instance.clone(),
            title: inst.as_ref().and_then(|i| i.title.clone()),
            slug: inst.as_ref().and_then(|i| i.slug.clone()),
            // The session entry carrying this delivery id is where the web app scrolls to.
            session_anchor: SessionAnchor { custom_type: "botler.delivered".into(), delivery_id: d.id.clone() },
            delivery: d,
        });
    }
    let thread =
        hub.store.events(&EventQuery { correlation: Some(id.clone()), limit: 50, ..Default::default() }).await?;
    Ok(Json(Trace { event, reached, runs: hub.store.runs_for_event(&id).await?, thread }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct SessionQuery {
    /// An older session file of the chat (from `sessions`); default the current one.
    #[ts(optional)]
    file: Option<String>,
    /// Only the last this many items (`earlier` says how many come before); default all.
    #[ts(optional)]
    last: Option<usize>,
    /// Only the items after this item id, when it is on the active branch (`continues` says so):
    /// a refresh of a chat on screen carries only what is new.
    #[ts(optional)]
    after: Option<String>,
    /// Only items before this item id (with `last`: the `last` of them), scrolling back.
    #[ts(optional)]
    before: Option<String>,
}

async fn session(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
    Query(q): Query<SessionQuery>,
) -> ApiResult<Json<SessionView>> {
    user_only(&p)?;
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    let window = crate::sessions::Window { last: q.last, after: q.after.as_deref(), before: q.before.as_deref() };
    Ok(Json(session_view(&hub, inst, q.file.as_deref(), window).await?))
}

/// A chat's session (`window` of its items) with what the web app shows around it; the live
/// stream pushes the same for the chat on screen.
pub async fn session_view(
    hub: &Hub,
    inst: botler_store::Instance,
    file: Option<&str>,
    window: crate::sessions::Window<'_>,
) -> Result<SessionView, ApiError> {
    let live = hub.agents.get(&inst.name).await;
    let busy = live.as_ref().is_some_and(|a| a.busy());
    let compacting = live.as_ref().is_some_and(|a| a.compacting());
    let deliveries = hub.store.pending_deliveries(&inst.name).await?;
    let pending = deliveries.len();
    let mut handover = None;
    for d in &deliveries {
        let ev = hub.store.event(&d.event_id).await?;
        if ev.kind == botler_core::event::kinds::SESSION_STARTED {
            handover = Some(serde_json::from_value(ev.payload).map_err(|e| {
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("handover report {} is unreadable: {e}", ev.id))
            })?);
        }
    }
    let phase = hub.agents.phase(&inst.name).await;
    let page = crate::sessions::read_page(hub, &inst.name, file, window).await?;
    let sessions = crate::sessions::files(hub, &inst.name).await?;
    let file = file.map_or_else(|| inst.session_file.clone(), str::to_string);
    let tool_renders =
        hub.applied().await.defs.tools.values().filter_map(|t| Some((t.pi_name(), t.render.clone()?))).collect();
    let queued = queued(hub, &inst.name, &page.delivered).await?;
    Ok(SessionView {
        instance: inst,
        busy,
        compacting,
        pending,
        phase,
        items: page.items,
        earlier: page.earlier,
        continues: page.continues,
        tool_renders,
        queued,
        sessions,
        file,
        handover,
    })
}

#[derive(Deserialize, ts_rs::TS)]
pub struct NewChat {
    text: String,
    /// Agent definition; defaults to `chat`.
    #[serde(default)]
    #[ts(optional)]
    definition: Option<String>,
}

/// Start a new chat with the user's first message.
async fn new_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(c): Json<NewChat>,
) -> ApiResult<Json<ChatCreated>> {
    user_only(&p)?;
    let def = c.definition.unwrap_or_else(|| "chat".into());
    let name = crate::router::new_instance(&hub, &def, None, None).await?;
    let ev =
        user_event(BuiltinPayload::Message(MessagePayload { text: c.text, files: vec![], mode: None, via: None }))?
            .to(name.clone());
    hub.emit(ev).await?;
    Ok(Json(ChatCreated { instance: name }))
}

#[derive(Deserialize, Default, ts_rs::TS)]
pub struct ArchiveRequest {
    /// Archive without waiting for the agent's handover (e.g. it no longer answers).
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    force: bool,
    #[serde(default)]
    #[ts(optional)]
    reason: Option<String>,
}

async fn archive_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
    body: Option<Json<ArchiveRequest>>,
) -> ApiResult<Json<ArchiveOutcome>> {
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
        crate::storage::force_archive(&hub, &inst.name, req.reason).await?;
        return Ok(Json(ArchiveOutcome {
            instance: inst.name,
            archived: true,
            note: "Archived without the agent's handover.".into(),
        }));
    }
    crate::storage::request_archive(&hub, &inst.name, req.reason, Source::User).await?;
    Ok(Json(ArchiveOutcome {
        instance: inst.name,
        archived: false,
        note: "The agent will hand over and then archive itself.".into(),
    }))
}

#[derive(Deserialize, Default, ts_rs::TS)]
pub struct RestoreRequest {
    /// When its agent definition is gone: restore it as a generic `chat` agent.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    as_generic: bool,
}

async fn restore_chat(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
    body: Option<Json<RestoreRequest>>,
) -> ApiResult<Json<ChatRestored>> {
    user_only(&p)?;
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let inst = hub
        .store
        .resolve_instance(&instance)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no chat {instance}")))?;
    match crate::restore::restore(&hub, &inst.name, req.as_generic).await {
        Ok(changes) => {
            hub.dispatch_notify.notify_one();
            Ok(Json(ChatRestored { restored: inst.name, changes, definition_gone: None }))
        }
        Err(HubError::DefinitionGone { instance, definition }) => {
            Ok(Json(ChatRestored { restored: instance, changes: vec![], definition_gone: Some(definition) }))
        }
        Err(e) => Err(e.into()),
    }
}

async fn instances(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Vec<Instance>>> {
    user_only(&p)?;
    Ok(Json(hub.store.all_instances(200).await?))
}

#[derive(Deserialize, ts_rs::TS)]
#[ts(optional_fields)]
pub struct RunsQuery {
    /// Only the runs of this job, service or agent.
    runnable: Option<String>,
}

async fn runs(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Query(q): Query<RunsQuery>,
) -> ApiResult<Json<Vec<Run>>> {
    user_only(&p)?;
    Ok(Json(match q.runnable {
        Some(r) => hub.store.runs_for(&r, 100).await?,
        None => hub.store.recent_runs(100).await?,
    }))
}

/// Everything defined, what happens next, and how it went.
async fn system(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<crate::system::SystemView>> {
    user_only(&p)?;
    Ok(Json(crate::system::view(&hub).await?))
}

async fn incidents(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Vec<Incident>>> {
    user_only(&p)?;
    Ok(Json(hub.store.open_incidents().await?))
}

/// Declared secret groups, each field with whether it has a value (never the values), and who
/// is granted the group.
async fn secrets(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<Vec<SecretGroupView>>> {
    user_only(&p)?;
    let applied = hub.applied().await;
    let set = hub.store.secret_names().await?;
    let mut grantees: std::collections::BTreeMap<&str, Vec<String>> = std::collections::BTreeMap::new();
    for a in applied.defs.agents.values() {
        for g in &a.spec.env.secrets {
            grantees.entry(g.as_str()).or_default().push(format!("agent {}", a.name));
        }
    }
    for j in applied.defs.jobs.values() {
        for g in &j.spec.env.secrets {
            grantees.entry(g.as_str()).or_default().push(format!("job {}", j.name));
        }
    }
    for (to, s) in applied.defs.instance.models.key_secrets() {
        if let Some(sec) = applied.defs.secrets.get(s) {
            grantees.entry(sec.group.as_str()).or_default().push(to);
        }
    }
    let mut holders: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for g in hub.store.all_live_grants().await? {
        if g.kind == botler_core::defs::ResourceKind::Secret && g.stale.is_none() {
            holders.entry(g.resource).or_default().push(g.holder);
        }
    }
    let groups = applied
        .defs
        .secret_groups
        .values()
        .map(|g| SecretGroupView {
            name: g.name.clone(),
            description: g.description.clone(),
            class: g.class,
            complete: g.secrets.iter().all(|n| set.contains(n)),
            fields: g
                .secrets
                .iter()
                .filter_map(|n| applied.defs.secrets.get(n))
                .map(|s| SecretFieldView {
                    name: s.name.clone(),
                    description: s.description.clone(),
                    file: s.file.clone(),
                    has_value: set.contains(&s.name),
                })
                .collect(),
            granted_to: grantees.get(g.name.as_str()).cloned().unwrap_or_default(),
            held_by: holders.remove(&g.name).unwrap_or_default(),
        })
        .collect();
    Ok(Json(groups))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct SecretValue {
    value: crate::secret_requests::SecretInput,
}

async fn set_secret(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
    Json(v): Json<SecretValue>,
) -> ApiResult<Json<Saved>> {
    user_only(&p)?;
    let Some(decl) = hub.applied().await.defs.secrets.get(&name).cloned() else {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("secret {name} is not declared in secrets.toml")));
    };
    let bytes =
        v.value.decode(&name, decl.file.is_some()).map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    hub.store.set_secret_bytes(&hub.secret_key, &name, &bytes).await?;
    crate::secret_requests::resolve_satisfied(&hub).await?;
    // Deliveries waiting on a missing secret can proceed now.
    hub.dispatch_notify.notify_one();
    Ok(Json(Saved { ok: true }))
}

async fn settings(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<crate::settings::Settings>> {
    user_only(&p)?;
    Ok(Json(crate::settings::get(&hub).await?))
}

async fn save_settings(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(edit): Json<crate::settings::SettingsEdit>,
) -> ApiResult<Json<crate::settings::SettingsSaved>> {
    user_only(&p)?;
    Ok(Json(crate::settings::save(&hub, &edit).await?))
}

async fn models(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<crate::model_catalog::CatalogView>> {
    user_only(&p)?;
    Ok(Json(crate::model_catalog::view(&hub).await))
}

async fn proposals(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Vec<Proposal>>> {
    user_only(&p)?;
    Ok(Json(hub.store.recent_proposals(50).await?))
}

async fn proposal(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<ProposalView>> {
    user_only(&p)?;
    Ok(Json(proposal_view(&hub, &id).await?))
}

async fn proposal_view(hub: &Hub, id: &str) -> Result<ProposalView, ApiError> {
    let proposal = hub.store.proposal(id).await?;
    let tz = hub.applied().await.defs.timezone;
    let explained = crate::transactions::explain(&proposal, false);
    Ok(ProposalView { summary: proposal.summary.as_ref().map(|s| s.to_markdown(tz)), explained, proposal })
}

async fn proposal_diff(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::review::ProposalDiff>> {
    user_only(&p)?;
    Ok(Json(crate::review::proposal_diff(&hub, &id).await?))
}

/// The user's decision on a proposal.
#[derive(Deserialize, ts_rs::TS)]
pub struct ProposalVerdict {
    approve: bool,
    /// Required to reject; an optional comment when approving. The agent reads it.
    #[serde(default)]
    #[ts(optional)]
    reason: Option<String>,
}

async fn decide_proposal(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
    Json(v): Json<ProposalVerdict>,
) -> ApiResult<Json<ProposalView>> {
    user_only(&p)?;
    decide(&hub, &id, v.approve, v.reason.as_deref()).await?;
    Ok(Json(proposal_view(&hub, &id).await?))
}

async fn decide(hub: &Arc<Hub>, id: &str, approve: bool, reason: Option<&str>) -> Result<(), ApiError> {
    if !approve && reason.is_none_or(|r| r.trim().is_empty()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "rejecting needs a reason; the agent reads it".into()));
    }
    Ok(crate::transactions::on_approval(hub, id, approve, reason).await?)
}

#[derive(Deserialize, ts_rs::TS)]
pub struct RevertRequest {
    commit: String,
}

/// Revert a merged commit. The user is the authority, so this applies directly.
async fn revert(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(r): Json<RevertRequest>,
) -> ApiResult<Json<Reverted>> {
    user_only(&p)?;
    let head = crate::transactions::revert(&hub, &r.commit).await?;
    Ok(Json(Reverted { reverted: r.commit, commit: head }))
}

async fn login(
    State(hub): State<AppState>,
    headers: HeaderMap,
    Json(s): Json<crate::login::SignIn>,
) -> Result<Response, ApiError> {
    match crate::login::sign_in(&hub, &s).await {
        Ok((device, cookie)) => {
            let set = crate::login::set_cookie(&cookie, crate::login::is_https(&headers));
            let mut r = Json(SignedIn { device }).into_response();
            r.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&set).map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
            );
            Ok(r)
        }
        Err(crate::login::SignInError::Invalid) => {
            // Slows guessing down; a real code or token answers at once.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Err(ApiError(StatusCode::UNAUTHORIZED, crate::login::SignInError::Invalid.to_string()))
        }
        Err(crate::login::SignInError::Hub(e)) => Err((&e).into()),
    }
}

async fn login_code(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<LoginCode>> {
    user_only(&p)?;
    let (code, expires_at) = crate::login::create_code(&hub).await?;
    Ok(Json(LoginCode { code, expires_at }))
}

async fn devices(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    current: Option<Extension<CurrentDevice>>,
) -> ApiResult<Json<Vec<DeviceView>>> {
    user_only(&p)?;
    let me = current.map(|c| c.0.0);
    let list = hub.store.devices().await?;
    Ok(Json(
        list.into_iter().map(|d| DeviceView { current: me.as_deref() == Some(d.id.as_str()), device: d }).collect(),
    ))
}

async fn revoke_device(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<DeviceRevoked>> {
    user_only(&p)?;
    let revoked = hub.store.revoke_device(&id).await?;
    tracing::info!(device = %id, revoked, "device signed out");
    Ok(Json(DeviceRevoked { revoked }))
}

/// Sign this device out (and drop its cookie).
async fn logout(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    current: Option<Extension<CurrentDevice>>,
) -> Result<Response, ApiError> {
    user_only(&p)?;
    let revoked = match current {
        Some(c) => hub.store.revoke_device(&c.0.0).await?,
        None => false,
    };
    let mut r = Json(DeviceRevoked { revoked }).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&crate::login::clear_cookie())
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
    );
    Ok(r)
}

/// The model gateway for jobs and services (see `gateway.rs`), in OpenAI's error shape.
async fn chat_completions(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(body): Json<serde_json::Value>,
) -> Response {
    use crate::gateway::GatewayError as G;
    let fail = |status: StatusCode, m: String| {
        (status, Json(serde_json::json!({ "error": { "message": m } }))).into_response()
    };
    let caller = match Caller::resolve(&hub, p).await {
        Ok(c) => c,
        Err(e) => return fail(StatusCode::FORBIDDEN, e.to_string()),
    };
    match crate::gateway::chat(&hub, &caller, body).await {
        Ok((status, reply)) => {
            (StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY), Json(reply)).into_response()
        }
        Err(e @ G::Forbidden(_)) => fail(StatusCode::FORBIDDEN, e.to_string()),
        Err(e @ G::BadRequest(_)) => fail(StatusCode::BAD_REQUEST, e.to_string()),
        Err(e @ G::Budget(_)) => fail(StatusCode::TOO_MANY_REQUESTS, e.to_string()),
        Err(e @ G::Upstream(_)) => fail(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(e @ G::Held { .. }) => fail(StatusCode::SERVICE_UNAVAILABLE, e.to_string()),
        Err(G::Hub(e)) => ApiError::from(&e).into_response(),
    }
}

async fn apps(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Vec<AppView>>> {
    user_only(&p)?;
    Ok(Json(apps_view(&hub).await?))
}

/// The apps services serve (`GET /apps`; pushed when they change).
pub async fn apps_view(hub: &Hub) -> Result<Vec<AppView>, ApiError> {
    let applied = hub.applied().await;
    let mut out = Vec::new();
    for j in applied.defs.jobs.values().filter(|j| j.spec.is_service()) {
        let Some(app) = &j.spec.app else { continue };
        let running = hub
            .containers
            .inspect(&crate::services::container_name(hub, &j.name))
            .await
            .map_err(HubError::from)?
            .is_some_and(|c| c.running);
        out.push(AppView {
            name: j.name.clone(),
            title: app.title.clone(),
            icon: app.icon.clone().unwrap_or_else(|| "app-window".into()),
            description: j.spec.description.clone(),
            running,
        });
    }
    Ok(out)
}

async fn app_ticket(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(name): Path<String>,
) -> ApiResult<Json<AppTicket>> {
    user_only(&p)?;
    if !hub.applied().await.defs.jobs.get(&name).is_some_and(|j| j.spec.app.is_some()) {
        return Err(ApiError(StatusCode::NOT_FOUND, format!("no app named {name}")));
    }
    let ticket = crate::apps::ticket(&hub, &name);
    Ok(Json(AppTicket { port: crate::apps::port(&hub), path: crate::apps::enter_path(&name, &ticket) }))
}

async fn channels(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
) -> ApiResult<Json<crate::channels::ChannelsView>> {
    user_only(&p)?;
    Ok(Json(crate::channels::view(&hub).await?))
}

async fn channel_code(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(n): Json<crate::channels::NewPairingCode>,
) -> ApiResult<Json<crate::channels::PairingCode>> {
    user_only(&p)?;
    let target = match n.target.as_deref() {
        None | Some("") => botler_core::FRONT_DOOR.to_string(),
        Some(t) => hub
            .store
            .resolve_instance(t)
            .await?
            .map(|i| i.name)
            .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, format!("no chat named {t}")))?,
    };
    let (code, expires_at) = crate::channels::create_code(&hub, &target).await?;
    Ok(Json(crate::channels::PairingCode { code, target, expires_at }))
}

async fn revoke_binding(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(id): Path<String>,
) -> ApiResult<Json<crate::channels::BindingRevoked>> {
    user_only(&p)?;
    Ok(Json(crate::channels::BindingRevoked { revoked: hub.store.revoke_channel_binding(&id).await? }))
}

async fn pause(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<PauseState>> {
    user_only(&p)?;
    crate::control::pause(&hub).await?;
    Ok(Json(PauseState { paused: true }))
}

async fn resume(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<PauseState>> {
    user_only(&p)?;
    crate::control::resume(&hub).await?;
    Ok(Json(PauseState { paused: false }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct ClearBreaker {
    /// The breaker's subject, as in `HubStatus.breakers` (`chain:<id>`, `pair:<a>|<b>`).
    subject: String,
}

/// Resume what a breaker paused (a pair's held messages are delivered).
async fn clear_breaker(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Json(c): Json<ClearBreaker>,
) -> ApiResult<Json<BreakerCleared>> {
    user_only(&p)?;
    let (cleared, released) = crate::loops::clear_breaker(&hub, &c.subject).await?;
    Ok(Json(BreakerCleared { cleared, released }))
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
        // A `monorepo = true` job's run pushes for the chat that started it.
        Principal::Run { run_id } => {
            let run = hub.store.run(run_id).await?;
            let granted = hub.applied().await.defs.jobs.get(&run.runnable).is_some_and(|j| j.spec.monorepo);
            if !granted || run.status.is_terminal() {
                return Err(ApiError(StatusCode::FORBIDDEN, format!("job {} has no monorepo access", run.runnable)));
            }
            match crate::jobs::requester(&hub, &run).await? {
                Some(chat) => chat,
                // Started by no chat: it may read, and push nowhere a chat would propose from.
                None => format!("run-{}", run.id),
            }
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
