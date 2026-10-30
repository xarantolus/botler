//! A job's persistent key-value state.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;
use crate::wire::Saved;

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct StateGet {
    pub key: String,
}

#[derive(Debug, Serialize, TS)]
pub struct StateValue {
    pub value: Option<String>,
}

impl Op for StateGet {
    const NAME: &'static str = "state_get";
    const DESCRIPTION: &'static str = "Read this job's persistent key-value state.";
    const WHO: Who = Who::Runs;
    type Output = StateValue;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<StateValue, OpError> {
        let owner = crate::trials::state_owner(hub, caller).await?;
        Ok(StateValue { value: hub.store.kv_get(&owner, &self.key).await? })
    }
}

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct StateSet {
    pub key: String,
    pub value: String,
}

impl Op for StateSet {
    const NAME: &'static str = "state_set";
    const DESCRIPTION: &'static str =
        "Write this job's persistent key-value state (cursors, dedup keys; up to 64 KiB per value).";
    const WHO: Who = Who::Runs;
    type Output = Saved;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<Saved, OpError> {
        let owner = crate::trials::state_owner(hub, caller).await?;
        hub.store.kv_set(&owner, &self.key, &self.value).await?;
        Ok(Saved { ok: true })
    }
}
