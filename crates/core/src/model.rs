use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── Chat message types ───────────────────────────────────────────────────────

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"`, `"assistant"`, or `"tool"`.
    pub role: String,
    /// The message text content.
    pub content: String,
    /// For `"tool"` role messages — the tool call ID this result belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls requested by the model (present on `"assistant"` messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    /// Create a system message (role `"system"`).
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Create a user message (role `"user"`).
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Create an assistant message (role `"assistant"`).
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// Create a tool-result message (role `"tool"`) correlating with
    /// `tool_call_id`.
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_call_id: Some(tool_call_id.into()),
            tool_calls: None,
        }
    }
}

// ── Tool types ───────────────────────────────────────────────────────────────

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique ID for correlating with the tool result.
    pub id: String,
    /// Name of the tool to call.
    pub name: String,
    /// JSON arguments for the tool.
    pub arguments: Value,
}

/// A tool definition provided to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name (must be unique across all tools).
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Value,
}

// ── Model completion ─────────────────────────────────────────────────────────

/// The result of a model completion request.
#[derive(Debug, Clone)]
pub struct ModelCompletion {
    /// Text content (present when the model produced a direct response).
    pub content: Option<String>,
    /// Tool calls requested by the model (empty when the model responded
    /// directly with text).
    pub tool_calls: Vec<ToolCall>,
    /// Log probabilities for each generated token (when supported).
    pub logprobs: Option<Vec<f64>>,
}

/// Optional settings for a chat completion.
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    /// Sampling temperature (0.0–2.0). `None` uses the provider default.
    pub temperature: Option<f64>,
    /// Request token logprobs when supported by the provider.
    pub logprobs: bool,
}

// ── Traits ───────────────────────────────────────────────────────────────────

/// Abstraction over an OpenAI-compatible language model endpoint.
///
/// In production this will call the Docker Model Runner or a remote API.
/// Tests use mock implementations.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Request a chat completion.
    ///
    /// Returns `None` content when the model makes tool calls instead of
    /// producing a direct text response.
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ModelCompletion, String>;
}

/// Abstraction over the MCP tool dispatcher.
///
/// In production this will be backed by the MCP client crate.  Tests use mock
/// implementations.
#[async_trait]
pub trait ToolDispatcher: Send + Sync {
    /// List all available tool definitions.
    async fn available_tools(&self) -> Vec<ToolDefinition>;

    /// Invoke a tool by name and return its result as a string.
    async fn call_tool(&self, name: &str, arguments: Value) -> String;
}
