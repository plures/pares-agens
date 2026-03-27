//! Unit tests using an in-process mock MCP server.
//!
//! The mock transport communicates directly via an in-memory channel, so
//! no real process or network is involved.

use async_trait::async_trait;
use mcp_client::{
    error::Result,
    protocol::{
        CallToolResult, InitializeResult, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
        ListToolsResult, ServerCapabilities, ServerInfo, Tool, ToolContent, ToolInputSchema,
        ToolsCapability,
    },
    transport::Transport,
    McpClient,
};
use serde_json::{json, Value};

// ── Mock transport ────────────────────────────────────────────────────────────

/// A [`Transport`] that answers requests from a fixed handler function.
struct MockTransport<F: Fn(&JsonRpcRequest) -> JsonRpcResponse + Send + Sync> {
    handler: F,
}

impl<F: Fn(&JsonRpcRequest) -> JsonRpcResponse + Send + Sync> MockTransport<F> {
    fn new(handler: F) -> Self {
        Self { handler }
    }
}

#[async_trait]
impl<F: Fn(&JsonRpcRequest) -> JsonRpcResponse + Send + Sync> Transport for MockTransport<F> {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        Ok((self.handler)(&request))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Echo the request id back; notifications get Value::Null.
fn req_id(req: &JsonRpcRequest) -> Value {
    req.id.clone().unwrap_or(Value::Null)
}

fn ok_response(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

fn err_response(id: Value, code: i64, message: &str) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

fn sample_tool() -> Tool {
    Tool {
        name: "web_search".into(),
        description: Some("Search the web".into()),
        input_schema: ToolInputSchema {
            schema_type: "object".into(),
            properties: Some(json!({
                "query": { "type": "string", "description": "The search query" }
            })),
            required: Some(vec!["query".into()]),
        },
    }
}

fn make_mock_client() -> McpClient {
    McpClient::new(MockTransport::new(|req| {
        let id = req_id(req);
        match req.method.as_str() {
            "initialize" => ok_response(
                id,
                serde_json::to_value(InitializeResult {
                    protocol_version: "2024-11-05".into(),
                    capabilities: ServerCapabilities {
                        tools: Some(ToolsCapability {
                            list_changed: Some(true),
                        }),
                        ..Default::default()
                    },
                    server_info: ServerInfo {
                        name: "mock-server".into(),
                        version: "0.1.0".into(),
                    },
                    instructions: None,
                })
                .unwrap(),
            ),
            "notifications/initialized" => ok_response(id, json!({})),
            "tools/list" => ok_response(
                id,
                serde_json::to_value(ListToolsResult {
                    tools: vec![sample_tool()],
                    next_cursor: None,
                })
                .unwrap(),
            ),
            "tools/call" => ok_response(
                id,
                serde_json::to_value(CallToolResult {
                    content: vec![ToolContent::Text {
                        text: "Search results for: rust programming".into(),
                    }],
                    is_error: false,
                })
                .unwrap(),
            ),
            "ping" => ok_response(id, json!({})),
            other => err_response(id, -32601, &format!("method not found: {other}")),
        }
    }))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn initialize_returns_server_info() {
    let mut client = make_mock_client();
    let result = client
        .initialize()
        .await
        .expect("initialize should succeed");
    assert_eq!(result.server_info.name, "mock-server");
    assert_eq!(result.protocol_version, "2024-11-05");
    assert!(result.capabilities.tools.is_some());
}

#[tokio::test]
async fn list_tools_returns_expected_tools() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();

    let tools = client
        .list_tools()
        .await
        .expect("list_tools should succeed");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "web_search");
    assert_eq!(tools[0].description.as_deref(), Some("Search the web"));
}

#[tokio::test]
async fn list_tools_uses_cache_on_second_call() {
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut client = McpClient::new(MockTransport::new(move |req| {
        let id = req_id(req);
        match req.method.as_str() {
            "initialize" | "notifications/initialized" => ok_response(id, json!({})),
            "tools/list" => {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ok_response(
                    id,
                    serde_json::to_value(ListToolsResult {
                        tools: vec![sample_tool()],
                        next_cursor: None,
                    })
                    .unwrap(),
                )
            }
            _ => err_response(id, -32601, "not found"),
        }
    }));

    // Initialize produces a minimal response so we don't fail on it.
    let _ = client.initialize().await;

    client.list_tools().await.unwrap();
    client.list_tools().await.unwrap(); // should use cache
    assert_eq!(
        call_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "tools/list should only be called once"
    );
}

#[tokio::test]
async fn refresh_tools_bypasses_cache() {
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let mut client = McpClient::new(MockTransport::new(move |req| {
        let id = req_id(req);
        match req.method.as_str() {
            "initialize" | "notifications/initialized" => ok_response(id, json!({})),
            "tools/list" => {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ok_response(
                    id,
                    serde_json::to_value(ListToolsResult {
                        tools: vec![sample_tool()],
                        next_cursor: None,
                    })
                    .unwrap(),
                )
            }
            _ => err_response(id, -32601, "not found"),
        }
    }));

    let _ = client.initialize().await;
    client.list_tools().await.unwrap();
    client.refresh_tools().await.unwrap(); // bypasses cache
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn refresh_tools_paginates_all_pages() {
    // First call returns page 1 with a cursor; second call returns page 2 with no cursor.
    let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = call_count.clone();

    let tool_b = Tool {
        name: "tool_b".into(),
        description: Some("Tool B".into()),
        input_schema: ToolInputSchema {
            schema_type: "object".into(),
            properties: None,
            required: None,
        },
    };
    let tool_b = std::sync::Arc::new(tool_b);

    let mut client = McpClient::new(MockTransport::new(move |req| {
        let id = req_id(req);
        match req.method.as_str() {
            "initialize" | "notifications/initialized" => ok_response(id, json!({})),
            "tools/list" => {
                let n = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    // First page: one tool + cursor
                    ok_response(
                        id,
                        serde_json::to_value(ListToolsResult {
                            tools: vec![sample_tool()],
                            next_cursor: Some("page2".into()),
                        })
                        .unwrap(),
                    )
                } else {
                    // Second page: another tool, no cursor
                    ok_response(
                        id,
                        serde_json::to_value(ListToolsResult {
                            tools: vec![(*tool_b).clone()],
                            next_cursor: None,
                        })
                        .unwrap(),
                    )
                }
            }
            _ => err_response(id, -32601, "not found"),
        }
    }));

    let _ = client.initialize().await;
    let tools = client.refresh_tools().await.unwrap();

    assert_eq!(tools.len(), 2, "should collect tools from both pages");
    assert_eq!(tools[0].name, "web_search");
    assert_eq!(tools[1].name, "tool_b");
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn call_tool_returns_text_content() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();

    let result = client
        .call_tool("web_search", Some(json!({"query": "rust programming"})))
        .await
        .expect("call_tool should succeed");

    assert!(!result.is_error);
    assert_eq!(result.content.len(), 1);
    match &result.content[0] {
        mcp_client::protocol::ToolContent::Text { text } => {
            assert!(text.contains("rust programming"));
        }
        _ => panic!("expected text content"),
    }
}

#[tokio::test]
async fn openai_tools_produces_correct_format() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();

    let tools = client
        .openai_tools()
        .await
        .expect("openai_tools should succeed");
    let arr = tools.as_array().expect("should be array");
    assert_eq!(arr.len(), 1);

    let tool = &arr[0];
    assert_eq!(tool["type"], "function");
    assert_eq!(tool["function"]["name"], "web_search");
    assert_eq!(tool["function"]["description"], "Search the web");
    assert_eq!(tool["function"]["parameters"]["type"], "object");
    assert!(tool["function"]["parameters"]["properties"]["query"].is_object());
    assert_eq!(tool["function"]["parameters"]["required"][0], "query");
}

#[tokio::test]
async fn openai_tool_by_name_returns_single_tool() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();

    let tool = client
        .openai_tool("web_search")
        .await
        .expect("openai_tool should succeed");
    assert_eq!(tool["function"]["name"], "web_search");
}

#[tokio::test]
async fn openai_tool_not_found_returns_error() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();

    let err = client.openai_tool("nonexistent").await.unwrap_err();
    assert!(matches!(err, mcp_client::McpError::ToolNotFound(_)));
}

#[tokio::test]
async fn get_cached_tool_returns_none_before_list() {
    let client = make_mock_client();
    assert!(client.get_cached_tool("web_search").is_none());
}

#[tokio::test]
async fn get_cached_tool_returns_tool_after_list() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();
    client.list_tools().await.unwrap();

    let tool = client
        .get_cached_tool("web_search")
        .expect("should be cached");
    assert_eq!(tool.name, "web_search");
}

#[tokio::test]
async fn invalidate_cache_clears_tools() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();
    client.list_tools().await.unwrap();

    assert!(client.cached_tools().is_some());
    client.invalidate_tools_cache();
    assert!(client.cached_tools().is_none());
}

#[tokio::test]
async fn jsonrpc_error_propagated_correctly() {
    let mut client = McpClient::new(MockTransport::new(|req| {
        err_response(req_id(req), -32602, "invalid params")
    }));

    let err = client.initialize().await.unwrap_err();
    assert!(matches!(
        err,
        mcp_client::McpError::JsonRpc { code: -32602, .. }
    ));
}

#[tokio::test]
async fn response_id_mismatch_returns_error() {
    // Mock that always returns id=999 regardless of request id.
    let mut client = McpClient::new(MockTransport::new(|_req| {
        ok_response(
            json!(999),
            json!({"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"x","version":"0"}}),
        )
    }));

    let err = client.initialize().await.unwrap_err();
    assert!(matches!(err, mcp_client::McpError::UnexpectedResponse(_)));
}

#[tokio::test]
async fn ping_returns_true_on_success() {
    let mut client = make_mock_client();
    client.initialize().await.unwrap();
    assert!(client.ping().await);
}

// ── new_guarded license gate ──────────────────────────────────────────────────

#[test]
fn new_guarded_free_tier_returns_feature_not_available() {
    let license = pares_agens_core::license::License::free();
    let result = McpClient::new_guarded(
        MockTransport::new(|req| ok_response(req_id(req), json!({}))),
        &license,
    );
    assert!(
        matches!(
            result,
            Err(pares_agens_core::license::LicenseError::FeatureNotAvailable { .. })
        ),
        "Free tier should block MCP tool orchestration"
    );
}

#[test]
fn new_guarded_pro_tier_succeeds() {
    let license = pares_agens_core::license::License::pro(None);
    let result = McpClient::new_guarded(
        MockTransport::new(|req| ok_response(req_id(req), json!({}))),
        &license,
    );
    assert!(
        result.is_ok(),
        "Pro tier should allow MCP tool orchestration"
    );
}

#[test]
fn new_guarded_expired_pro_returns_expired() {
    let past = chrono::Utc::now() - chrono::TimeDelta::days(1);
    let license = pares_agens_core::license::License::pro(Some(past));
    let result = McpClient::new_guarded(
        MockTransport::new(|req| ok_response(req_id(req), json!({}))),
        &license,
    );
    assert!(
        matches!(
            result,
            Err(pares_agens_core::license::LicenseError::Expired)
        ),
        "Expired Pro license should be rejected"
    );
}
