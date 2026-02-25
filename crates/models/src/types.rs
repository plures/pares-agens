//! OpenAI-compatible API request and response types.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Common
// ---------------------------------------------------------------------------

/// Role of a participant in a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A single message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Participant role.
    pub role: Role,
    /// Text content of the message (may be absent for pure tool-call messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls initiated by the assistant in this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// For `role: tool` messages, identifies which tool call this is the result of.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Optional name (used in older function-calling spec).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    /// Convenience constructor for a plain user or system message.
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool / function calling
// ---------------------------------------------------------------------------

/// An assistant-initiated call to a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this specific call (used to correlate tool results).
    pub id: String,
    /// Always `"function"` in the current spec.
    #[serde(rename = "type")]
    pub kind: String,
    /// The function being invoked.
    pub function: FunctionCall,
}

/// The function name and arguments from a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Name of the function.
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
}

/// A tool available to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// Always `"function"` for function-calling tools.
    #[serde(rename = "type")]
    pub kind: String,
    /// Schema of the callable function.
    pub function: FunctionDefinition,
}

impl Tool {
    /// Convenience constructor for a function tool.
    pub fn function(name: impl Into<String>, description: impl Into<String>, parameters: serde_json::Value) -> Self {
        Self {
            kind: "function".into(),
            function: FunctionDefinition {
                name: name.into(),
                description: Some(description.into()),
                parameters: Some(parameters),
            },
        }
    }
}

/// JSON-schema description of a callable function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Non-streaming request / response
// ---------------------------------------------------------------------------

/// Request body for `/v1/chat/completions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    /// Model identifier, e.g. `"gpt-4o"` or `"ai/mistral-nemo"`.
    pub model: String,
    /// Conversation history.
    pub messages: Vec<ChatMessage>,
    /// Tools the model may call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    /// Controls which tool (if any) the model must call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Maximum tokens to generate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0–2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Whether to stream the response.  Set by the client when streaming.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
}

impl ChatCompletionRequest {
    /// Minimal constructor for a simple conversation.
    pub fn new(model: impl Into<String>, messages: Vec<ChatMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            tools: None,
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            stream: None,
        }
    }
}

/// Successful response from `/v1/chat/completions` (non-streaming).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// One completion candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
}

/// Token-usage statistics returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ---------------------------------------------------------------------------
// Streaming types (SSE)
// ---------------------------------------------------------------------------

/// A single chunk from a streaming `/v1/chat/completions` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

/// Delta choice inside a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkChoice {
    pub index: u32,
    /// Incremental delta for this chunk.
    pub delta: DeltaMessage,
    pub finish_reason: Option<String>,
}

/// Partial message content sent in a streaming chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaMessage {
    /// Role, included only in the first chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
    /// Content fragment (may be empty string or absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool-call fragments (streamed incrementally).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
}
