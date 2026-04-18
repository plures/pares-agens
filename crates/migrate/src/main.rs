//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN> [--model-url <URL>] [--model <MODEL>]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use reqwest::header::{HeaderMap, HeaderValue};

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::{Agent, Memory};
use pares_agens_core::auth::copilot::{CopilotAuth, CopilotModelClient};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::delegation::{broker::DelegationBroker, registry::AgentRegistry};
use pares_agens_core::memory::{
    embed::{EmbeddingProvider, MockEmbedder, OllamaEmbedder},
    entry::Exchange,
    store::{HostAdapterConfig, HostAdapterRecord, PluresDbStore},
    PluresLm,
};
use pares_agens_core::model::{
    ChatMessage as CoreChatMessage, ChatOptions, ModelClient, ToolDefinition, ToolDispatcher,
};
use pares_agens_core::procedure::{Procedure, ProcedureRegistry};
use pares_agens_core::Event;
use pares_agens_migrate::{migrate, openclaw};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::router::ModelRouter;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role, Tool};

#[derive(Debug, Parser)]
#[command(
    name = "pares-agens",
    version,
    about = "Pares Agens agent runtime CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

struct RouterModelClient {
    router: Arc<ModelRouter>,
    model: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CopilotAuthCache {
    oauth_token: String,
}

#[async_trait]
impl ModelClient for RouterModelClient {
    async fn complete(
        &self,
        messages: &[CoreChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<pares_agens_core::model::ModelCompletion, String> {
        let converted_messages = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                ChatMessage {
                    role,
                    content: Some(m.content.clone()),
                    tool_calls: m.tool_calls.clone().map(|calls| {
                        calls
                            .into_iter()
                            .map(|call| pares_models::types::ToolCall {
                                id: call.id,
                                kind: "function".into(),
                                function: pares_models::types::FunctionCall {
                                    name: call.name,
                                    arguments: call.arguments.to_string(),
                                },
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.clone(),
                    name: None,
                }
            })
            .collect();

        let mut request = ChatCompletionRequest::new(&self.model, converted_messages);
        if !tools.is_empty() {
            request.tools = Some(
                tools
                    .iter()
                    .map(|tool| {
                        Tool::function(
                            tool.name.clone(),
                            tool.description.clone(),
                            tool.parameters.clone(),
                        )
                    })
                    .collect(),
            );
        }
        if let Some(temp) = options.temperature {
            request.temperature = Some(temp as f32);
        }
        if options.logprobs {
            request.logprobs = Some(true);
        }

        let response = self
            .router
            .chat(&request)
            .await
            .map_err(|e| e.to_string())?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| "model returned no choices".to_string())?;

        let tool_calls = choice
            .message
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|call| pares_agens_core::model::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: serde_json::from_str(&call.function.arguments)
                    .unwrap_or(serde_json::Value::String(call.function.arguments)),
            })
            .collect();

        let logprobs = choice
            .logprobs
            .as_ref()
            .and_then(|lp| lp.content.as_ref())
            .map(|tokens| tokens.iter().filter_map(|t| t.logprob).collect::<Vec<_>>())
            .filter(|vals| !vals.is_empty());

        Ok(pares_agens_core::model::ModelCompletion {
            content: choice.message.content.clone(),
            tool_calls,
            logprobs,
        })
    }
}

struct ProcedureToolDispatcher {
    registry: Arc<ProcedureRegistry>,
}

#[async_trait]
impl ToolDispatcher for ProcedureToolDispatcher {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        let handler = match self.registry.matching(name).next() {
            Some(h) => h,
            None => return format!("no procedure registered for {name}"),
        };

        let event = Event::Message {
            id: Uuid::new_v4().to_string(),
            channel: "tool".into(),
            sender: "model".into(),
            content: arguments.to_string(),
        };

        let results = handler.execute(&event).await;
        for result in results {
            if let Event::ToolResult {
                content, is_error, ..
            } = result
            {
                if is_error {
                    return format!("tool error: {content}");
                }
                return content;
            }
        }

        format!("procedure {name} returned no tool result")
    }
}

struct PluresMemory {
    plures_lm: Arc<PluresLm>,
}

#[async_trait]
impl Memory for PluresMemory {
    async fn capture(&self, content: &str) -> Result<(), String> {
        let exchange = Exchange {
            user: content.to_string(),
            assistant: String::new(),
        };
        self.plures_lm
            .capture(&exchange)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, String> {
        let entries = self
            .plures_lm
            .recall(query, 5, &[])
            .await
            .map_err(|e| e.to_string())?;
        Ok(entries.into_iter().map(|e| e.content).collect())
    }
}

struct ReadFileProcedure;
struct WriteFileProcedure;
struct RunCommandProcedure;
struct EditFileProcedure;
struct ListDirectoryProcedure;
struct WebFetchProcedure;
struct WebSearchProcedure {
    brave_api_key: Option<String>,
}

#[async_trait]
impl Procedure for ReadFileProcedure {
    fn name(&self) -> &str {
        "read_file"
    }

    fn handles(&self) -> &str {
        "read_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("path").and_then(|v| v.as_str()) {
                        Some(path) => tokio::fs::read_to_string(path)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err("missing 'path'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "read_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WriteFileProcedure {
    fn name(&self) -> &str {
        "write_file"
    }

    fn handles(&self) -> &str {
        "write_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let path = args.get("path").and_then(|v| v.as_str());
                        let body = args.get("content").and_then(|v| v.as_str());
                        match (path, body) {
                            (Some(path), Some(body)) => tokio::fs::write(path, body)
                                .await
                                .map_err(|e| e.to_string())
                                .map(|_| "ok".to_string()),
                            _ => Err("missing 'path' or 'content'".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "write_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for RunCommandProcedure {
    fn name(&self) -> &str {
        "run_command"
    }

    fn handles(&self) -> &str {
        "run_command"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("command").and_then(|v| v.as_str()) {
                        Some(command) => {
                            let output = tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(command)
                                .output()
                                .await
                                .map_err(|e| e.to_string());
                            match output {
                                Ok(output) => {
                                    let stdout = String::from_utf8_lossy(&output.stdout);
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    let status = output
                                        .status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "signal".into());
                                    Ok(format!(
                                        "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                                        status, stdout, stderr
                                    ))
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'command'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "run_command".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for EditFileProcedure {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn handles(&self) -> &str {
        "edit_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let path = args.get("path").and_then(|v| v.as_str());
                        let old_text = args.get("old_text").and_then(|v| v.as_str());
                        let new_text = args.get("new_text").and_then(|v| v.as_str());
                        match (path, old_text, new_text) {
                            (Some(path), Some(old_text), Some(new_text)) => {
                                let body = tokio::fs::read_to_string(path)
                                    .await
                                    .map_err(|e| e.to_string());
                                match body {
                                    Ok(mut body) => {
                                        if let Some(idx) = body.find(old_text) {
                                            body.replace_range(idx..idx + old_text.len(), new_text);
                                            tokio::fs::write(path, body)
                                                .await
                                                .map_err(|e| e.to_string())
                                                .map(|_| "ok".to_string())
                                        } else {
                                            Err("old_text not found".into())
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            _ => Err("missing 'path', 'old_text', or 'new_text'".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "edit_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for ListDirectoryProcedure {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn handles(&self) -> &str {
        "list_directory"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("path").and_then(|v| v.as_str()) {
                        Some(path) => {
                            let entries =
                                tokio::fs::read_dir(path).await.map_err(|e| e.to_string());
                            match entries {
                                Ok(mut entries) => {
                                    let mut names = Vec::new();
                                    let mut error: Option<String> = None;
                                    loop {
                                        match entries.next_entry().await {
                                            Ok(Some(entry)) => {
                                                if let Some(name) = entry.file_name().to_str() {
                                                    names.push(name.to_string());
                                                }
                                            }
                                            Ok(None) => break,
                                            Err(e) => {
                                                error = Some(e.to_string());
                                                break;
                                            }
                                        }
                                    }
                                    if let Some(error) = error {
                                        Err(error)
                                    } else {
                                        Ok(names.join("\n"))
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'path'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "list_directory".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WebFetchProcedure {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn handles(&self) -> &str {
        "web_fetch"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("url").and_then(|v| v.as_str()) {
                        Some(url) => {
                            let response = reqwest::get(url).await.map_err(|e| e.to_string());
                            match response {
                                Ok(response) => {
                                    match response.text().await.map_err(|e| e.to_string()) {
                                        Ok(body) => {
                                            let truncated = if body.len() > 10_000 {
                                                body.chars().take(10_000).collect::<String>()
                                            } else {
                                                body
                                            };
                                            Ok(truncated)
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'url'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "web_fetch".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WebSearchProcedure {
    fn name(&self) -> &str {
        "web_search"
    }

    fn handles(&self) -> &str {
        "web_search"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let query = args.get("query").and_then(|v| v.as_str());
                        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(5);
                        let api_key = self.brave_api_key.clone();
                        match (query, api_key) {
                            (Some(query), Some(api_key)) => {
                                let mut headers = HeaderMap::new();
                                let token =
                                    HeaderValue::from_str(&api_key).map_err(|e| e.to_string());
                                match token {
                                    Ok(token) => {
                                        headers.insert("X-Subscription-Token", token);
                                        let client = reqwest::Client::new();
                                        let response = client
                                            .get("https://api.search.brave.com/res/v1/web/search")
                                            .headers(headers)
                                            .query(&[("q", query), ("count", &count.to_string())])
                                            .send()
                                            .await
                                            .map_err(|e| e.to_string());
                                        match response {
                                            Ok(response) => {
                                                let value: Result<serde_json::Value, String> =
                                                    response
                                                        .json()
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                match value {
                                                    Ok(value) => {
                                                        let results = value
                                                            .get("web")
                                                            .and_then(|v| v.get("results"))
                                                            .and_then(|v| v.as_array())
                                                            .map(|items| {
                                                                items
                                                                    .iter()
                                                                    .filter_map(|item| {
                                                                        Some(serde_json::json!({
                                                                            "title": item.get("title")?.as_str()?,
                                                                            "url": item.get("url")?.as_str()?,
                                                                            "description": item
                                                                                .get("description")
                                                                                .and_then(|d| d.as_str())
                                                                                .unwrap_or("")
                                                                        }))
                                                                    })
                                                                    .collect::<Vec<_>>()
                                                            })
                                                            .unwrap_or_default();
                                                        Ok(serde_json::json!(results).to_string())
                                                    }
                                                    Err(e) => Err(e),
                                                }
                                            }
                                            Err(e) => Err(e),
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            (None, _) => Err("missing 'query'".into()),
                            (_, None) => Err("missing BRAVE_API_KEY".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "web_search".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

fn parse_tool_args(raw: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Write a UTF-8 text file to disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace the first occurrence of old_text with new_text in a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_text": {"type": "string"},
                    "new_text": {"type": "string"}
                },
                "required": ["path", "old_text", "new_text"]
            }),
        },
        ToolDefinition {
            name: "list_directory".into(),
            description: "List files in a directory, one per line".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch a URL and return the response body (truncated)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "web_search".into(),
            description: "Search the web via Brave Search API".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "count": {"type": "integer"}
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "run_command".into(),
            description: "Run a shell command and return stdout/stderr".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        },
    ]
}

fn build_system_prompt(path: Option<PathBuf>) -> Result<String, String> {
    // Explicit path takes priority.
    if let Some(path) = path {
        return std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read system prompt {}: {e}", path.display()));
    }

    // Auto-discover: check $HOME/.pares-agens/SYSTEM-PROMPT.md
    if let Ok(home) = std::env::var("HOME") {
        let home_prompt = PathBuf::from(&home).join(".pares-agens/SYSTEM-PROMPT.md");
        if home_prompt.exists() {
            tracing::info!("Loading system prompt from {}", home_prompt.display());
            return std::fs::read_to_string(&home_prompt)
                .map_err(|e| format!("failed to read {}: {e}", home_prompt.display()));
        }
    }

    // Built-in fallback
    Ok("You are Pares Agens, an AI agent built on the plures technology stack. Be direct, use tools proactively, and push commits without asking.".to_string())
}

fn parse_sync_topic_key(raw: &str) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    let value = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if value.len() != 64 {
        return Err("sync topic key must be 64 hex characters (32 bytes)".to_string());
    }

    let mut key = [0u8; 32];
    for i in 0..32 {
        let pair = &value[(i * 2)..(i * 2 + 2)];
        key[i] = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("invalid hex byte at position {}: {pair}", i * 2))?;
    }
    Ok(key)
}

const ADAPTER_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1200);
const ADAPTER_DISCOVERY_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
struct SingleConnectionConflict {
    kind: String,
    connection_id: String,
    hosts: Vec<String>,
}

fn sanitize_hostname(raw: &str) -> String {
    let mut value = String::new();
    let mut prev_underscore = false;
    for c in raw.trim().chars() {
        let mapped = if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            c
        } else {
            '_'
        };
        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        value.push(mapped);
    }
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        value = "unknown-host".to_string();
    }
    value
}

fn current_hostname() -> String {
    if let Ok(value) = std::env::var("HOSTNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    if let Ok(value) = std::env::var("COMPUTERNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    #[cfg(unix)]
    if let Ok(value) = std::fs::read_to_string("/etc/hostname") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    "unknown-host".to_string()
}

async fn read_host_adapter_configs(
    store: &PluresDbStore,
    local_host: &str,
    sync_enabled: bool,
) -> Result<Vec<HostAdapterRecord>, String> {
    let mut records = store
        .list_host_adapters()
        .await
        .map_err(|e| format!("failed to list host adapter configs: {e}"))?;
    if !sync_enabled {
        return Ok(records);
    }

    let deadline = tokio::time::Instant::now() + ADAPTER_DISCOVERY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if records.iter().any(|record| record.host != local_host) {
            break;
        }
        tokio::time::sleep(ADAPTER_DISCOVERY_INTERVAL).await;
        records = store
            .list_host_adapters()
            .await
            .map_err(|e| format!("failed to list host adapter configs: {e}"))?;
    }
    Ok(records)
}

fn detect_single_connection_conflicts(
    local_host: &str,
    records: &[HostAdapterRecord],
) -> Vec<SingleConnectionConflict> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut owners: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for record in records {
        for adapter in &record.adapters {
            if !adapter.single_connection || adapter.connection_id.trim().is_empty() {
                continue;
            }
            owners
                .entry((adapter.kind.clone(), adapter.connection_id.clone()))
                .or_default()
                .insert(record.host.clone());
        }
    }

    owners
        .into_iter()
        .filter_map(|((kind, connection_id), hosts)| {
            if hosts.len() < 2 || !hosts.contains(local_host) {
                return None;
            }
            Some(SingleConnectionConflict {
                kind,
                connection_id,
                hosts: hosts.into_iter().collect(),
            })
        })
        .collect()
}

fn redact_connection_id(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return "********".to_string();
    }
    let start: String = value.chars().take(4).collect();
    let end: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}…{end}")
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Migrate data from an existing OpenClaw installation.
    Migrate {
        /// Path to the OpenClaw installation directory.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Directory to write migrated output files.
        #[arg(long, value_name = "PATH", default_value = "migration")]
        output: PathBuf,

        /// Simulate the migration without writing any files.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run the agent as a headless daemon with a channel adapter.
    Serve {
        /// Telegram bot token (from BotFather).
        #[arg(long, env = "PARES_TELEGRAM_TOKEN")]
        telegram_token: String,

        /// OpenAI-compatible API URL (GitHub Models or OpenAI compatible endpoint).
        #[arg(
            long,
            env = "PARES_MODEL_URL",
            default_value = "https://models.inference.ai.azure.com"
        )]
        model_url: String,

        /// Model name to use.
        #[arg(long, env = "PARES_MODEL", default_value = "gpt-4o")]
        model: String,

        /// Use GitHub Copilot device flow authentication.
        #[arg(long)]
        copilot: bool,

        /// Deep model name used for low-confidence escalation.
        #[arg(long, env = "PARES_DEEP_MODEL", default_value = "gpt-4.1")]
        deep_model: String,

        /// Deep model API URL (defaults to --model-url).
        #[arg(long, env = "PARES_DEEP_MODEL_URL")]
        deep_model_url: Option<String>,

        /// API key for the model provider.
        #[arg(long, env = "PARES_API_KEY")]
        api_key: Option<String>,

        /// Optional OpenAI-compatible embeddings endpoint.
        #[arg(long, env = "PARES_EMBED_URL")]
        embed_url: Option<String>,

        /// Embeddings model name.
        #[arg(long, env = "PARES_EMBED_MODEL", default_value = "nomic-embed-text")]
        embed_model: String,

        /// Path to a system prompt file.
        #[arg(long, value_name = "PATH")]
        system_prompt: Option<PathBuf>,

        /// Brave Search API key (falls back to BRAVE_API_KEY env var).
        #[arg(long, env = "BRAVE_API_KEY")]
        brave_api_key: Option<String>,

        /// 32-byte Hyperswarm sync topic key (hex) for multi-host replication.
        #[arg(long, env = "PARES_SYNC_TOPIC_KEY")]
        sync_topic_key: Option<String>,

        /// Shared SEA key (base64url-encoded SeaKeyPair JSON) required to decrypt sync payloads.
        #[arg(long, env = "PARES_SYNC_SHARED_KEY")]
        sync_shared_key: Option<String>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Migrate {
            from,
            output,
            dry_run,
        } => {
            let source = match from.or_else(openclaw::auto_detect) {
                Some(p) => p,
                None => {
                    eprintln!(
                        "No OpenClaw installation found. \
                         Use --from <PATH> to specify one."
                    );
                    std::process::exit(1);
                }
            };
            match migrate::run(&source, &output, dry_run) {
                Ok(report) => {
                    report.print();
                }
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Serve {
            telegram_token,
            model_url,
            model,
            copilot,
            deep_model,
            deep_model_url,
            api_key,
            embed_url,
            embed_model,
            system_prompt,
            brave_api_key,
            sync_topic_key,
            sync_shared_key,
        } => {
            tracing::info!("Starting Pares Agens daemon");
            let sync_enabled = sync_topic_key.is_some();

            let system_prompt = match build_system_prompt(system_prompt) {
                Ok(prompt) => prompt,
                Err(e) => {
                    tracing::error!("{e}");
                    std::process::exit(1);
                }
            };

            let mut model = model;
            let mut deep_model = deep_model;

            if copilot {
                if model == "gpt-4o" {
                    // Benchmark 2026-04-16: GPT-4.1 = 90% combined (GPQA+coding)
                    // at 3.7s avg. Fastest frontier model on Copilot Enterprise.
                    model = "gpt-4.1".into();
                }
                if deep_model == "gpt-4.1" {
                    // Benchmark 2026-04-16: Opus 4.6 = only model scoring 100%
                    // on BOTH GPQA Diamond AND coding. Worth the latency.
                    deep_model = "claude-opus-4.6".into();
                }
                tracing::info!("Copilot auth enabled");
                tracing::info!("Model: {model} (copilot)");
            } else {
                tracing::info!("Model: {model} @ {model_url}");
            }

            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

            let (model_client, deep_model_client): (Arc<dyn ModelClient>, Arc<dyn ModelClient>) =
                if copilot {
                    let auth_path = PathBuf::from(&home).join(".pares-agens/copilot-auth.json");
                    let cached = std::fs::read_to_string(&auth_path)
                        .ok()
                        .and_then(|raw| serde_json::from_str::<CopilotAuthCache>(&raw).ok());

                    let oauth_token = if let Some(cache) = cached {
                        cache.oauth_token
                    } else {
                        let (device_code, user_code, verification_uri) =
                            match CopilotAuth::device_flow_start().await {
                                Ok(response) => response,
                                Err(e) => {
                                    tracing::error!("copilot device flow failed: {e}");
                                    std::process::exit(1);
                                }
                            };

                        println!(
                            "Authorize Copilot: visit {verification_uri} and enter code {user_code}"
                        );

                        let oauth_token = match CopilotAuth::device_flow_poll(&device_code).await {
                            Ok(token) => token,
                            Err(e) => {
                                tracing::error!("copilot device flow polling failed: {e}");
                                std::process::exit(1);
                            }
                        };

                        if let Some(parent) = auth_path.parent() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                tracing::warn!("failed to create copilot auth dir: {e}");
                            }
                        }
                        if let Ok(serialized) = serde_json::to_string_pretty(&CopilotAuthCache {
                            oauth_token: oauth_token.clone(),
                        }) {
                            if let Err(e) = std::fs::write(&auth_path, serialized) {
                                tracing::warn!("failed to persist copilot auth: {e}");
                            }
                        }

                        oauth_token
                    };

                    let auth = CopilotAuth::new(oauth_token.clone());
                    let deep_auth = CopilotAuth::new(oauth_token);

                    (
                        Arc::new(CopilotModelClient::new(auth, model.clone())),
                        Arc::new(CopilotModelClient::new(deep_auth, deep_model.clone())),
                    )
                } else {
                    // Set up model router
                    let provider_config = ProviderConfig::new(&model_url, api_key.clone());
                    let router_config = RouterConfig::single("default", provider_config);
                    let model_router = Arc::new(ModelRouter::new(router_config));

                    let deep_model_url = deep_model_url.unwrap_or_else(|| model_url.clone());
                    let deep_provider_config =
                        ProviderConfig::new(&deep_model_url, api_key.clone());
                    let deep_router_config = RouterConfig::single("deep", deep_provider_config);
                    let deep_model_router = Arc::new(ModelRouter::new(deep_router_config));

                    (
                        Arc::new(RouterModelClient {
                            router: model_router.clone(),
                            model: model.clone(),
                        }) as Arc<dyn ModelClient>,
                        Arc::new(RouterModelClient {
                            router: deep_model_router.clone(),
                            model: deep_model.clone(),
                        }) as Arc<dyn ModelClient>,
                    )
                };

            // Set up PluresDB memory store + PluresLM (native)
            let memory_path = PathBuf::from(home).join(".pares-agens/memory");
            let store = if let Some(topic_key_raw) = sync_topic_key {
                let shared_key = match sync_shared_key {
                    Some(key) => key,
                    None => {
                        tracing::error!(
                            "--sync-topic-key requires --sync-shared-key (or PARES_SYNC_SHARED_KEY)"
                        );
                        std::process::exit(1);
                    }
                };
                let topic_key = match parse_sync_topic_key(&topic_key_raw) {
                    Ok(key) => key,
                    Err(e) => {
                        tracing::error!("invalid --sync-topic-key: {e}");
                        std::process::exit(1);
                    }
                };
                tracing::info!("PluresDB Hyperswarm sync enabled");
                match PluresDbStore::open_with_sync(&memory_path, &topic_key, &shared_key) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        tracing::error!("failed to open sync-enabled memory store: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                match PluresDbStore::open_with_embeddings(&memory_path) {
                    Ok(store) => {
                        tracing::info!(
                            "PluresDB with native fastembed (auto-embed on every write)"
                        );
                        Arc::new(store)
                    }
                    Err(e) => {
                        tracing::warn!("fastembed unavailable ({e}), falling back to basic store");
                        match PluresDbStore::open(&memory_path) {
                            Ok(store) => Arc::new(store),
                            Err(e2) => {
                                tracing::error!("failed to open memory store: {e2}");
                                std::process::exit(1);
                            }
                        }
                    }
                }
            };

            let hostname = current_hostname();
            if let Err(e) = store
                .set_host_adapters(
                    &hostname,
                    vec![HostAdapterConfig {
                        kind: "telegram".to_string(),
                        connection_id: telegram_token.clone(),
                        single_connection: true,
                    }],
                )
                .await
            {
                tracing::error!("failed to persist local adapter config for host {hostname}: {e}");
                std::process::exit(1);
            }

            let host_configs =
                match read_host_adapter_configs(&store, &hostname, sync_enabled).await {
                    Ok(configs) => configs,
                    Err(e) => {
                        tracing::error!("{e}");
                        std::process::exit(1);
                    }
                };

            let conflicts = detect_single_connection_conflicts(&hostname, &host_configs);
            for conflict in &conflicts {
                tracing::error!(
                    adapter = %conflict.kind,
                    connection = %redact_connection_id(&conflict.connection_id),
                    hosts = %conflict.hosts.join(", "),
                    "single-connection adapter conflict detected"
                );
            }
            if !conflicts.is_empty() {
                tracing::error!(
                    "headless mode: refusing to start adapter; keep this adapter enabled on only one host in the swarm (resolve ownership in setup wizard or by disabling Telegram on other hosts)"
                );
                std::process::exit(1);
            }

            let embedder: Box<dyn EmbeddingProvider> = match embed_url {
                Some(url) => Box::new(OllamaEmbedder::new(
                    url,
                    embed_model.clone(),
                    api_key.clone(),
                )),
                None => Box::new(MockEmbedder),
            };

            let plures_lm = Arc::new(PluresLm::new(
                Arc::clone(&store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
                embedder,
                128_000,
            ));

            // Keep a reference to the store for conversation turn persistence.
            let turn_store: Arc<dyn pares_agens_core::memory::store::MemoryStore> = store;

            let memory = Arc::new(PluresMemory {
                plures_lm: Arc::clone(&plures_lm),
            });
            let cerebellum = Cerebellum::new(CerebellumConfig::default());

            let brave_api_key = brave_api_key.or_else(|| std::env::var("BRAVE_API_KEY").ok());

            // Register native tool procedures
            let mut procedure_registry = ProcedureRegistry::new();
            procedure_registry.register(Box::new(ReadFileProcedure));
            procedure_registry.register(Box::new(WriteFileProcedure));
            procedure_registry.register(Box::new(EditFileProcedure));
            procedure_registry.register(Box::new(ListDirectoryProcedure));
            procedure_registry.register(Box::new(WebFetchProcedure));
            procedure_registry.register(Box::new(WebSearchProcedure { brave_api_key }));
            procedure_registry.register(Box::new(RunCommandProcedure));
            let procedure_registry = Arc::new(procedure_registry);

            let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
            });

            let mut registry = AgentRegistry::new();
            registry.register_builtins();
            let registry = Arc::new(registry);
            let delegation_broker = DelegationBroker::new(
                Arc::clone(&registry),
                Arc::clone(&model_client),
                Arc::clone(&tool_dispatcher),
            );

            let agent = Arc::new(
                Agent::with_cerebellum(memory, cerebellum, Arc::clone(&plures_lm))
                    .with_model(model_client, tool_dispatcher, system_prompt)
                    .with_deep_model(deep_model_client)
                    .with_delegation(delegation_broker)
                    .with_turn_store(turn_store),
            );

            // Set up Telegram adapter
            let config = TelegramConfig::new(telegram_token);
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            // Start the task scheduler in the background
            let scheduler = pares_agens_agenda::scheduler::Scheduler::new()
                .with_executor(std::sync::Arc::new(|cmd: String| {
                    tokio::spawn(async move {
                        match tokio::process::Command::new("sh")
                            .arg("-c")
                            .arg(&cmd)
                            .output()
                            .await
                        {
                            Ok(output) => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                if output.status.success() {
                                    stdout.to_string()
                                } else {
                                    format!("EXIT {}: {}\n{}", output.status, stdout, stderr)
                                }
                            }
                            Err(e) => format!("EXEC ERROR: {e}"),
                        }
                    })
                }));

            // Spawn scheduler loop
            tokio::spawn(async move {
                scheduler.start().await;
            });
            tracing::info!("Scheduler started");

            let agent_clone = agent.clone();

            if let Err(e) = adapter
                .run(move |event: Event| {
                    let agent = agent_clone.clone();
                    Box::pin(async move { agent.handle_event(event).await })
                })
                .await
            {
                tracing::error!("Telegram adapter exited: {e}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_single_connection_conflicts_for_local_host() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, "telegram");
        assert_eq!(
            conflicts[0].hosts,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_single_connections() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_local_conflicts() {
        let records = vec![
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "gamma".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }
}
