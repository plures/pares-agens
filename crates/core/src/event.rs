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
        let msg = Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "alice".into(),
            content: "hello".into(),
        };
        assert_eq!(msg.kind(), "message");

        let timer = Event::Timer {
            id: "t1".into(),
            name: "daily".into(),
            recurring: true,
        };
        assert_eq!(timer.kind(), "timer");

        let sc = Event::StateChange {
            key: "mood".into(),
            old_value: None,
            new_value: serde_json::json!("happy"),
        };
        assert_eq!(sc.kind(), "state_change");

        let mr = Event::ModelResponse {
            request_id: "r1".into(),
            model: "qwen3".into(),
            content: "Sure!".into(),
        };
        assert_eq!(mr.kind(), "model_response");

        let tr = Event::ToolResult {
            tool_call_id: "tc1".into(),
            tool_name: "search".into(),
            content: "{}".into(),
            is_error: false,
        };
        assert_eq!(tr.kind(), "tool_result");
    }
}
