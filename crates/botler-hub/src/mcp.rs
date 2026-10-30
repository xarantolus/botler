//! The hub's MCP server: a thin adapter from MCP tool calls to [`crate::ops`].
//!
//! Stateless: every request is authenticated by its bearer token (checked by the HTTP layer,
//! which puts the [`Principal`] into the request extensions), so a hub restart loses nothing.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::Value;

use crate::auth::Principal;
use crate::hub::Hub;
use crate::ops::{self, Caller, OpError};

#[derive(Clone)]
pub struct BotlerMcp {
    pub hub: Arc<Hub>,
}

fn principal(ctx: &RequestContext<RoleServer>) -> Result<Principal, ErrorData> {
    ctx.extensions
        .get::<http::request::Parts>()
        .and_then(|p| p.extensions.get::<Principal>().cloned())
        .ok_or_else(|| ErrorData::invalid_request("missing authentication", None))
}

impl BotlerMcp {
    async fn caller(&self, ctx: &RequestContext<RoleServer>) -> Result<Caller, ErrorData> {
        Caller::resolve(&self.hub, principal(ctx)?).await.map_err(|e| ErrorData::invalid_request(e.to_string(), None))
    }
}

impl ServerHandler for BotlerMcp {
    fn get_info(&self) -> rmcp::model::ServerConfig {
        let mut info = rmcp::model::ServerConfig::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info.name = "botler".into();
        info.server_info.version = env!("CARGO_PKG_VERSION").into();
        info.instructions = Some(
            "The botler hub. Use these tools to message other agents, set timers, ask the user, and inspect jobs and incidents."
                .into(),
        );
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let caller = self.caller(&ctx).await?;
        let tools = ops::visible(&caller)
            .map(|o| {
                let schema = match o.schema() {
                    Value::Object(m) => m,
                    other => {
                        tracing::error!(op = o.name(), schema = %other, "tool schema is not an object");
                        serde_json::Map::new()
                    }
                };
                Tool::new(o.name(), o.description(), Arc::new(schema))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let caller = self.caller(&ctx).await?;
        let input = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let result = match ops::call(&self.hub, &caller, &request.name, input).await {
            Ok(v) => {
                let text = match v {
                    Value::String(s) => s,
                    other => serde_json::to_string_pretty(&other)
                        .map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
                };
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
            // Errors go back to the model as tool errors so it can correct itself.
            Err(e) => {
                if matches!(e, OpError::Hub(_)) {
                    tracing::warn!(tool = %request.name, caller = ?caller.principal, error = %e, "tool call failed");
                }
                CallToolResult::error(vec![ContentBlock::text(e.to_string())])
            }
        };
        Ok(result.into())
    }
}
