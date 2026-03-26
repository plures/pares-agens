//! MCP (Model Context Protocol) JSON-RPC 2.0 message types.
//!
//! Spec: <https://modelcontextprotocol.io/docs/concepts/architecture>

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── JSON-RPC 2.0 ────────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request.
///
/// Requests carry an `id`; notifications omit it (use [`JsonRpcRequest::notification`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// The JSON-RPC protocol version string (always `"2.0"`).
    pub jsonrpc: String,
    /// Present for requests; omitted for notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// The name of the JSON-RPC method to invoke.
    pub method: String,
    /// Optional parameters for the method.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Create a JSON-RPC 2.0 request (with an `id`).
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: Some(id.into()),
            method: method.into(),
            params,
        }
    }

    /// Create a JSON-RPC 2.0 notification (no `id`; no response expected).
    pub fn notification(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: None,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// The JSON-RPC protocol version string (always `"2.0"`).
    pub jsonrpc: String,
    /// The id echoed from the corresponding request.
    pub id: Value,
    /// The result payload on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error object on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// The numeric error code defined by the JSON-RPC spec or the application.
    pub code: i64,
    /// Human-readable description of the error.
    pub message: String,
    /// Optional additional error data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ── MCP initialize ───────────────────────────────────────────────────────────

/// Parameters for the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// The MCP protocol version this client supports.
    pub protocol_version: String,
    /// The capabilities advertised by this client.
    pub capabilities: ClientCapabilities,
    /// Human-readable information about this client.
    pub client_info: ClientInfo,
}

impl Default for InitializeParams {
    fn default() -> Self {
        Self {
            protocol_version: "2024-11-05".into(),
            capabilities: ClientCapabilities::default(),
            client_info: ClientInfo {
                name: "pares-agens".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }
}

/// Client capabilities advertised during `initialize`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// Whether the client supports root-listing and change notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapability>,
    /// Whether the client supports sampling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<Value>,
}

/// Indicates support for root listing and change notifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapability {
    /// Whether the client will emit `roots/listChanged` notifications.
    pub list_changed: bool,
}

/// Human-readable info about this client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// The name of the client application.
    pub name: String,
    /// The version string of the client application.
    pub version: String,
}

/// Result of a successful `initialize` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The MCP protocol version the server is using.
    pub protocol_version: String,
    /// The capabilities supported by the server.
    pub capabilities: ServerCapabilities,
    /// Human-readable information about the server.
    pub server_info: ServerInfo,
    /// Optional human-readable usage instructions for the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Server capabilities returned during `initialize`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    /// Whether the server exposes tools and supports `tools/list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
    /// Whether the server exposes resources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<Value>,
    /// Whether the server exposes prompts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<Value>,
}

/// Indicates that the server supports tool listing and invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    /// Whether the server will emit `tools/listChanged` notifications.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

/// Human-readable info about the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// The name of the server application.
    pub name: String,
    /// The version string of the server application.
    pub version: String,
}

// ── MCP tools/list ───────────────────────────────────────────────────────────

/// Optional parameters for a `tools/list` request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListToolsParams {
    /// Pagination cursor returned by a previous `tools/list` response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

/// Result of a `tools/list` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListToolsResult {
    /// The list of tools available on this page.
    pub tools: Vec<Tool>,
    /// Cursor to use when fetching the next page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// A single MCP tool definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// The unique name used to invoke this tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema describing the tool's input parameters.
    pub input_schema: ToolInputSchema,
}

/// JSON Schema for a tool's input parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInputSchema {
    /// The JSON Schema type (typically `"object"`).
    #[serde(rename = "type")]
    pub schema_type: String,
    /// The properties of the schema object, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
    /// List of required property names, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

// ── MCP tools/call ───────────────────────────────────────────────────────────

/// Parameters for a `tools/call` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    /// The name of the tool to invoke.
    pub name: String,
    /// Optional arguments to pass to the tool.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
}

/// Result of a `tools/call` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    /// The content items returned by the tool.
    pub content: Vec<ToolContent>,
    /// Whether the tool reported an error condition.
    #[serde(default)]
    pub is_error: bool,
}

/// Content returned by a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContent {
    /// Plain-text content returned by the tool.
    Text {
        /// The text string produced by the tool.
        text: String,
    },
    /// Base64-encoded image content returned by the tool.
    Image {
        /// Base64-encoded image data.
        data: String,
        /// The MIME type of the image (e.g. `"image/png"`).
        mime_type: String,
    },
    /// An embedded resource reference returned by the tool.
    Resource {
        /// The resource descriptor as a raw JSON value.
        resource: Value,
    },
}
