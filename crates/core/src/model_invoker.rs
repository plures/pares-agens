//! ModelInvoker procedure — bridges the reactive event loop to the LLM.
//!
//! Handles `"message"` events by building conversation context, invoking the
//! model client with available tools, executing any tool calls, and emitting
//! a `"model_response"` event with the final text.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::event::Event;
use crate::model::{ChatMessage, ChatOptions, ModelClient, ToolDispatcher};
use crate::procedure::Procedure;

/// Maximum tool-call loop iterations before forcing a text response.
const MAX_TOOL_ITERATIONS: usize = 10;

/// Default system prompt used when none is configured.
const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a helpful assistant. Answer concisely and accurately.";

/// A procedure that invokes the language model in response to inbound messages.
///
/// Handles the full tool-call loop: if the model requests tool calls, they are
/// executed via the [`ToolDispatcher`] and the results are fed back until the
/// model produces a text response (or the iteration limit is reached).
pub struct ModelInvoker {
    model: Arc<dyn ModelClient>,
    tools: Arc<dyn ToolDispatcher>,
    system_prompt: String,
}

impl ModelInvoker {
    /// Create a new `ModelInvoker` with default system prompt.
    pub fn new(model: Arc<dyn ModelClient>, tools: Arc<dyn ToolDispatcher>) -> Self {
        Self {
            model,
            tools,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
        }
    }

    /// Create a new `ModelInvoker` with a custom system prompt.
    pub fn with_system_prompt(
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            model,
            tools,
            system_prompt: system_prompt.into(),
        }
    }
}

#[async_trait]
impl Procedure for ModelInvoker {
    fn name(&self) -> &str {
        "model_invoker"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let (id, _channel, _sender, content) = match event {
            Event::Message {
                id,
                channel,
                sender,
                content,
            } => (id, channel, sender, content),
            _ => return vec![],
        };

        // Build initial messages
        let mut messages = vec![
            ChatMessage::system(&self.system_prompt),
            ChatMessage::user(content),
        ];

        let tool_defs = self.tools.available_tools().await;
        let options = ChatOptions::default();

        // Tool-call loop
        for iteration in 0..MAX_TOOL_ITERATIONS {
            let completion = match self.model.complete(&messages, &tool_defs, &options).await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "model_invoker: model completion failed");
                    return vec![Event::ModelResponse {
                        request_id: id.clone(),
                        model: "unknown".into(),
                        content: format!("Error: {}", e),
                    }];
                }
            };

            // If model returned text content and no tool calls, we're done
            if completion.tool_calls.is_empty() {
                let text = completion.content.unwrap_or_default();
                debug!(iteration, "model_invoker: got text response");
                return vec![Event::ModelResponse {
                    request_id: id.clone(),
                    model: "unknown".into(),
                    content: text,
                }];
            }

            // Model wants tool calls — add assistant message with tool_calls
            messages.push(ChatMessage {
                role: "assistant".into(),
                content: completion.content.clone().unwrap_or_default(),
                tool_call_id: None,
                tool_calls: Some(completion.tool_calls.clone()),
            });

            // Execute each tool call and append results
            for tc in &completion.tool_calls {
                debug!(tool = %tc.name, call_id = %tc.id, "model_invoker: executing tool");
                let result = self.tools.call_tool(&tc.name, tc.arguments.clone()).await;
                messages.push(ChatMessage::tool_result(&tc.id, &result));
            }

            info!(iteration, tools = completion.tool_calls.len(), "model_invoker: tool iteration complete");
        }

        // Hit max iterations — return what we have
        warn!("model_invoker: hit max tool iterations ({})", MAX_TOOL_ITERATIONS);
        vec![Event::ModelResponse {
            request_id: id.clone(),
            model: "unknown".into(),
            content: "Error: maximum tool call iterations reached".into(),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelCompletion, ToolCall, ToolDefinition};
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ── Mock ModelClient ──────────────────────────────────────────────────────

    struct MockModelClient {
        responses: Mutex<Vec<Result<ModelCompletion, String>>>,
    }

    impl MockModelClient {
        fn new(responses: Vec<Result<ModelCompletion, String>>) -> Self {
            Self {
                responses: Mutex::new(responses),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                Ok(ModelCompletion {
                    content: Some("fallback".into()),
                    tool_calls: vec![],
                    logprobs: None,
                })
            } else {
                responses.remove(0)
            }
        }
    }

    // ── Mock ToolDispatcher ──────────────────────────────────────────────────

    struct MockToolDispatcher {
        tools: Vec<ToolDefinition>,
        call_count: AtomicUsize,
        results: Mutex<Vec<String>>,
    }

    impl MockToolDispatcher {
        fn new(tools: Vec<ToolDefinition>, results: Vec<String>) -> Self {
            Self {
                tools,
                call_count: AtomicUsize::new(0),
                results: Mutex::new(results),
            }
        }
    }

    #[async_trait]
    impl ToolDispatcher for MockToolDispatcher {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            self.tools.clone()
        }

        async fn call_tool(&self, _name: &str, _arguments: Value) -> String {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut results = self.results.lock().unwrap();
            if results.is_empty() {
                "ok".into()
            } else {
                results.remove(0)
            }
        }
    }

    fn make_message_event() -> Event {
        Event::Message {
            id: "msg-1".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: "What's the weather?".into(),
        }
    }

    // ── Test: simple text response ───────────────────────────────────────────

    #[tokio::test]
    async fn simple_text_response() {
        let model = Arc::new(MockModelClient::new(vec![Ok(ModelCompletion {
            content: Some("It's sunny!".into()),
            tool_calls: vec![],
            logprobs: None,
        })]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));

        let invoker = ModelInvoker::new(model, tools);
        let results = invoker.execute(&make_message_event()).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => assert_eq!(content, "It's sunny!"),
            _ => panic!("expected ModelResponse"),
        }
    }

    // ── Test: tool call then text response ───────────────────────────────────

    #[tokio::test]
    async fn tool_call_then_text_response() {
        let model = Arc::new(MockModelClient::new(vec![
            // First call: model requests a tool call
            Ok(ModelCompletion {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "tc-1".into(),
                    name: "get_weather".into(),
                    arguments: json!({"city": "Seattle"}),
                }],
                logprobs: None,
            }),
            // Second call: model returns text after seeing tool result
            Ok(ModelCompletion {
                content: Some("It's rainy in Seattle.".into()),
                tool_calls: vec![],
                logprobs: None,
            }),
        ]));
        let tools = Arc::new(MockToolDispatcher::new(
            vec![ToolDefinition {
                name: "get_weather".into(),
                description: "Get weather".into(),
                parameters: json!({}),
            }],
            vec!["Rainy, 52°F".into()],
        ));

        let invoker = ModelInvoker::new(model, tools.clone());
        let results = invoker.execute(&make_message_event()).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => {
                assert_eq!(content, "It's rainy in Seattle.")
            }
            _ => panic!("expected ModelResponse"),
        }
        assert_eq!(tools.call_count.load(Ordering::SeqCst), 1);
    }

    // ── Test: model error ────────────────────────────────────────────────────

    #[tokio::test]
    async fn model_error_returns_error_event() {
        let model = Arc::new(MockModelClient::new(vec![Err(
            "connection timeout".into(),
        )]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));

        let invoker = ModelInvoker::new(model, tools);
        let results = invoker.execute(&make_message_event()).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => {
                assert!(content.contains("connection timeout"));
            }
            _ => panic!("expected ModelResponse"),
        }
    }

    // ── Test: max iterations safeguard ───────────────────────────────────────

    #[tokio::test]
    async fn max_iterations_safeguard() {
        // Model always returns tool calls, never text
        let responses: Vec<Result<ModelCompletion, String>> = (0..MAX_TOOL_ITERATIONS)
            .map(|i| {
                Ok(ModelCompletion {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: format!("tc-{}", i),
                        name: "loop_tool".into(),
                        arguments: json!({}),
                    }],
                    logprobs: None,
                })
            })
            .collect();

        let model = Arc::new(MockModelClient::new(responses));
        let tools = Arc::new(MockToolDispatcher::new(
            vec![ToolDefinition {
                name: "loop_tool".into(),
                description: "loops".into(),
                parameters: json!({}),
            }],
            (0..MAX_TOOL_ITERATIONS).map(|_| "ok".into()).collect(),
        ));

        let invoker = ModelInvoker::new(model, tools.clone());
        let results = invoker.execute(&make_message_event()).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => {
                assert!(content.contains("maximum tool call iterations"));
            }
            _ => panic!("expected ModelResponse"),
        }
        assert_eq!(
            tools.call_count.load(Ordering::SeqCst),
            MAX_TOOL_ITERATIONS
        );
    }

    // ── Test: system prompt inclusion ────────────────────────────────────────

    #[tokio::test]
    async fn system_prompt_is_included() {
        let custom_prompt = "You are a pirate assistant.";
        // We'll check that the model receives the system prompt by inspecting
        // the messages in a custom mock
        struct CapturingModel {
            captured: Mutex<Vec<Vec<ChatMessage>>>,
        }

        #[async_trait]
        impl ModelClient for CapturingModel {
            async fn complete(
                &self,
                messages: &[ChatMessage],
                _tools: &[ToolDefinition],
                _options: &ChatOptions,
            ) -> Result<ModelCompletion, String> {
                self.captured.lock().unwrap().push(messages.to_vec());
                Ok(ModelCompletion {
                    content: Some("Arrr!".into()),
                    tool_calls: vec![],
                    logprobs: None,
                })
            }
        }

        let model = Arc::new(CapturingModel {
            captured: Mutex::new(vec![]),
        });
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));

        let invoker =
            ModelInvoker::with_system_prompt(model.clone(), tools, custom_prompt);
        invoker.execute(&make_message_event()).await;

        let captured = model.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0][0].role, "system");
        assert_eq!(captured[0][0].content, custom_prompt);
    }

    // ── Test: non-message events are ignored ─────────────────────────────────

    #[tokio::test]
    async fn non_message_event_is_ignored() {
        let model = Arc::new(MockModelClient::new(vec![]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));

        let invoker = ModelInvoker::new(model, tools);
        let timer = Event::Timer {
            id: "t-1".into(),
            name: "tick".into(),
            recurring: false,
        };
        let results = invoker.execute(&timer).await;
        assert!(results.is_empty());
    }
}
