//! MCP server management — spawn, initialize, tool discovery, tool execution.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{error, info};

use mcp_client::protocol::{Tool as McpTool, ToolContent};
use mcp_client::transport::stdio::StdioTransport;
use mcp_client::McpClient;

use crate::state::{AppState, McpServerConfig};

/// Tool call result sent to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub server_name: String,
    pub tool_name: String,
    pub content: String,
    pub is_error: bool,
}

/// Discovered tool with its parent server name.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredTool {
    pub server_name: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Start all enabled MCP servers from settings and discover their tools.
pub async fn start_mcp_servers(state: &AppState) {
    let configs: Vec<McpServerConfig> = {
        let settings = state.settings.lock().await;
        settings
            .mcp_servers
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect()
    };

    if configs.is_empty() {
        info!("No MCP servers configured");
        return;
    }

    let mut clients = state.mcp_clients.lock().await;
    let mut all_tools: Vec<(String, McpTool)> = Vec::new();

    for config in &configs {
        match spawn_and_init(&config.command, &config.args).await {
            Ok((client, tools)) => {
                info!(
                    server = %config.name,
                    tool_count = tools.len(),
                    "MCP server connected"
                );
                for tool in &tools {
                    all_tools.push((config.name.clone(), tool.clone()));
                }
                clients.insert(config.name.clone(), client);
            }
            Err(e) => {
                error!(
                    server = %config.name,
                    command = %config.command,
                    error = %e,
                    "Failed to start MCP server"
                );
            }
        }
    }

    info!(total_tools = all_tools.len(), "MCP tool discovery complete");
    *state.mcp_tools.write().await = all_tools;
}

/// Spawn a single MCP server process, initialize it, and list tools.
async fn spawn_and_init(
    command: &str,
    args: &[String],
) -> Result<(McpClient, Vec<McpTool>), String> {
    let str_args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let transport = StdioTransport::spawn(command, &str_args)
        .await
        .map_err(|e| format!("spawn failed: {e}"))?;

    let mut client = McpClient::new(transport);
    client
        .initialize()
        .await
        .map_err(|e| format!("initialize failed: {e}"))?;

    let tools = client
        .list_tools()
        .await
        .map_err(|e| format!("list_tools failed: {e}"))?;

    Ok((client, tools))
}

/// Execute a tool call, routing to the correct MCP server.
pub async fn call_tool(
    state: &AppState,
    tool_name: &str,
    arguments: Option<Value>,
) -> ToolCallResult {
    let telemetry_enabled = {
        let settings = state.settings.lock().await;
        settings.telemetry.enabled
    };
    if telemetry_enabled {
        state.telemetry_service.record_tool_usage(tool_name).await;
    }

    // Find which server owns this tool
    let server_name = {
        let tools = state.mcp_tools.read().await;
        tools
            .iter()
            .find(|(_, t)| t.name == tool_name)
            .map(|(name, _)| name.clone())
    };

    let Some(server_name) = server_name else {
        return ToolCallResult {
            server_name: "unknown".into(),
            tool_name: tool_name.into(),
            content: format!("No MCP server provides tool '{tool_name}'"),
            is_error: true,
        };
    };

    let mut clients = state.mcp_clients.lock().await;
    let Some(client) = clients.get_mut(&server_name) else {
        return ToolCallResult {
            server_name: server_name.clone(),
            tool_name: tool_name.into(),
            content: format!("MCP server '{server_name}' not connected"),
            is_error: true,
        };
    };

    match client.call_tool(tool_name, arguments).await {
        Ok(result) => {
            let content = result
                .content
                .iter()
                .filter_map(|c| match c {
                    ToolContent::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");

            ToolCallResult {
                server_name,
                tool_name: tool_name.into(),
                content,
                is_error: result.is_error,
            }
        }
        Err(e) => {
            error!(
                server = %server_name,
                tool = %tool_name,
                error = %e,
                "MCP tool call failed"
            );
            ToolCallResult {
                server_name,
                tool_name: tool_name.into(),
                content: format!("Tool call failed: {e}"),
                is_error: true,
            }
        }
    }
}

/// Get all discovered tools in a frontend-friendly format.
pub async fn list_discovered_tools(state: &AppState) -> Vec<DiscoveredTool> {
    let tools = state.mcp_tools.read().await;
    tools
        .iter()
        .map(|(server_name, tool)| DiscoveredTool {
            server_name: server_name.clone(),
            name: tool.name.clone(),
            description: tool.description.clone(),
            input_schema: serde_json::to_value(&tool.input_schema).unwrap_or_default(),
        })
        .collect()
}

/// Get tools in OpenAI function-calling format for injection into chat requests.
pub async fn openai_tools(state: &AppState) -> Vec<Value> {
    let tools = state.mcp_tools.read().await;
    tools
        .iter()
        .map(|(_, tool)| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description.as_deref().unwrap_or(""),
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect()
}

/// Restart all MCP servers (stop all, then start enabled).
pub async fn restart_mcp_servers(state: &AppState) {
    {
        let mut clients = state.mcp_clients.lock().await;
        clients.clear();
    }
    {
        let mut tools = state.mcp_tools.write().await;
        tools.clear();
    }
    start_mcp_servers(state).await;
}
