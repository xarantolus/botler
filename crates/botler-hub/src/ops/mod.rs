//! Everything agents, jobs and the user can ask the hub to do. Transport-neutral: the MCP server,
//! the SDK's HTTP API and the web app all call these, so permissions, rate limits and validation
//! live in one place. Each op is one [`Op`] implementation in the module of its domain, listed
//! once in [`OPS`].

mod chats;
mod incidents;
mod messages;
mod proposals;
mod requests;
mod state;
mod timers;

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Arc;

use botler_core::defs::Capability;
use botler_core::duration::HumanDuration;
use botler_core::limits::{RateLimit, default_limit};
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use ts_rs::TS;

pub use chats::*;
pub use incidents::*;
pub use messages::*;
pub use proposals::*;
pub use requests::*;
pub use state::*;
pub use timers::*;

use crate::auth::Principal;
use crate::hub::{Hub, HubError};
use crate::{health, memory, secret_requests};

#[derive(Debug, thiserror::Error)]
pub enum OpError {
    #[error(transparent)]
    Hub(#[from] HubError),
    #[error("invalid arguments: {0}")]
    BadRequest(String),
    #[error("not allowed: {0}")]
    Forbidden(String),
    #[error("unknown operation {0}")]
    Unknown(String),
    /// The event was rejected; the message says what to fix.
    #[error("{0}")]
    InvalidEvent(String),
}

impl From<botler_store::Error> for OpError {
    fn from(e: botler_store::Error) -> Self {
        Self::Hub(e.into())
    }
}

pub type OpResult = Result<Value, OpError>;

/// Who is calling, resolved from their token.
#[derive(Clone, Debug)]
pub struct Caller {
    pub principal: Principal,
    /// For agents: their definition's capabilities.
    pub capabilities: Vec<Capability>,
    /// For runs: the runnable.
    pub runnable: Option<String>,
}

impl Caller {
    pub async fn resolve(hub: &Hub, principal: Principal) -> Result<Self, OpError> {
        let applied = hub.applied().await;
        match &principal {
            Principal::User => Ok(Self { principal, capabilities: vec![], runnable: None }),
            Principal::Agent { instance: name, wake } => {
                if !hub.agents.is_current(name, wake).await {
                    return Err(OpError::Forbidden(format!("token belongs to a stopped session of {name}")));
                }
                let inst = hub
                    .store
                    .find_instance(name)
                    .await?
                    .ok_or_else(|| OpError::Forbidden(format!("unknown instance {name}")))?;
                let capabilities =
                    applied.defs.agents.get(&inst.definition).map(|d| d.spec.capabilities.clone()).unwrap_or_default();
                Ok(Self { principal, capabilities, runnable: None })
            }
            Principal::Run { run_id } => {
                let run = hub.store.run(run_id).await?;
                if run.status.is_terminal() {
                    return Err(OpError::Forbidden(format!("run {run_id} has finished")));
                }
                Ok(Self { principal, capabilities: vec![], runnable: Some(run.runnable) })
            }
        }
    }

    pub fn source(&self) -> botler_core::Source {
        use botler_core::Source;
        match &self.principal {
            Principal::User => Source::User,
            Principal::Agent { instance, .. } => Source::Agent(instance.clone()),
            Principal::Run { .. } => Source::Runnable(self.runnable.clone().unwrap_or_default()),
        }
    }

    /// Rate-limit subject.
    pub(crate) fn subject(&self) -> String {
        self.source().key()
    }

    pub(crate) fn instance(&self) -> Option<&str> {
        match &self.principal {
            Principal::Agent { instance, .. } => Some(instance),
            _ => None,
        }
    }

    /// The calling agent's instance; anyone else is refused with `refusal`.
    pub(crate) fn agent(&self, refusal: &str) -> Result<&str, OpError> {
        self.instance().ok_or_else(|| OpError::Forbidden(refusal.into()))
    }

    pub fn may(&self, cap: Capability) -> bool {
        matches!(self.principal, Principal::User) || self.capabilities.contains(&cap)
    }
}

/// Who may call an op. Callers only see the ops they may call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Everyone,
    /// The user and agents, not jobs or services.
    UserAndAgents,
    /// The user and runs: the callers without a memory mount.
    UserAndRuns,
    /// Jobs and services.
    Runs,
    /// The user and agents with this capability.
    Capable(Capability),
    /// Only agents with this capability.
    AgentsWith(Capability),
}

impl Who {
    pub fn admits(self, c: &Caller) -> bool {
        let run = matches!(c.principal, Principal::Run { .. });
        match self {
            Self::Everyone => true,
            Self::UserAndAgents => !run,
            Self::UserAndRuns => !matches!(c.principal, Principal::Agent { .. }),
            Self::Runs => run,
            Self::Capable(cap) => c.may(cap),
            Self::AgentsWith(cap) => c.capabilities.contains(&cap),
        }
    }
}

/// One operation, implemented by its input type, in the module of its domain and listed once in
/// [`OPS`]. The MCP tools, `/api/v1/ops/{name}`, the pi extension's turn-ending tools, the Python
/// SDK (`tests/sdk.rs`) and the TypeScript bindings are all generated from that list.
pub trait Op: DeserializeOwned + JsonSchema + TS + Send + 'static {
    const NAME: &'static str;
    /// What agents read in their tool list.
    const DESCRIPTION: &'static str;
    const WHO: Who;
    /// The agent's turn is over by nature after this op: the outcome arrives later as an event
    /// that wakes it, so another model call right after it can only produce filler or polling. MCP
    /// tool results cannot end a turn, so the pi extension registers these as its own tools that
    /// call `/api/v1/ops/{name}` and return `terminate: true` (from `turn-tools.json`, rendered by
    /// [`turn_ending_tools`]); their MCP duplicates are hidden from the agent in `mcp.json`.
    /// `send_message` is not one: work usually goes on after it.
    const ENDS_TURN: bool = false;
    type Output: Serialize + TS + 'static;

    fn call(self, hub: &Arc<Hub>, caller: &Caller) -> impl Future<Output = Result<Self::Output, OpError>> + Send;
}

/// An [`Op`] with its types erased, as [`OPS`] holds it.
pub trait AnyOp: Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn who(&self) -> Who;
    fn ends_turn(&self) -> bool;
    /// JSON Schema of the input.
    fn schema(&self) -> Value;
    /// TypeScript names of the input and output types.
    fn ts_names(&self, cfg: &ts_rs::Config) -> (String, String);
    /// Export the input and output types with their dependencies.
    fn export_types(&self, ex: &mut crate::bindings::Exporter<'_>);
    fn run<'a>(&'a self, hub: &'a Arc<Hub>, caller: &'a Caller, input: Value) -> BoxFuture<'a, OpResult>;
}

struct Registered<T>(PhantomData<fn() -> T>);

const fn op<T: Op>() -> Registered<T> {
    Registered(PhantomData)
}

impl<T: Op> AnyOp for Registered<T> {
    fn name(&self) -> &'static str {
        T::NAME
    }
    fn description(&self) -> &'static str {
        T::DESCRIPTION
    }
    fn who(&self) -> Who {
        T::WHO
    }
    fn ends_turn(&self) -> bool {
        T::ENDS_TURN
    }
    fn schema(&self) -> Value {
        schema::<T>()
    }
    fn ts_names(&self, cfg: &ts_rs::Config) -> (String, String) {
        (T::name(cfg), T::Output::name(cfg))
    }
    fn export_types(&self, ex: &mut crate::bindings::Exporter<'_>) {
        ex.export::<T>();
        ex.export::<T::Output>();
    }
    fn run<'a>(&'a self, hub: &'a Arc<Hub>, caller: &'a Caller, input: Value) -> BoxFuture<'a, OpResult> {
        Box::pin(async move {
            let input: T = serde_json::from_value(if input.is_null() { json!({}) } else { input })
                .map_err(|e| OpError::BadRequest(e.to_string()))?;
            to_json(&input.call(hub, caller).await?)
        })
    }
}

/// Every op, in the order agents see them.
pub static OPS: &[&dyn AnyOp] = &[
    &op::<SendMessage>(),
    &op::<EmitEvent>(),
    &op::<SetTimer>(),
    &op::<CancelTimer>(),
    &op::<ListTimers>(),
    &op::<SetChatTitle>(),
    &op::<SpawnAgent>(),
    &op::<ListChats>(),
    &op::<ProposeChange>(),
    &op::<PreviewChange>(),
    &op::<GetProposal>(),
    &op::<SubmitReview>(),
    &op::<secret_requests::SecretRequest>(),
    &op::<ListRequests>(),
    &op::<CancelRequest>(),
    &op::<ArchiveReady>(),
    &op::<ListEventTypes>(),
    &op::<ReadSession>(),
    &op::<memory::MemoryWrite>(),
    &op::<memory::MemoryAppend>(),
    &op::<memory::MemoryDelete>(),
    &op::<memory::MemoryRead>(),
    &op::<memory::MemoryList>(),
    &op::<memory::MemoryHistory>(),
    &op::<GetEvents>(),
    &op::<AskUser>(),
    &op::<GetRunLogs>(),
    &op::<GetIncident>(),
    &op::<GetIncidentHistory>(),
    &op::<ResolveIncident>(),
    &op::<StateGet>(),
    &op::<StateSet>(),
    &op::<health::Health>(),
];

/// The op called `name`.
pub fn find(name: &str) -> Option<&'static dyn AnyOp> {
    OPS.iter().copied().find(|o| o.name() == name)
}

/// The ops `caller` may call.
pub fn visible(caller: &Caller) -> impl Iterator<Item = &'static dyn AnyOp> + '_ {
    OPS.iter().copied().filter(|o| o.who().admits(caller))
}

/// Names of the ops that end the agent's turn ([`Op::ENDS_TURN`]).
pub fn turn_ending() -> impl Iterator<Item = &'static str> {
    OPS.iter().filter(|o| o.ends_turn()).map(|o| o.name())
}

/// The turn-ending ops `caller` may call, as `{name, description, parameters}`.
pub fn turn_ending_tools(caller: &Caller) -> Value {
    visible(caller)
        .filter(|o| o.ends_turn())
        .map(|o| json!({ "name": o.name(), "description": o.description(), "parameters": o.schema() }))
        .collect()
}

pub(crate) fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|e| json!({ "type": "object", "description": format!("schema error: {e}") }))
}

pub(crate) fn to_json<T: Serialize>(v: &T) -> Result<Value, OpError> {
    serde_json::to_value(v).map_err(|e| OpError::Hub(HubError::Io(std::io::Error::other(e))))
}

async fn limit(hub: &Hub, caller: &Caller, op: &str) -> Result<(), OpError> {
    let applied = hub.applied().await;
    let custom: Option<RateLimit> = match &caller.principal {
        Principal::Agent { instance: name, .. } => match hub.store.find_instance(name).await? {
            Some(i) => applied.defs.agents.get(&i.definition).and_then(|d| d.spec.limits.get(op).copied()),
            None => None,
        },
        Principal::Run { .. } => {
            caller.runnable.as_ref().and_then(|r| applied.defs.jobs.get(r)).and_then(|j| j.spec.limits.get(op).copied())
        }
        Principal::User => return Ok(()), // the user is not rate limited
    };
    hub.rate_limit(&caller.subject(), op, custom.unwrap_or_else(|| default_limit(op))).await.map_err(Into::into)
}

/// Run operation `name` for `caller`.
pub async fn call(hub: &Arc<Hub>, caller: &Caller, name: &str, input: Value) -> OpResult {
    let op = find(name).ok_or_else(|| OpError::Unknown(name.into()))?;
    if !op.who().admits(caller) {
        return Err(OpError::Forbidden(format!("{name} is not available to you")));
    }
    limit(hub, caller, name).await?;
    op.run(hub, caller, input).await
}

/// A time given as exactly one of an RFC 3339 `at` or a delay `after` from `now`.
fn parse_when(at: Option<&str>, after: Option<&str>, now: DateTime<Utc>) -> Result<DateTime<Utc>, OpError> {
    match (at, after) {
        (Some(at), None) => DateTime::parse_from_rfc3339(at)
            .map(|t| t.with_timezone(&Utc))
            .map_err(|e| OpError::BadRequest(format!("`at` must be RFC 3339: {e}"))),
        (None, Some(d)) => {
            let d: HumanDuration =
                d.parse().map_err(|e| OpError::BadRequest(format!("`in` must be a duration like 20m: {e}")))?;
            Ok(now + d.as_chrono())
        }
        _ => Err(OpError::BadRequest("give exactly one of `at` or `in`".into())),
    }
}
