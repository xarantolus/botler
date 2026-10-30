//! Disk space: the storage overview and clean-ups. User only.

use std::sync::Arc;

use botler_core::duration::HumanDuration;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who, resolve_chat};
use crate::hub::Hub;
use crate::storage::{self, StorageOverview};

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct StorageUsage {
    /// Measure again instead of returning the last overview.
    #[serde(default)]
    pub refresh: Option<bool>,
}

impl Op for StorageUsage {
    const NAME: &'static str = "storage_usage";
    const DESCRIPTION: &'static str = "Disk use of the data directory by chat (home, workspace, session) and by kind (shared homes, images, exports, logs, search index, memory), largest first.";
    const WHO: Who = Who::User;
    type Output = StorageOverview;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<StorageOverview, OpError> {
        Ok(storage::overview(hub, self.refresh.unwrap_or(false)).await?)
    }
}

#[derive(Debug, Serialize, TS)]
pub struct Freed {
    pub freed_bytes: u64,
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct ClearChatCaches {
    pub chat: String,
}

impl Op for ClearChatCaches {
    const NAME: &'static str = "clear_chat_caches";
    const DESCRIPTION: &'static str =
        "Delete a sleeping chat's rebuildable files: gitignored files in its git checkouts and `~/.cache`.";
    const WHO: Who = Who::User;
    type Output = Freed;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Freed, OpError> {
        let chat = resolve_chat(hub, &self.chat).await?;
        Ok(Freed { freed_bytes: storage::clear_chat_caches(hub, &chat).await? })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct DeleteChat {
    /// An archived chat.
    pub chat: String,
}

impl Op for DeleteChat {
    const NAME: &'static str = "delete_chat";
    const DESCRIPTION: &'static str = "Delete an archived chat for good: its home, workspace, session and pending timers and deliveries. Its events stay in the journal.";
    const WHO: Who = Who::User;
    type Output = Freed;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Freed, OpError> {
        let chat = resolve_chat(hub, &self.chat).await?;
        Ok(Freed { freed_bytes: storage::delete_chat(hub, &chat).await? })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
#[ts(optional_fields)]
pub struct CollectGarbage {
    /// Delete run logs older than this (default `30d`).
    #[serde(default)]
    pub logs_older_than: Option<String>,
}

impl Op for CollectGarbage {
    const NAME: &'static str = "collect_garbage";
    const DESCRIPTION: &'static str =
        "Remove unused images and exports now, and run logs older than `logs_older_than` (default 30d).";
    const WHO: Who = Who::User;
    type Output = Freed;

    async fn call(self, hub: &Arc<Hub>, _: &Caller) -> Result<Freed, OpError> {
        let older: HumanDuration = self
            .logs_older_than
            .as_deref()
            .unwrap_or("30d")
            .parse()
            .map_err(|e| OpError::BadRequest(format!("`logs_older_than` must be a duration like 30d: {e}")))?;
        Ok(Freed { freed_bytes: storage::collect_garbage(hub, older.0).await? })
    }
}
