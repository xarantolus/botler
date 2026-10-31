//! SQLite runtime state for the hub.
//!
//! The hub is the only writer. Every table holds facts that cannot be derived from the monorepo;
//! anything derivable (next fire times, rollups, search indexes) is recomputed, never stored.

mod breakers;
mod channels;
mod deliveries;
mod dialogs;
mod events;
mod grants;
mod incidents;
mod instances;
mod kv;
mod meta;
mod proposals;
mod runs;
mod schedules;
mod secrets;
mod timers;
mod trials;
mod usage;
mod waits;

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};

pub use breakers::Breaker;
pub use channels::{ChannelBinding, ChannelStranger, OutboxItem};
pub use deliveries::{Delivery, DeliveryStatus};
pub use dialogs::{Dialog, DialogKind, DialogStatus, NewDialog, Priority};
pub use events::EventQuery;
pub use grants::{Grant, NewSubscription};
pub use incidents::{Incident, IncidentStatus};
pub use instances::{ArchiveRecord, Handoff, HandoverReport, Instance, InstanceState, InterruptedTurn, NewInstance};
pub use proposals::{NewProposal, Proposal, ProposalDecision, ProposalKind, ProposalStage, ProposalStatus};
pub use runs::{NewRun, Run, RunKind, RunStatus};
pub use schedules::ScheduleState;
pub use secrets::SecretKey;
pub use timers::{NewTimer, Timer, TimerStatus};
pub use trials::{MAX_EFFECTS as MAX_TRIAL_EFFECTS, Trial};
pub use usage::UsageTotals;
pub use waits::{ReplyExpectation, TurnRetry};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("corrupt row in {table}: {detail}")]
    Corrupt { table: &'static str, detail: String },
    #[error("{kind} {id} not found")]
    NotFound { kind: &'static str, id: String },
    #[error("instance {0} already exists")]
    InstanceExists(String),
    #[error("the name {0} is already used by another chat")]
    SlugTaken(String),
    #[error("dialog is {status}{}", reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    DialogClosed { status: DialogStatus, reason: Option<String> },
    #[error("answer {answer:?} is not one of {allowed:?}")]
    AnswerNotAllowed { answer: String, allowed: Vec<String> },
    #[error("value for {key} exceeds {limit} bytes")]
    ValueTooLarge { key: String, limit: usize },
    #[error("cannot encode {what}: {source}")]
    Encode { what: &'static str, source: serde_json::Error },
    #[error("secret key file {path}: {source}")]
    KeyFile { path: std::path::PathBuf, source: std::io::Error },
    #[error("secret key file {0} does not hold a 32-byte key")]
    KeyLength(std::path::PathBuf),
    #[error("encrypting secret {0} failed")]
    Encrypt(String),
    #[error("cannot decrypt secret {0}: wrong key or tampered value")]
    Decrypt(String),
    #[error("secret {0} is not valid UTF-8")]
    NotUtf8(String),
}

pub(crate) fn not_found(kind: &'static str, id: &str) -> Error {
    Error::NotFound { kind, id: id.to_string() }
}

pub(crate) fn encode<T: serde::Serialize + ?Sized>(what: &'static str, v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(|source| Error::Encode { what, source })
}

pub type Result<T> = std::result::Result<T, Error>;

/// Handle to the runtime database. Cheap to clone.
#[derive(Clone, Debug)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if needed) and migrate the database at `path`.
    pub async fn open(path: &Path) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(10));
        let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
        Self::init(pool).await
    }

    /// A private in-memory database (tests).
    pub async fn memory() -> Result<Self> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        // One connection: each in-memory connection would otherwise be its own database.
        let pool = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Consistent online backup to `dest` (a new file).
    pub async fn backup(&self, dest: &Path) -> Result<()> {
        sqlx::query("VACUUM INTO ?").bind(dest.display().to_string()).execute(&self.pool).await?;
        Ok(())
    }
}

/// Fixed-width, sortable timestamp text.
pub(crate) fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub(crate) fn parse_ts(table: &'static str, s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| Error::Corrupt { table, detail: format!("bad timestamp {s:?}: {e}") })
}

pub(crate) fn parse_opt_ts(table: &'static str, s: Option<String>) -> Result<Option<DateTime<Utc>>> {
    s.map(|s| parse_ts(table, &s)).transpose()
}

pub(crate) fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// String-backed enums stored in TEXT columns.
macro_rules! text_enum {
    ($name:ident { $($(#[$attr:meta])* $variant:ident = $text:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, ts_rs::TS)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$attr])* $variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            pub(crate) fn parse(table: &'static str, s: &str) -> $crate::Result<Self> {
                match s {
                    $($text => Ok(Self::$variant),)+
                    other => Err($crate::Error::Corrupt { table, detail: format!("unknown {} {other:?}", stringify!($name)) }),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use text_enum;
