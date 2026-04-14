//! Event types consumed and emitted by the reactive event loop.

use serde::{Deserialize, Serialize};

/// All event types the executor can receive and dispatch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// An inbound message from a user or channel.
    Message {
        /// Unique message identifier.
        id: String,
        /// Channel the message arrived on (e.g. `"telegram"`, `"stdin"`).
        channel: String,
        /// Display name or ID of the sender.
        sender: String,
        /// Raw message text.
        content: String,
    },
    /// A scheduled timer fired.
    Timer {
        /// Unique timer identifier.
        id: String,
        /// Human-readable timer name used for handler lookup.
        name: String,
        /// Whether the timer should be rescheduled after firing.
        recurring: bool,
    },
    /// A key in PluresDB state changed.
    StateChange {
        /// The key whose value changed.
        key: String,
        /// Previous value, or `None` if the key was newly created.
        old_value: Option<serde_json::Value>,
        /// New value after the change.
        new_value: serde_json::Value,
    },
    /// A model finished generating a response.
    ModelResponse {
        /// ID of the originating request (correlates with a `Message` ID).
        request_id: String,
        /// Identifier of the model that produced the response.
        model: String,
        /// Generated response text.
        content: String,
    },
    /// A tool/MCP call returned a result.
    ToolResult {
        /// ID correlating this result with the originating tool call.
        tool_call_id: String,
        /// Name of the tool that was invoked.
        tool_name: String,
        /// Text output returned by the tool.
        content: String,
        /// `true` when the tool reported an error.
        is_error: bool,
    },
    /// A praxis pre-action gate blocked procedure execution.
    ///
    /// Emitted by [`Executor::dispatch`] when the [`PraxisGate`] rejects an
    /// action via its `check()` method.  This is the gate-based constraint path;
    /// for the store-based path see [`Event::ConstraintViolation`].
    /// The blocked procedure is skipped (not executed).
    PreActionConstraint {
        /// The action string that was checked (e.g. `"execute_procedure:foo"`).
        action: String,
        /// Human-readable reason returned by the gate.
        reason: String,
    },
    /// A praxis pre-action constraint blocked procedure execution.
    ///
    /// Emitted by [`Executor::dispatch`] when `on_action` returns
    /// [`ActionBlocked`][pares_agens_praxis::db::procedures::ActionBlocked].
    /// The `fix` field surfaces the remediation instructions from all
    /// violated constraints so the caller or logs can act on them.
    ConstraintViolation {
        /// Name of the procedure that was blocked.
        procedure: String,
        /// Event kind that triggered the dispatch attempt.
        event_kind: String,
        /// Human-readable summary of all blocking violations.
        message: String,
        /// Semicolon-separated remediation instructions from every violated constraint.
        fix: String,
    },
    /// A sub-agent task exceeded the size limits defined in ADR-0013.
    ///
    /// Emitted when a task description exceeds 200 words or an expected text
    /// output exceeds 2 000 characters.  The receiver should split the task
    /// into `suggested_splits` smaller sub-tasks before re-dispatching.
    TaskDecompositionRequired {
        /// Word count of the task description that triggered the violation.
        word_count: usize,
        /// Estimated character count of the expected text output, if the
        /// output type is `"text"` and it exceeded the limit.
        output_chars: Option<usize>,
        /// Suggested number of sub-tasks to decompose the original task into.
        suggested_splits: usize,
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
            Event::PreActionConstraint { .. } => "pre_action_constraint",
            Event::ConstraintViolation { .. } => "constraint_violation",
            Event::TaskDecompositionRequired { .. } => "task_decomposition_required",
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
