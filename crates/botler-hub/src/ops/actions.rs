//! Invoking approval-gated actions (see `actions.rs`).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{Caller, Op, OpError, Who};
use crate::hub::Hub;

#[derive(Debug, Deserialize, JsonSchema, TS)]
pub struct InvokeAction {
    /// The action (`jobs/<name>/job.toml` with `lifetime = "action"`).
    pub name: String,
    /// Its arguments, matching its `args` schema. The user approves exactly these.
    #[ts(type = "Record<string, unknown>")]
    pub args: Value,
}

#[derive(Debug, Serialize, TS)]
pub struct ActionRequested {
    pub proposal_id: String,
    pub note: String,
}

impl Op for InvokeAction {
    const NAME: &'static str = "invoke_action";
    const DESCRIPTION: &'static str = "Run an approval-gated action (something that reaches other people or uses hub-held secrets) with arguments. The user approves exactly these arguments; the outcome arrives as an `action.result` event.";
    const WHO: Who = Who::UserAndAgents;
    const ENDS_TURN: bool = true;
    type Output = ActionRequested;

    async fn call(self, hub: &Arc<Hub>, caller: &Caller) -> Result<ActionRequested, OpError> {
        let instance = caller.agent("only agents invoke actions")?;
        let p = crate::actions::invoke(hub, instance, &self.name, self.args).await?;
        Ok(ActionRequested {
            proposal_id: p.id,
            note: "Waiting for the user's approval; the result arrives as an action.result event. Stop until then."
                .into(),
        })
    }
}
