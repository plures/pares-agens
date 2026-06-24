//! High-level MCP client with tool discovery and caching.

use serde_json::{json, Value};
use tracing::debug;

use crate::{
    error::{McpError, Result},
    protocol::{
        CallToolParams, CallToolResult, InitializeParams, InitializeResult, JsonRpcRequest,
        ListToolsParams, ListToolsResult, Tool,
    },
    transport::Transport,
};

/// MCP client that wraps a [`Transport`] and provides high-level methods for
/// the MCP protocol.
///
/// Call [`McpClient::initialize`] once after construction to complete the
/// handshake before using any other methods.
pub struct McpClient {
    transport: Box<dyn Transport>,
    tools_cache: Option<Vec<Tool>>,
    next_id: u64,
}

impl McpClient {
    /// Create a new client from any [`Transport`].
    pub fn new(transport: impl Transport + 'static) -> Self {
        Self {
            transport: Box::new(transport),
            tools_cache: None,
            next_id: 1,
        }
    }

    /// Create a new client, gated behind the Pro license.
    ///
    /// MCP tool orchestration is a Pro feature.  Pass a valid Pro [`License`]
    /// to proceed; a Free-tier license returns
    /// [`pares_radix_core::license::LicenseError::FeatureNotAvailable`].
    ///
    /// [`License`]: pares_radix_core::license::License
    pub fn new_guarded(
        transport: impl Transport + 'static,
        license: &pares_radix_core::license::License,
    ) -> std::result::Result<Self, pares_radix_core::license::LicenseError> {
        license.check_feature(pares_radix_core::license::Feature::McpToolOrchestration)?;
        Ok(Self::new(transport))
    }

    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::Number(id.into())
    }

    fn make_request(&mut self, method: &str, params: Option<Value>) -> JsonRpcRequest {
        let id = self.next_id();
        JsonRpcRequest::new(id, method, params)
    }

    async fn call(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let request = self.make_request(method, params);
        let expected_id = request.id.clone();
        debug!(method, "sending MCP request");
        let response = self.transport.send(request).await?;

        // Validate that the server echoed back the same request id.
        if let Some(ref eid) = expected_id {
            if &response.id != eid {
                return Err(McpError::UnexpectedResponse(format!(
                    "response id {:?} does not match request id {:?}",
                    response.id, eid
                )));
            }
        }

        if let Some(err) = response.error {
            return Err(McpError::JsonRpc {
                code: err.code,
                message: err.message,
            });
        }

        response
            .result
            .ok_or_else(|| McpError::UnexpectedResponse("response has no result field".into()))
    }

    // ── Lifecycle ────────────────────────────────────────────────────────────

    /// Perform the MCP `initialize` handshake. Must be called once before any
    /// other method.
    pub async fn initialize(&mut self) -> Result<InitializeResult> {
        let params = serde_json::to_value(InitializeParams::default())?;
        let result = self.call("initialize", Some(params)).await?;
        let init: InitializeResult = serde_json::from_value(result)?;

        // Send the required `notifications/initialized` notification.
        // Notifications have no id and require no response.
        let notification = JsonRpcRequest::notification("notifications/initialized", None);
        let _ = self.transport.send(notification).await;

        Ok(init)
    }

    // ── Tools ────────────────────────────────────────────────────────────────

    /// List available tools, using the in-memory cache when available.
    pub async fn list_tools(&mut self) -> Result<Vec<Tool>> {
        if let Some(cached) = &self.tools_cache {
            return Ok(cached.clone());
        }
        self.refresh_tools().await
    }

    /// Bypass the cache and fetch tools directly from the server, following
    /// cursor-based pagination until all pages are consumed.
    pub async fn refresh_tools(&mut self) -> Result<Vec<Tool>> {
        let mut all_tools: Vec<Tool> = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            // Only include the cursor param when paginating; first call uses no params.
            let params = cursor
                .take()
                .map(|c| serde_json::to_value(ListToolsParams { cursor: Some(c) }))
                .transpose()?;
            let result = self.call("tools/list", params).await?;
            let page: ListToolsResult = serde_json::from_value(result)?;
            all_tools.extend(page.tools);
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }

        self.tools_cache = Some(all_tools.clone());
        Ok(all_tools)
    }

    /// Invalidate the tools cache so the next [`list_tools`] call fetches
    /// fresh data from the server.
    pub fn invalidate_tools_cache(&mut self) {
        self.tools_cache = None;
    }

    /// Call a tool by name with the given arguments.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Option<Value>,
    ) -> Result<CallToolResult> {
        let params = serde_json::to_value(CallToolParams {
            name: name.into(),
            arguments,
        })?;
        let result = self.call("tools/call", Some(params)).await?;
        let tool_result: CallToolResult = serde_json::from_value(result)?;
        Ok(tool_result)
    }

    /// Look up a cached tool by name (returns `None` if the cache is cold).
    pub fn get_cached_tool(&self, name: &str) -> Option<&Tool> {
        self.tools_cache
            .as_deref()
            .and_then(|tools| tools.iter().find(|t| t.name == name))
    }

    /// Return the raw tools cache without contacting the server.
    pub fn cached_tools(&self) -> Option<&[Tool]> {
        self.tools_cache.as_deref()
    }

    // ── Convenience ──────────────────────────────────────────────────────────

    /// Return all tools in OpenAI function-calling format.
    pub async fn openai_tools(&mut self) -> Result<Value> {
        let tools = self.list_tools().await?;
        Ok(crate::openai::tools_to_openai(&tools))
    }

    /// Return a single tool in OpenAI function-calling format.
    pub async fn openai_tool(&mut self, name: &str) -> Result<Value> {
        let tools = self.list_tools().await?;
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| McpError::ToolNotFound(name.into()))?;
        Ok(crate::openai::to_openai_function(tool))
    }

    /// Send a `ping` request and return `true` if the server responds.
    pub async fn ping(&mut self) -> bool {
        self.call("ping", Some(json!({}))).await.is_ok()
    }
}
