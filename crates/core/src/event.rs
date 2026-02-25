//! Agent event types.

use serde::{Deserialize, Serialize};

/// Events that drive the Pares Agens reactive procedure executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An inbound message from a channel adapter.
    Message { content: String, channel: String },
    /// A scheduled timer fired.
    Timer { name: String },
    /// A PluresDB state key changed.
    StateChange { key: String, value: serde_json::Value },
    /// A model finished generating a response.
    ModelResponse { content: String },
    /// A tool/function call returned a result.
    ToolResult { tool_call_id: String, content: String },
}
