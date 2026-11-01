//! The shapes of the HTTP API and the route table.
//!
//! Every response the web app reads is a named type here (or a store/core record it embeds).
//! The web app's TypeScript types and its typed route table are generated from these with ts-rs
//! (see [`crate::bindings`]); nothing on the frontend side is maintained by hand.

use std::collections::BTreeMap;

use botler_core::Event;
use botler_core::defs::{SecretClass, ToolRender};
use botler_core::payloads::{SecretsOutcome, SendMode};
use botler_store::{Breaker, Delivery, Dialog, Incident, Instance, Proposal, Run, UsageTotals};
use chrono::{DateTime, Utc};
use serde::Serialize;
use ts_rs::TS;
use uuid::Uuid;

use crate::api::{
    Answer, ArchiveRequest, ClearBreaker, EventsQuery, NewChat, ProposalVerdict, RestoreRequest, RevertRequest,
    SecretValue, SessionQuery, UploadQuery, UserMessage,
};
use crate::memory::{MemoryConflict, MemoryHistory, MemoryList, MemoryRead, MemoryWrite};
use crate::ops::{ClearChatCaches, CollectGarbage, DeleteChat, StorageUsage};
use crate::ops::{
    ForkChat, GetProposal, GetRunLogs, GetSessionTree, GotoEntry, Handover, LabelEntry, ListConnections, Op,
    RevokeAccess, StopChat, Unsubscribe,
};
use crate::phase::WakePhase;
use crate::review::ProposalDiff;
use crate::search::{SearchQuery, SearchResults};
use crate::secret_requests::{NewField, Submission};
use crate::sessions::Item;

/// Every error response: a status code and this body.
#[derive(Debug, Serialize, TS)]
pub struct ApiErrorBody {
    pub error: String,
    /// For memory writes based on a stale version (409): the current version.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub conflict: Option<MemoryConflict>,
}

#[derive(Debug, Serialize, TS)]
pub struct RuntimeInfo {
    pub binary: String,
    pub version: String,
    pub rootless: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct HubStatus {
    pub version: String,
    /// The monorepo commit whose definitions are applied.
    pub commit: String,
    pub paused: bool,
    /// The front-door chat (the user's default conversation, pinned on top).
    pub front_door: String,
    /// The instance's time zone (IANA name); agents and delivery headers use it.
    pub timezone: String,
    pub runtime: RuntimeInfo,
    /// Degraded: the applied commit does not load, so these parts of it are left out until a
    /// change fixes them (empty normally).
    pub definition_problems: Vec<botler_core::Diagnostic>,
    pub skipped_definitions: Vec<String>,
    pub open_dialogs: usize,
    pub open_incidents: usize,
    pub breakers: Vec<Breaker>,
    pub services: Vec<ServiceStatus>,
    /// What needs attention, for the sidebar's counts.
    pub attention: crate::system::Attention,
    /// The applied jobs and services, agent definitions and secret groups, by name: the web app
    /// links them where agents mention them.
    pub jobs: Vec<String>,
    pub agents: Vec<String>,
    pub secret_groups: Vec<String>,
}

/// A service of the applied definitions and where it can be reached.
#[derive(Debug, Serialize, TS)]
pub struct ServiceStatus {
    pub name: String,
    pub state: ServiceState,
    /// Its address for agents granted it, on the services network (`http://<name>:<port>`).
    pub internal_url: Option<String>,
    /// Ports published on the host, reachable from outside.
    pub published: Vec<PublishedPort>,
}

/// Where a service is in its life, as the reconciler sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Running,
    /// About to start (or being started).
    Starting,
    /// Exited; restarted once its backoff is over.
    Backoff,
    /// Crash-looping: not restarted until its incident is resolved.
    Failed,
    /// botler is paused.
    Paused,
}

#[derive(Debug, Serialize, TS)]
pub struct PublishedPort {
    pub bind: String,
    pub host_port: u16,
    pub port: u16,
    /// A link to it: the host's name for a `0.0.0.0` bind, else the bound address.
    pub url: String,
}

#[derive(Debug, Serialize, TS)]
pub struct EventPosted {
    pub event_id: Uuid,
}

/// A dialog as the user sees it. Credentials requests carry their form.
#[derive(Debug, Serialize, TS)]
pub struct DialogView {
    #[serde(flatten)]
    pub dialog: Dialog,
    /// Fields whose values the user enters (credentials requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub form: Option<Vec<NewField>>,
    /// Fields that will be removed, values included (credentials requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub remove: Option<Vec<String>>,
    /// The secret group (credentials requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub group: Option<String>,
    /// Whether the group does not exist yet (credentials requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub new_group: Option<bool>,
    /// What to open (visit requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub visit: Option<crate::ops::VisitSpec>,
}

/// An app (a service's web UI) for the navigation.
#[derive(Debug, Serialize, TS)]
pub struct AppView {
    pub name: String,
    pub title: String,
    /// A Lucide icon name.
    pub icon: String,
    pub description: String,
    pub running: bool,
}

/// Where the iframe opens an app: `path` on the apps `port` of the host the web app is on.
#[derive(Debug, Serialize, TS)]
pub struct AppTicket {
    pub port: u16,
    pub path: String,
}

/// A device signed in.
#[derive(Debug, Serialize, TS)]
pub struct SignedIn {
    pub device: botler_store::Device,
}

#[derive(Debug, Serialize, TS)]
pub struct LoginCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, TS)]
pub struct DeviceView {
    #[serde(flatten)]
    pub device: botler_store::Device,
    /// The device this request comes from.
    pub current: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct DeviceRevoked {
    pub revoked: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct Dialogs {
    pub open: Vec<DialogView>,
    /// The latest dialogs in any state, newest first.
    pub recent: Vec<Dialog>,
}

/// A proposal for the user's review, its deployment summary as markdown.
#[derive(Debug, Serialize, TS)]
pub struct ProposalView {
    pub proposal: Proposal,
    pub summary: Option<String>,
    /// Where it stands and what happens next, in plain words (what its agent reads in `process`).
    pub explained: String,
}

#[derive(Debug, Serialize, TS)]
pub struct DialogAnswer {
    pub dialog: Dialog,
}

/// Where in a session a delivery landed (the web app scrolls there).
#[derive(Debug, Serialize, TS)]
pub struct SessionAnchor {
    pub custom_type: String,
    pub delivery_id: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ReachedChat {
    pub instance: String,
    pub title: Option<String>,
    pub slug: Option<String>,
    pub session_anchor: SessionAnchor,
    pub delivery: Delivery,
}

/// Everything an event caused: which chats it reached, runs it started, its thread.
#[derive(Debug, Serialize, TS)]
pub struct Trace {
    pub event: Event,
    pub reached: Vec<ReachedChat>,
    pub runs: Vec<Run>,
    pub thread: Vec<Event>,
}

#[derive(Debug, Serialize, TS)]
pub struct SessionView {
    pub instance: Instance,
    /// pi is working on a turn.
    pub busy: bool,
    /// Deliveries waiting to be put into the session.
    pub pending: usize,
    /// Waking progress, or why the last wake failed.
    pub phase: Option<WakePhase>,
    pub items: Vec<Item>,
    /// Items of this session before `items` (`last` left them out).
    pub earlier: usize,
    /// Render hints of the Python tools that declare one, by the name pi calls them (`web_search`).
    pub tool_renders: BTreeMap<String, ToolRender>,
    /// The user's messages not in the session yet, oldest first: held by the hub (waiting for
    /// the run to end, or for the agent to wake), or handed to pi as steers.
    pub queued: Vec<QueuedMessage>,
    /// The chat's session files, oldest first, the current one last: earlier ones are from
    /// handovers to a fresh session.
    pub sessions: Vec<String>,
    /// The session file shown.
    pub file: String,
    /// The handover report waiting to open the current session (it lands when the agent next
    /// wakes); shown while the fresh session is still empty.
    pub handover: Option<botler_core::payloads::SessionStartedPayload>,
}

#[derive(Debug, Serialize, TS)]
pub struct QueuedMessage {
    /// Cancel with `DELETE /api/v1/messages/{id}` (this id).
    pub delivery: String,
    pub text: String,
    pub at: DateTime<Utc>,
    pub mode: SendMode,
    /// Handed to pi already (a steer it adds once the running tool calls are done): it can no
    /// longer be taken back.
    pub handed_over: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct Cancelled {
    pub delivery: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatCreated {
    pub instance: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ArchiveOutcome {
    pub instance: String,
    /// Archived right away (forced); otherwise the agent hands over first.
    pub archived: bool,
    pub note: String,
}

#[derive(Debug, Serialize, TS)]
pub struct ChatRestored {
    pub restored: String,
    /// What changed while it was archived (the agent is told too).
    pub changes: Vec<String>,
    /// Set when it was NOT restored because its agent definition is gone: restore it again with
    /// `as_generic`, or delete it.
    pub definition_gone: Option<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct SecretFieldView {
    pub name: String,
    pub description: String,
    /// Delivered as a file at this path instead of an env var.
    pub file: Option<String>,
    pub has_value: bool,
}

/// A declared secret group: its fields and whether each has a value (never the values).
#[derive(Debug, Serialize, TS)]
pub struct SecretGroupView {
    pub name: String,
    pub description: String,
    pub class: SecretClass,
    pub complete: bool,
    pub fields: Vec<SecretFieldView>,
    /// Definitions (and model calls) it is granted to.
    pub granted_to: Vec<String>,
    /// Chats holding it through a grant (also archived ones, whose grants are suspended).
    pub held_by: Vec<String>,
}

#[derive(Debug, Serialize, TS)]
pub struct Saved {
    pub ok: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct Reverted {
    pub reverted: String,
    /// The new main.
    pub commit: String,
}

#[derive(Debug, Serialize, TS)]
pub struct BreakerCleared {
    /// Whether the breaker was tripped.
    pub cleared: bool,
    /// Held messages released for delivery (pair breakers).
    pub released: u64,
}

#[derive(Debug, Serialize, TS)]
pub struct PauseState {
    pub paused: bool,
}

#[derive(Debug, Serialize, TS)]
pub struct BudgetView {
    pub usd_per_day: Option<f64>,
    pub tokens_per_day: Option<u64>,
}

/// Model spend: today (in the hub's timezone) and all time.
#[derive(Debug, Serialize, TS)]
pub struct Usage {
    pub day_start: DateTime<Utc>,
    pub timezone: String,
    pub today: UsageTotals,
    pub today_by_instance: BTreeMap<String, UsageTotals>,
    pub total_by_instance: BTreeMap<String, UsageTotals>,
    /// The per-agent daily budget for agents that do not declare their own.
    pub default_budget: BudgetView,
}

/// One HTTP route as the generated client sees it. Built by [`routes!`].
#[derive(Debug, Clone)]
pub struct RouteSpec {
    pub name: &'static str,
    pub method: &'static str,
    pub path: String,
    /// TypeScript names of the query, body and response types.
    pub query: Option<String>,
    pub body: Option<String>,
    pub response: String,
}

/// Visits the TypeScript types a route table mentions (see [`crate::bindings`]).
pub trait TypeSink {
    fn add<T: TS + 'static + ?Sized>(&mut self);
}

/// The route table: the ops the web app calls as `ops { name: OpInput; }` (`POST
/// /api/v1/ops/{op}` with the op's input and output; listed last), then
/// `name: METHOD "path" [, query: Q] [, body: B] => Response;`. Path parameters are written
/// `{name}`. Handlers in `api.rs` are registered at the same paths; a test checks that every path
/// here is served.
macro_rules! routes {
    (@name $cfg:ident) => { None };
    (@name $cfg:ident $t:ty) => { Some(<$t as TS>::name($cfg)) };
    (
        ops { $( $op_route:ident : $op:ty ; )* }
        $( $name:ident : $method:ident $path:literal $(, query: $query:ty)? $(, body: $body:ty)? => $resp:ty ; )*
    ) => {
        pub fn route_table(cfg: &ts_rs::Config) -> Vec<RouteSpec> {
            let mut table = vec![$(
                RouteSpec {
                    name: stringify!($name),
                    method: stringify!($method),
                    path: $path.into(),
                    query: routes!(@name cfg $($query)?),
                    body: routes!(@name cfg $($body)?),
                    response: <$resp as TS>::name(cfg),
                }
            ),*];
            $(
                table.push(RouteSpec {
                    name: stringify!($op_route),
                    method: "POST",
                    path: format!("/api/v1/ops/{}", <$op as Op>::NAME),
                    query: None,
                    body: Some(<$op as TS>::name(cfg)),
                    response: <<$op as Op>::Output as TS>::name(cfg),
                });
            )*
            table
        }

        /// Hand every type the routes use to `sink`.
        pub fn route_types(sink: &mut impl TypeSink) {
            $(
                $( sink.add::<$query>(); )?
                $( sink.add::<$body>(); )?
                sink.add::<$resp>();
            )*
            $(
                sink.add::<$op>();
                sink.add::<<$op as Op>::Output>();
            )*
        }
    };
}

routes! {
    ops {
        run_logs: GetRunLogs;
        proposal: GetProposal;
        memory_list: MemoryList;
        memory_read: MemoryRead;
        memory_write: MemoryWrite;
        memory_history: MemoryHistory;
        storage_usage: StorageUsage;
        clear_chat_caches: ClearChatCaches;
        delete_chat: DeleteChat;
        collect_garbage: CollectGarbage;
        connections: ListConnections;
        revoke_access: RevokeAccess;
        unsubscribe: Unsubscribe;
        run_job: crate::ops::RunJob;
        restart_service: crate::ops::RestartService;
        stop_chat: StopChat;
        handover: Handover;
        compact_chat: crate::ops::CompactChat;
        clear_chat: crate::ops::ClearChat;
        raise_budget: crate::ops::RaiseBudget;
        pin_chat: crate::ops::PinChat;
        rename_chat: crate::ops::RenameChat;
        list_documents: crate::ops::ListDocuments;
        read_document: crate::ops::ReadDocument;
        write_document: crate::ops::WriteDocument;
        delete_document: crate::ops::DeleteDocument;
        session_tree: GetSessionTree;
        goto_entry: GotoEntry;
        label_entry: LabelEntry;
        fork_chat: ForkChat;
    }
    status: GET "/api/v1/status" => HubStatus;
    usage: GET "/api/v1/usage" => Usage;
    send_message: POST "/api/v1/messages", body: UserMessage => EventPosted;
    upload: PUT "/api/v1/uploads", query: UploadQuery => crate::uploads::Uploaded;
    cancel_message: DELETE "/api/v1/messages/{id}" => Cancelled;
    dialogs: GET "/api/v1/dialogs" => Dialogs;
    answer_dialog: POST "/api/v1/dialogs/{id}/answer", body: Answer => DialogAnswer;
    submit_secrets: POST "/api/v1/dialogs/{id}/secrets", body: Submission => SecretsOutcome;
    events: GET "/api/v1/events", query: EventsQuery => Vec<Event>;
    trace: GET "/api/v1/events/{id}/trace" => Trace;
    search: GET "/api/v1/search", query: SearchQuery => SearchResults;
    instances: GET "/api/v1/instances" => Vec<Instance>;
    session: GET "/api/v1/sessions/{instance}", query: SessionQuery => SessionView;
    new_chat: POST "/api/v1/chats", body: NewChat => ChatCreated;
    archive_chat: POST "/api/v1/chats/{instance}/archive", body: ArchiveRequest => ArchiveOutcome;
    restore_chat: POST "/api/v1/chats/{instance}/restore", body: RestoreRequest => ChatRestored;
    runs: GET "/api/v1/runs", query: crate::api::RunsQuery => Vec<Run>;
    system: GET "/api/v1/system" => crate::system::SystemView;
    incidents: GET "/api/v1/incidents" => Vec<Incident>;
    secrets: GET "/api/v1/secrets" => Vec<SecretGroupView>;
    set_secret: PUT "/api/v1/secrets/{name}", body: SecretValue => Saved;
    proposals: GET "/api/v1/proposals" => Vec<Proposal>;
    proposal_view: GET "/api/v1/proposals/{id}" => ProposalView;
    proposal_diff: GET "/api/v1/proposals/{id}/diff" => ProposalDiff;
    decide_proposal: POST "/api/v1/proposals/{id}/decision", body: ProposalVerdict => ProposalView;
    revert: POST "/api/v1/revert", body: RevertRequest => Reverted;
    pause: POST "/api/v1/pause" => PauseState;
    resume: POST "/api/v1/resume" => PauseState;
    clear_breaker: POST "/api/v1/breakers/clear", body: ClearBreaker => BreakerCleared;
    settings: GET "/api/v1/settings" => crate::settings::Settings;
    save_settings: PUT "/api/v1/settings", body: crate::settings::SettingsEdit => crate::settings::SettingsSaved;
    models: GET "/api/v1/models" => crate::model_catalog::CatalogView;
    login: POST "/api/v1/login", body: crate::login::SignIn => SignedIn;
    login_code: POST "/api/v1/login/codes" => LoginCode;
    devices: GET "/api/v1/devices" => Vec<DeviceView>;
    revoke_device: DELETE "/api/v1/devices/{id}" => DeviceRevoked;
    logout: POST "/api/v1/logout" => DeviceRevoked;
    apps: GET "/api/v1/apps" => Vec<AppView>;
    app_ticket: POST "/api/v1/apps/{name}/ticket" => AppTicket;
    channels: GET "/api/v1/channels" => crate::channels::ChannelsView;
    channel_code: POST "/api/v1/channels/codes", body: crate::channels::NewPairingCode => crate::channels::PairingCode;
    revoke_binding: DELETE "/api/v1/channels/bindings/{id}" => crate::channels::BindingRevoked;
}
