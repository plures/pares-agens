use serde::{Deserialize, Serialize};

/// All event types the executor can receive and dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An inbound message from a user or channel.
    Message {
        id: String,
        channel: String,
        sender: String,
        content: String,
    },
    /// A scheduled timer fired.
    Timer {
        id: String,
        name: String,
        recurring: bool,
    },
    /// A key in PluresDB state changed.
    StateChange {
        key: String,
        old_value: Option<serde_json::Value>,
        new_value: serde_json::Value,
    },
    /// A model finished generating a response.
    ModelResponse {
        request_id: String,
        model: String,
        content: String,
    },
    /// A tool/MCP call returned a result.
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
    },
}

impl Event {
    /// Human-readable name of the event variant, used for logging and dispatch.
    pub fn kind(&self) -> &'static str {
        match self {
            Event::Message { .. } => "message",
            Event::Timer { .. } => "timer",
            Event::StateChange { .. } => "state_change",
            Event::ModelResponse { .. } => "model_response",
            Event::ToolResult { .. } => "tool_result",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_returns_correct_name() {
        let events = [
            (
                Event::Message {
                    id: "1".into(),
                    channel: "c".into(),
                    sender: "u".into(),
                    content: "hi".into(),
                },
                "message",
            ),
            (
                Event::Timer {
                    id: "t".into(),
                    name: "daily".into(),
                    recurring: true,
                },
                "timer",
            ),
            (
                Event::StateChange {
                    key: "mood".into(),
                    old_value: None,
                    new_value: serde_json::json!("happy"),
                },
                "state_change",
            ),
            (
                Event::ModelResponse {
                    request_id: "r".into(),
                    model: "qwen3".into(),
                    content: "ok".into(),
                },
                "model_response",
            ),
            (
                Event::ToolResult {
                    tool_call_id: "tc".into(),
                    tool_name: "search".into(),
                    content: "{}".into(),
                    is_error: false,
                },
                "tool_result",
            ),
        ];

        for (event, expected) in &events {
            assert_eq!(event.kind(), *expected);
        }
    }
}
