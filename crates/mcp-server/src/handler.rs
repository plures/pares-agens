//! Tool handler trait — dispatches tool calls from MCP clients.

use async_trait::async_trait;
use serde_json::Value;

/// Result of a tool invocation.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// The content returned by the tool (text).
    pub content: String,
    /// Whether the tool invocation was an error.
    pub is_error: bool,
}

/// Trait for handling tool calls from MCP clients.
///
/// Implementors provide the actual tool execution logic. The MCP server
/// delegates `tools/call` requests to this handler.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    /// List available tool definitions.
    ///
    /// Returns tool names, descriptions, and JSON schemas for parameters.
    async fn list_tools(&self) -> Vec<pares_radix_mcp_client::protocol::Tool>;

    /// Execute a named tool with the given arguments.
    async fn call_tool(&self, name: &str, arguments: Value) -> ToolResult;
}
