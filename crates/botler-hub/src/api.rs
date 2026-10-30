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
use botler_core::payloads::{BuiltinPayload, DialogAnsweredPayload, MessagePayload, SecretsOutcome, SendMode};
use botler_core::{Event, Source};
use botler_store::{DialogStatus, EventQuery, Incident, Instance, Proposal, Run};
use chrono::Utc;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Deserialize;
use serde_json::Value;

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::ops::{self, Caller, OpError};
use crate::wire::{
    ApiErrorBody, ArchiveOutcome, BreakerCleared, BudgetView, Cancelled, ChatCreated, ChatRestored, DialogAnswer,
    DialogView, Dialogs, EventPosted, HubStatus, PauseState, ProposalView, QueuedMessage, ReachedChat, Reverted,
    RuntimeInfo, Saved, SecretFieldView, SecretGroupView, SessionAnchor, SessionView, Trace, Usage,
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
        .route("/api/v1/messages", post(user_message))
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
        .route("/git/monorepo.git/{*rest}", any(git))
        .layer(middleware::from_fn_with_state(hub.clone(), authenticate));
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // The live stream authenticates itself (browsers cannot set headers on WebSockets).
        .route("/api/v1/live", get(crate::live::socket))
        .merge(authed)
        // The web app is public static content; every API call it makes carries the user token.
        .route("/legacy", get(crate::web::legacy))
        .fallback(crate::web::app)
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
    let applied = hub.applied().await;
    let info = hub.containers.info();
    let services = crate::services::status(&hub).await?;
    Ok(Json(HubStatus {
        version: env!("CARGO_PKG_VERSION").into(),
        commit: applied.commit.clone(),
        paused: hub.paused().await?,
        front_door: botler_core::FRONT_DOOR.into(),
        timezone: applied.defs.timezone.name().into(),
        runtime: RuntimeInfo { binary: info.binary.clone(), version: info.version.clone(), rootless: info.rootless },
        open_dialogs: hub.store.open_dialogs().await?.len(),
        open_incidents: hub.store.open_incidents().await?.len(),
        breakers: hub.store.breakers().await?,
        attention: crate::system::attention(&hub, &services).await?,
        services,
    }))
}

/// Model spend today (in the hub's timezone) and all time, per chat.
async fn usage(State(hub): State<AppState>, Extension(p): Extension<Principal>) -> ApiResult<Json<Usage>> {
    user_only(&p)?;
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
    Ok(Json(Usage {
        day_start,
        timezone: tz.name().into(),
        today: hub.store.usage_since(None, day_start).await?,
        today_by_instance: hub.store.usage_by_instance(day_start).await?,
        total_by_instance: hub.store.usage_by_instance(chrono::DateTime::<Utc>::UNIX_EPOCH).await?,
        default_budget: BudgetView { usd_per_day: budget.usd_per_day, tokens_per_day: budget.tokens_per_day },
    }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct UserMessage {
    /// The chat; defaults to the front door.
    #[serde(default = "main_instance")]
    #[ts(optional, as = "Option<String>")]
    to: String,
    text: String,
    /// While the agent works: wait for its turn to end (the default) or interrupt it.
    #[serde(default)]
    #[ts(optional, as = "Option<SendMode>")]
    mode: SendMode,
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
    let payload = MessagePayload { text: m.text, files: vec![], mode: Some(m.mode) };
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

/// The user's messages waiting for the agent's turn to end.
async fn queued(hub: &Hub, instance: &str) -> Result<Vec<QueuedMessage>, ApiError> {
    let mut out = Vec::new();
    for d in hub.store.pending_deliveries(instance).await? {
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
    let applied = hub.applied().await;
    let mut open = Vec::new();
    for d in hub.store.open_dialogs().await? {
        let mut view = DialogView { dialog: d, form: None, remove: None, group: None, new_group: None };
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
    Ok(Json(Dialogs { open, recent: hub.store.recent_dialogs(50).await? }))
}

#[derive(Deserialize, ts_rs::TS)]
pub struct Answer {
    answer: String,
    /// Approvals: why the user rejects (required) or a comment on approving. The agent reads it.
    #[serde(default)]
    #[ts(optional)]
    reason: Option<String>,
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
    let d = hub.store.answer_dialog(&id, &a.answer).await?;
    let ev = user_event(BuiltinPayload::DialogAnswered(DialogAnsweredPayload {
        dialog_id: d.id.clone(),
        question: d.question.clone(),
        answer: a.answer,
        group: None,
        outcome: None,
        note: None,
    }))?
    .to(d.instance.clone());
    hub.emit(ev).await?;
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

async fn session(
    State(hub): State<AppState>,
    Extension(p): Extension<Principal>,
    Path(instance): Path<String>,
) -> ApiResult<Json<SessionView>> {
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
    let tool_renders =
        hub.applied().await.defs.tools.values().filter_map(|t| Some((t.pi_name(), t.render.clone()?))).collect();
    let queued = queued(&hub, &inst.name).await?;
    Ok(Json(SessionView { instance: inst, busy, pending, phase, items, tool_renders, queued }))
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
    let ev = user_event(BuiltinPayload::Message(MessagePayload { text: c.text, files: vec![], mode: None }))?
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
    for (r, route) in &applied.defs.instance.models.routes {
        if let Some(sec) = route.api_key_secret.as_ref().and_then(|n| applied.defs.secrets.get(n)) {
            grantees.entry(sec.group.as_str()).or_default().push(format!("model route {r}"));
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
    // Deliveries waiting on a missing secret can proceed now.
    hub.dispatch_notify.notify_one();
    Ok(Json(Saved { ok: true }))
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
    Ok(ProposalView { summary: proposal.summary.as_ref().map(|s| s.to_markdown(tz)), proposal })
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
