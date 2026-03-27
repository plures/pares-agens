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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── JsonRpcRequest ───────────────────────────────────────────────────────

    #[test]
    fn jsonrpc_request_new_sets_id_and_method() {
        let req = JsonRpcRequest::new(1u64, "tools/list", None);
        assert_eq!(req.jsonrpc, "2.0");
        assert_eq!(req.id, Some(json!(1u64)));
        assert_eq!(req.method, "tools/list");
        assert!(req.params.is_none());
    }

    #[test]
    fn jsonrpc_request_new_with_params() {
        let params = json!({"cursor": "next-page"});
        let req = JsonRpcRequest::new(2u64, "tools/list", Some(params.clone()));
        assert_eq!(req.params, Some(params));
    }

    #[test]
    fn jsonrpc_notification_has_no_id() {
        let notif = JsonRpcRequest::notification("notifications/initialized", None);
        assert!(notif.id.is_none());
        assert_eq!(notif.method, "notifications/initialized");
    }

    #[test]
    fn jsonrpc_request_serde_roundtrip() {
        let req = JsonRpcRequest::new("req-1", "initialize", Some(json!({"key": "val"})));
        let json = serde_json::to_string(&req).unwrap();
        let decoded: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.method, req.method);
        assert_eq!(decoded.id, req.id);
    }

    #[test]
    fn jsonrpc_notification_omits_id_field_when_serialized() {
        let notif = JsonRpcRequest::notification("ping", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(
            !json.contains("\"id\""),
            "id should be absent in notification JSON"
        );
    }

    // ── JsonRpcResponse ──────────────────────────────────────────────────────

    #[test]
    fn jsonrpc_response_success_serde_roundtrip() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: json!(1u64),
            result: Some(json!({"tools": []})),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.id, json!(1u64));
        assert!(decoded.result.is_some());
        assert!(decoded.error.is_none());
    }

    #[test]
    fn jsonrpc_response_error_serde_roundtrip() {
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id: json!(42u64),
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "Method not found".into(),
                data: None,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        let err = decoded.error.unwrap();
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "Method not found");
    }

    #[test]
    fn jsonrpc_error_with_data_roundtrip() {
        let err = JsonRpcError {
            code: -32700,
            message: "Parse error".into(),
            data: Some(json!({"detail": "unexpected token"})),
        };
        let json = serde_json::to_string(&err).unwrap();
        let decoded: JsonRpcError = serde_json::from_str(&json).unwrap();
        assert!(decoded.data.is_some());
    }

    // ── InitializeParams ─────────────────────────────────────────────────────

    #[test]
    fn initialize_params_default_has_correct_protocol_version() {
        let params = InitializeParams::default();
        assert_eq!(params.protocol_version, "2024-11-05");
        assert_eq!(params.client_info.name, "pares-agens");
    }

    #[test]
    fn initialize_params_serde_roundtrip() {
        let params = InitializeParams::default();
        let json = serde_json::to_string(&params).unwrap();
        let decoded: InitializeParams = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.protocol_version, params.protocol_version);
        assert_eq!(decoded.client_info.name, params.client_info.name);
    }

    // ── Tool ─────────────────────────────────────────────────────────────────

    #[test]
    fn tool_serde_roundtrip_with_description() {
        let tool = Tool {
            name: "web_search".into(),
            description: Some("Search the web".into()),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({"q": {"type": "string"}})),
                required: Some(vec!["q".into()]),
            },
        };
        let json = serde_json::to_string(&tool).unwrap();
        let decoded: Tool = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "web_search");
        assert_eq!(decoded.description.as_deref(), Some("Search the web"));
        assert_eq!(
            decoded.input_schema.required.as_deref(),
            Some(["q".to_string()].as_slice())
        );
    }

    #[test]
    fn tool_serde_roundtrip_without_optional_fields() {
        let tool = Tool {
            name: "ping".into(),
            description: None,
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: None,
                required: None,
            },
        };
        let json = serde_json::to_string(&tool).unwrap();
        assert!(!json.contains("\"description\""));
        assert!(!json.contains("\"required\""));
        let decoded: Tool = serde_json::from_str(&json).unwrap();
        assert!(decoded.description.is_none());
    }

    // ── CallToolResult / ToolContent ─────────────────────────────────────────

    #[test]
    fn call_tool_result_text_content_serde_roundtrip() {
        let result = CallToolResult {
            content: vec![ToolContent::Text { text: "42".into() }],
            is_error: false,
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: CallToolResult = serde_json::from_str(&json).unwrap();
        assert!(!decoded.is_error);
        match &decoded.content[0] {
            ToolContent::Text { text } => assert_eq!(text, "42"),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn call_tool_result_error_flag_roundtrip() {
        let result = CallToolResult {
            content: vec![ToolContent::Text {
                text: "error detail".into(),
            }],
            is_error: true,
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: CallToolResult = serde_json::from_str(&json).unwrap();
        assert!(decoded.is_error);
    }

    #[test]
    fn tool_content_image_variant_roundtrip() {
        let img = ToolContent::Image {
            data: "base64data".into(),
            mime_type: "image/png".into(),
        };
        let json = serde_json::to_string(&img).unwrap();
        let decoded: ToolContent = serde_json::from_str(&json).unwrap();
        match decoded {
            ToolContent::Image { data, mime_type } => {
                assert_eq!(data, "base64data");
                assert_eq!(mime_type, "image/png");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn tool_content_resource_variant_roundtrip() {
        let res = ToolContent::Resource {
            resource: json!({"uri": "file:///tmp/test"}),
        };
        let json = serde_json::to_string(&res).unwrap();
        let decoded: ToolContent = serde_json::from_str(&json).unwrap();
        match decoded {
            ToolContent::Resource { resource } => {
                assert_eq!(resource["uri"], "file:///tmp/test");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    // ── ListToolsParams / ListToolsResult ────────────────────────────────────

    #[test]
    fn list_tools_params_default_has_no_cursor() {
        let p = ListToolsParams::default();
        assert!(p.cursor.is_none());
    }

    #[test]
    fn list_tools_result_serde_roundtrip() {
        let result = ListToolsResult {
            tools: vec![],
            next_cursor: Some("cursor-abc".into()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let decoded: ListToolsResult = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.next_cursor.as_deref(), Some("cursor-abc"));
        assert!(decoded.tools.is_empty());
    }

    // ── CallToolParams ───────────────────────────────────────────────────────

    #[test]
    fn call_tool_params_serde_roundtrip() {
        let p = CallToolParams {
            name: "get_weather".into(),
            arguments: Some(json!({"city": "London"})),
        };
        let json = serde_json::to_string(&p).unwrap();
        let decoded: CallToolParams = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.name, "get_weather");
        assert_eq!(decoded.arguments.as_ref().unwrap()["city"], "London");
    }
}
