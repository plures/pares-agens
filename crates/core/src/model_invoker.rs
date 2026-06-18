//! ModelInvoker procedure — bridges the reactive event loop to the LLM.
//!
//! Handles `"message"` events by building conversation context, invoking the
//! model client with available tools, executing any tool calls, and emitting
//! a `"model_response"` event with the final text.
//!
//! # Headroom compression
//!
//! When constructed via [`ModelInvoker::with_headroom`], a pre-call
//! compression hook is activated.  Before each model completion, the current
//! message list is serialised into a [`StateStore`] key
//! (`headroom:input:<request_id>`).  A reactive `.px` pipeline (outside this
//! crate) may observe that key and write a compressed version to
//! `headroom:output:<request_id>`.  The invoker reads the output key; if
//! present it uses the compressed messages, otherwise the originals pass
//! through unchanged.
//!
//! The same gate applies to individual tool results that exceed
//! [`TOOL_RESULT_COMPRESS_BYTES`] bytes.

use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::event::Event;
use crate::model::{ChatMessage, ChatOptions, ModelClient, ToolDispatcher};
use crate::procedure::Procedure;
use crate::state::StateStore;

/// Maximum tool-call loop iterations before forcing a text response.
const MAX_TOOL_ITERATIONS: usize = 10;

/// Default system prompt used when none is configured.
const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a helpful assistant. Answer concisely and accurately.";

/// Approximate token threshold for compressing the whole message list.
///
/// Counted as `total_chars / 4` (the typical GPT-family char-per-token ratio).
/// When the rough token estimate exceeds this value *and* headroom compression
/// is enabled, the invoker triggers the `.px` pipeline.
const MESSAGE_TOKEN_THRESHOLD: usize = 500;

/// Byte threshold for compressing an individual tool result.
///
/// Tool outputs longer than this (in UTF-8 bytes) are forwarded to the
/// compression pipeline when headroom is enabled.
const TOOL_RESULT_COMPRESS_BYTES: usize = 2000;

// ---------------------------------------------------------------------------
// Token-count helper
// ---------------------------------------------------------------------------

/// Rough token estimate for a slice of messages.
///
/// Uses the commonly-cited heuristic of 1 token ≈ 4 characters.  This is
/// intentionally fast and slightly over-estimates; accuracy is not critical
/// since it only gates an optimisation pass, not correctness.
fn count_message_tokens(messages: &[ChatMessage]) -> usize {
    let total_chars: usize = messages.iter().map(|m| m.content.len()).sum();
    total_chars / 4
}

// ---------------------------------------------------------------------------
// Struct
// ---------------------------------------------------------------------------

/// A procedure that invokes the language model in response to inbound messages.
///
/// Handles the full tool-call loop: if the model requests tool calls, they are
/// executed via the [`ToolDispatcher`] and the results are fed back until the
/// model produces a text response (or the iteration limit is reached).
///
/// # Headroom compression
///
/// Construct with [`ModelInvoker::with_headroom`] to activate the pre-call
/// compression hook.  See module-level docs for the full protocol.
pub struct ModelInvoker {
    model: Arc<dyn ModelClient>,
    tools: Arc<dyn ToolDispatcher>,
    system_prompt: String,
    /// When `true`, the compression hook is active.
    headroom_enabled: bool,
    /// State store used to exchange data with the reactive `.px` compression
    /// pipeline.  Only needed when `headroom_enabled` is `true`; `None`
    /// effectively disables compression even if the flag is set.
    state_store: Option<Arc<dyn StateStore>>,
}

impl ModelInvoker {
    /// Create a new `ModelInvoker` with default system prompt and headroom
    /// compression disabled.
    pub fn new(model: Arc<dyn ModelClient>, tools: Arc<dyn ToolDispatcher>) -> Self {
        Self {
            model,
            tools,
            system_prompt: DEFAULT_SYSTEM_PROMPT.to_string(),
            headroom_enabled: false,
            state_store: None,
        }
    }

    /// Create a new `ModelInvoker` with a custom system prompt and headroom
    /// compression disabled.
    pub fn with_system_prompt(
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            model,
            tools,
            system_prompt: system_prompt.into(),
            headroom_enabled: false,
            state_store: None,
        }
    }

    /// Create a `ModelInvoker` with headroom compression enabled.
    ///
    /// The `state_store` is used to write raw messages and read back
    /// compressed output via the reactive `.px` pipeline.  If the pipeline
    /// produces no output within the same synchronous call, the original
    /// messages pass through unchanged.
    pub fn with_headroom(
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
        system_prompt: impl Into<String>,
        state_store: Arc<dyn StateStore>,
    ) -> Self {
        Self {
            model,
            tools,
            system_prompt: system_prompt.into(),
            headroom_enabled: true,
            state_store: Some(state_store),
        }
    }

    // -----------------------------------------------------------------------
    // Compression helpers
    // -----------------------------------------------------------------------

    /// Attempt to compress `messages` by writing them to the state store and
    /// reading back any output produced by the reactive `.px` pipeline.
    ///
    /// The keys used are:
    /// - **input**:  `headroom:input:<request_id>`  (written here)
    /// - **output**: `headroom:output:<request_id>` (read here, written by pipeline)
    ///
    /// If the store is absent, the pipeline writes nothing, or deserialisation
    /// fails, the original messages are returned unchanged.  All errors are
    /// logged at `warn` level and are non-fatal.
    async fn compress_messages(
        &self,
        request_id: &str,
        messages: &[ChatMessage],
    ) -> Vec<ChatMessage> {
        let store = match &self.state_store {
            Some(s) => s,
            None => {
                debug!(request_id, "headroom: no state store, skipping compression");
                return messages.to_vec();
            }
        };

        // 1. Serialise and write input to the store.
        let input_key = format!("headroom:input:{}", request_id);
        let messages_json = match serde_json::to_value(messages) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, request_id, "headroom: failed to serialise messages, skipping compression");
                return messages.to_vec();
            }
        };
        store.set(&input_key, messages_json).await;
        debug!(request_id, "headroom: wrote input messages to store");

        // 2. Read back compressed output (written synchronously by the pipeline
        //    during the `set` call above via reactive triggers, if present).
        let output_key = format!("headroom:output:{}", request_id);
        match store.get(&output_key).await {
            Some(compressed) => {
                match serde_json::from_value::<Vec<ChatMessage>>(compressed) {
                    Ok(compressed_msgs) => {
                        info!(
                            request_id,
                            original = messages.len(),
                            compressed = compressed_msgs.len(),
                            "headroom: using compressed messages from pipeline"
                        );
                        compressed_msgs
                    }
                    Err(e) => {
                        warn!(error = %e, request_id, "headroom: failed to deserialise compressed messages, using originals");
                        messages.to_vec()
                    }
                }
            }
            None => {
                debug!(request_id, "headroom: pipeline produced no output, using original messages");
                messages.to_vec()
            }
        }
    }

    /// Attempt to compress a tool result by writing it to the state store and
    /// reading back any output produced by the reactive `.px` pipeline.
    ///
    /// The keys used are:
    /// - **input**:  `headroom:tool-input:<request_id>:<tool_call_id>`
    /// - **output**: `headroom:tool-output:<request_id>:<tool_call_id>`
    ///
    /// If compression is unavailable or fails, the original result string is
    /// returned unchanged.  All errors are logged at `warn` level.
    async fn compress_tool_result(
        &self,
        request_id: &str,
        tool_call_id: &str,
        result: &str,
    ) -> String {
        let store = match &self.state_store {
            Some(s) => s,
            None => {
                return result.to_string();
            }
        };

        let input_key = format!("headroom:tool-input:{}:{}", request_id, tool_call_id);
        store
            .set(&input_key, serde_json::Value::String(result.to_string()))
            .await;
        debug!(request_id, tool_call_id, "headroom: wrote tool result to store");

        let output_key = format!("headroom:tool-output:{}:{}", request_id, tool_call_id);
        match store.get(&output_key).await {
            Some(serde_json::Value::String(compressed)) => {
                info!(
                    request_id,
                    tool_call_id,
                    original_bytes = result.len(),
                    compressed_bytes = compressed.len(),
                    "headroom: using compressed tool result from pipeline"
                );
                compressed
            }
            Some(_) => {
                warn!(
                    request_id,
                    tool_call_id,
                    "headroom: compressed tool result was not a string, using original"
                );
                result.to_string()
            }
            None => {
                debug!(request_id, tool_call_id, "headroom: pipeline produced no tool output, using original");
                result.to_string()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Procedure impl
// ---------------------------------------------------------------------------

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
            // === HEADROOM COMPRESSION (if enabled and over threshold) ===
            // Applied before each model call so that accumulated tool results
            // in `messages` are also candidates for compression.
            let messages_for_completion =
                if self.headroom_enabled
                    && count_message_tokens(&messages) > MESSAGE_TOKEN_THRESHOLD
                {
                    debug!(
                        request_id = %id,
                        iteration,
                        estimated_tokens = count_message_tokens(&messages),
                        "headroom: token threshold exceeded, attempting compression"
                    );
                    self.compress_messages(id, &messages).await
                } else {
                    messages.clone()
                };

            let completion =
                match self.model.complete(&messages_for_completion, &tool_defs, &options).await {
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
                let raw_result = self.tools.call_tool(&tc.name, tc.arguments.clone()).await;

                // === HEADROOM: compress large tool results ===
                let result = if self.headroom_enabled
                    && raw_result.len() > TOOL_RESULT_COMPRESS_BYTES
                {
                    debug!(
                        request_id = %id,
                        tool_call_id = %tc.id,
                        bytes = raw_result.len(),
                        "headroom: tool result over threshold, attempting compression"
                    );
                    self.compress_tool_result(id, &tc.id, &raw_result).await
                } else {
                    raw_result
                };

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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelCompletion, ToolCall, ToolDefinition};
    use crate::state::InMemoryStateStore;
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
            _messages: &[ChatMessage],
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

    /// Variant that captures all messages passed to `complete()` for inspection.
    struct CapturingModelClient {
        captured: Mutex<Vec<Vec<ChatMessage>>>,
        response: ModelCompletion,
    }

    impl CapturingModelClient {
        fn new(response: ModelCompletion) -> Self {
            Self {
                captured: Mutex::new(vec![]),
                response,
            }
        }
    }

    #[async_trait]
    impl ModelClient for CapturingModelClient {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            self.captured.lock().unwrap().push(messages.to_vec());
            Ok(self.response.clone())
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

    // ── Existing tests (unchanged behaviour) ─────────────────────────────────

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

    // ── New headroom tests ────────────────────────────────────────────────────

    /// Helper: build a `ModelInvoker` with headroom enabled + shared store.
    fn make_headroom_invoker(
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
        store: Arc<InMemoryStateStore>,
    ) -> ModelInvoker {
        ModelInvoker::with_headroom(model, tools, DEFAULT_SYSTEM_PROMPT, store)
    }

    // ── Test: headroom enabled but below threshold — no compression ───────────

    #[tokio::test]
    async fn headroom_below_threshold_no_compression() {
        // Short content — well under 500-token threshold
        let model = Arc::new(MockModelClient::new(vec![Ok(ModelCompletion {
            content: Some("Sunny!".into()),
            tool_calls: vec![],
            logprobs: None,
        })]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new());

        let invoker = make_headroom_invoker(model, tools, store.clone());
        let results = invoker.execute(&make_message_event()).await;

        // Should succeed normally
        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => assert_eq!(content, "Sunny!"),
            _ => panic!("expected ModelResponse"),
        }

        // No input key should have been written (below threshold)
        assert!(store.get("headroom:input:msg-1").await.is_none());
    }

    // ── Test: headroom enabled and over threshold — compression attempted ─────

    #[tokio::test]
    async fn headroom_above_threshold_writes_input_key() {
        // Craft a message event whose content pushes estimated tokens > 500.
        // 500 tokens × 4 chars/token = 2000 chars minimum for user content.
        // System prompt adds ~60 chars. Build a 2000-char user message.
        let long_content = "x".repeat(2000);
        let event = Event::Message {
            id: "msg-big".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: long_content.clone(),
        };

        let model = Arc::new(MockModelClient::new(vec![Ok(ModelCompletion {
            content: Some("Compressed response.".into()),
            tool_calls: vec![],
            logprobs: None,
        })]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new());

        let invoker = make_headroom_invoker(model, tools, store.clone());
        invoker.execute(&event).await;

        // The invoker should have written the input key
        assert!(store.get("headroom:input:msg-big").await.is_some());
    }

    // ── Test: pipeline provides compressed output — invoker uses it ───────────

    #[tokio::test]
    async fn headroom_uses_compressed_output_from_pipeline() {
        let long_content = "y".repeat(2000);
        let event = Event::Message {
            id: "msg-compress".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: long_content,
        };

        // The model will capture what messages it receives
        let model = Arc::new(CapturingModelClient::new(ModelCompletion {
            content: Some("ok".into()),
            tool_calls: vec![],
            logprobs: None,
        }));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new());

        // Pre-seed the store with a compressed output that the "pipeline" would
        // have written.  The compressed version is just two messages.
        let compressed_msgs = vec![
            ChatMessage::system("compressed system"),
            ChatMessage::user("compressed user summary"),
        ];
        store
            .set(
                "headroom:output:msg-compress",
                serde_json::to_value(&compressed_msgs).unwrap(),
            )
            .await;

        let invoker = make_headroom_invoker(model.clone(), tools, store);
        invoker.execute(&event).await;

        // The model should have received the 2-message compressed list
        let captured = model.captured.lock().unwrap();
        assert_eq!(captured.len(), 1, "model should have been called once");
        assert_eq!(
            captured[0].len(),
            2,
            "model should receive 2 compressed messages, not the original 3"
        );
        assert_eq!(captured[0][0].content, "compressed system");
        assert_eq!(captured[0][1].content, "compressed user summary");
    }

    // ── Test: pipeline produces nothing — original messages used ─────────────

    #[tokio::test]
    async fn headroom_fallback_when_pipeline_produces_no_output() {
        let long_content = "z".repeat(2000);
        let event = Event::Message {
            id: "msg-fallback".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: long_content,
        };

        let model = Arc::new(CapturingModelClient::new(ModelCompletion {
            content: Some("ok".into()),
            tool_calls: vec![],
            logprobs: None,
        }));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new()); // no pre-seeded output

        let invoker = make_headroom_invoker(model.clone(), tools, store);
        invoker.execute(&event).await;

        // Model should have received the original (system + user = 2) messages
        let captured = model.captured.lock().unwrap();
        assert_eq!(captured.len(), 1, "model should have been called once");
        // Original: system + user = 2 messages (no compression output)
        assert_eq!(captured[0].len(), 2, "model should receive original 2 messages");
    }

    // ── Test: headroom disabled — no store interaction ────────────────────────

    #[tokio::test]
    async fn headroom_disabled_no_store_interaction() {
        let long_content = "a".repeat(2000);
        let event = Event::Message {
            id: "msg-disabled".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: long_content,
        };

        let model = Arc::new(MockModelClient::new(vec![Ok(ModelCompletion {
            content: Some("ok".into()),
            tool_calls: vec![],
            logprobs: None,
        })]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));

        // headroom_enabled = false via ::new()
        let invoker = ModelInvoker::new(model, tools);
        let results = invoker.execute(&event).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => assert_eq!(content, "ok"),
            _ => panic!("expected ModelResponse"),
        }
    }

    // ── Test: tool result compression triggered when result is large ──────────

    #[tokio::test]
    async fn headroom_compresses_large_tool_result() {
        // A tool result bigger than TOOL_RESULT_COMPRESS_BYTES (2000)
        let big_result = "R".repeat(4000);

        let model = Arc::new(MockModelClient::new(vec![
            // First call: request tool
            Ok(ModelCompletion {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "tc-big".into(),
                    name: "big_tool".into(),
                    arguments: json!({}),
                }],
                logprobs: None,
            }),
            // Second call: return text after seeing tool result
            Ok(ModelCompletion {
                content: Some("done".into()),
                tool_calls: vec![],
                logprobs: None,
            }),
        ]));
        let tools = Arc::new(MockToolDispatcher::new(
            vec![ToolDefinition {
                name: "big_tool".into(),
                description: "returns a big payload".into(),
                parameters: json!({}),
            }],
            vec![big_result.clone()],
        ));
        let store = Arc::new(InMemoryStateStore::new());

        // Pre-seed a compressed tool output that the pipeline would write
        store
            .set(
                "headroom:tool-output:msg-1:tc-big",
                serde_json::Value::String("compressed summary".into()),
            )
            .await;

        let invoker = make_headroom_invoker(model, tools, store.clone());
        let results = invoker.execute(&make_message_event()).await;

        assert_eq!(results.len(), 1);
        match &results[0] {
            Event::ModelResponse { content, .. } => assert_eq!(content, "done"),
            _ => panic!("expected ModelResponse"),
        }

        // Verify the tool input key was written
        let written = store.get("headroom:tool-input:msg-1:tc-big").await;
        assert!(written.is_some(), "tool input key should have been written");
        match written.unwrap() {
            serde_json::Value::String(s) => assert_eq!(s, big_result),
            _ => panic!("expected string value"),
        }
    }

    // ── Test: tool result below threshold — no compression ────────────────────

    #[tokio::test]
    async fn headroom_small_tool_result_not_compressed() {
        let small_result = "small output".to_string();

        let model = Arc::new(MockModelClient::new(vec![
            Ok(ModelCompletion {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "tc-small".into(),
                    name: "small_tool".into(),
                    arguments: json!({}),
                }],
                logprobs: None,
            }),
            Ok(ModelCompletion {
                content: Some("done".into()),
                tool_calls: vec![],
                logprobs: None,
            }),
        ]));
        let tools = Arc::new(MockToolDispatcher::new(
            vec![ToolDefinition {
                name: "small_tool".into(),
                description: "returns small payload".into(),
                parameters: json!({}),
            }],
            vec![small_result],
        ));
        let store = Arc::new(InMemoryStateStore::new());

        let invoker = make_headroom_invoker(model, tools, store.clone());
        invoker.execute(&make_message_event()).await;

        // Tool input key should NOT have been written (below threshold)
        assert!(
            store.get("headroom:tool-input:msg-1:tc-small").await.is_none(),
            "small tool result should not trigger compression"
        );
    }

    // ── Test: count_message_tokens helper ─────────────────────────────────────

    #[test]
    fn token_count_heuristic() {
        // 8 chars total / 4 = 2 tokens
        let msgs = vec![ChatMessage::user("12345678")];
        assert_eq!(count_message_tokens(&msgs), 2);

        // Empty messages
        let empty: Vec<ChatMessage> = vec![];
        assert_eq!(count_message_tokens(&empty), 0);

        // Multiple messages summed
        let multi = vec![
            ChatMessage::system("abcd"),   // 4 chars
            ChatMessage::user("efgh"),     // 4 chars
        ];
        assert_eq!(count_message_tokens(&multi), 2); // 8 / 4
    }

    // ── Test: with_headroom constructor ───────────────────────────────────────

    #[tokio::test]
    async fn with_headroom_constructor_sets_fields() {
        let model = Arc::new(MockModelClient::new(vec![Ok(ModelCompletion {
            content: Some("hi".into()),
            tool_calls: vec![],
            logprobs: None,
        })]));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new());

        let invoker = ModelInvoker::with_headroom(
            model,
            tools,
            "custom system prompt",
            store,
        );

        assert!(invoker.headroom_enabled);
        assert!(invoker.state_store.is_some());
        assert_eq!(invoker.system_prompt, "custom system prompt");
    }

    // ── Test: headroom with invalid compressed output falls back ──────────────

    #[tokio::test]
    async fn headroom_invalid_compressed_output_uses_originals() {
        let long_content = "w".repeat(2000);
        let event = Event::Message {
            id: "msg-invalid".into(),
            channel: "telegram".into(),
            sender: "alice".into(),
            content: long_content,
        };

        let model = Arc::new(CapturingModelClient::new(ModelCompletion {
            content: Some("ok".into()),
            tool_calls: vec![],
            logprobs: None,
        }));
        let tools = Arc::new(MockToolDispatcher::new(vec![], vec![]));
        let store = Arc::new(InMemoryStateStore::new());

        // Write invalid JSON for the compressed output (not a Vec<ChatMessage>)
        store
            .set(
                "headroom:output:msg-invalid",
                json!({"not": "a message array"}),
            )
            .await;

        let invoker = make_headroom_invoker(model.clone(), tools, store);
        invoker.execute(&event).await;

        // Should fall back to originals (2 messages)
        let captured = model.captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(
            captured[0].len(),
            2,
            "should fall back to original 2 messages on deserialisation failure"
        );
    }
}
