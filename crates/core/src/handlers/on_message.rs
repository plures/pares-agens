use std::sync::Arc;

use async_trait::async_trait;
use tracing::{debug, error, info, warn};

use crate::{
    event::Event,
    memory::{MemoryCapture, MemoryClient},
    model::{ChatMessage, ChatOptions, ModelClient, ToolDispatcher},
    procedure::Procedure,
};

/// Maximum number of model → tool → model agentic loop iterations.
///
/// Prevents infinite loops when a model repeatedly requests tool calls.
const MAX_TOOL_ITERATIONS: usize = 5;

/// Built-in `on_message` procedure.
///
/// Implements the full 6-step agent message pipeline:
/// 1. **PluresLM recall** — retrieve relevant memories for context
/// 2. **Format context** — build the message history from memories + current input
/// 3. **Call model with tools** — send messages + tool definitions to the model
/// 4. **Handle tool calls** — execute any requested tools and re-call the model
/// 5. **Emit response** — return a [`Event::Message`] with the agent's reply
/// 6. **Capture memory** — persist the conversation turn to PluresLM
pub struct OnMessage {
    memory: Arc<dyn MemoryClient>,
    model: Arc<dyn ModelClient>,
    tools: Arc<dyn ToolDispatcher>,
    system_prompt: String,
}

impl OnMessage {
    /// Create a new `OnMessage` handler.
    ///
    /// # Arguments
    /// * `memory` — PluresLM client for recall and capture
    /// * `model` — language model client
    /// * `tools` — MCP tool dispatcher
    /// * `system_prompt` — the system prompt prepended to every conversation
    pub fn new(
        memory: Arc<dyn MemoryClient>,
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            memory,
            model,
            tools,
            system_prompt: system_prompt.into(),
        }
    }
}

#[async_trait]
impl Procedure for OnMessage {
    fn name(&self) -> &str {
        "on_message"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let Event::Message {
            id,
            channel,
            sender,
            content,
        } = event
        else {
            return vec![];
        };

        info!(
            sender,
            channel,
            content = content.as_str(),
            "on_message: received"
        );

        // ── Step 1: PluresLM recall ───────────────────────────────────────────
        let memories = self.memory.recall(content, 10).await;
        debug!(count = memories.len(), "recalled memories");

        // ── Step 2: Format context ────────────────────────────────────────────
        let mut messages: Vec<ChatMessage> = Vec::with_capacity(memories.len() + 2);
        messages.push(ChatMessage::system(&self.system_prompt));

        for mem in &memories {
            if mem.role == "user" {
                messages.push(ChatMessage::user(&mem.content));
            } else {
                messages.push(ChatMessage::assistant(&mem.content));
            }
        }

        messages.push(ChatMessage::user(content));

        // ── Steps 3 & 4: Agentic loop (model → tools → model …) ──────────────
        let tool_defs = self.tools.available_tools().await;

        // Track whether the model produced a final text response.
        let mut final_response: Option<String> = None;

        for iteration in 0..MAX_TOOL_ITERATIONS {
            let completion = match self
                .model
                .complete(&messages, &tool_defs, &ChatOptions::default())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    error!(error = %e, "model completion failed");
                    break;
                }
            };

            // No tool calls — the model produced a final response.
            if completion.tool_calls.is_empty() {
                final_response = Some(completion.content.unwrap_or_default());
                break;
            }

            warn!(
                iteration,
                tool_calls = completion.tool_calls.len(),
                "model requested tool calls"
            );

            // Append the assistant turn with the tool call requests.
            let mut assistant_msg =
                ChatMessage::assistant(completion.content.as_deref().unwrap_or(""));
            assistant_msg.tool_calls = Some(completion.tool_calls.clone());
            messages.push(assistant_msg);

            // Execute each tool and append the results.
            for tc in &completion.tool_calls {
                info!(tool = tc.name.as_str(), id = tc.id.as_str(), "calling tool");
                let result = self.tools.call_tool(&tc.name, tc.arguments.clone()).await;
                messages.push(ChatMessage::tool_result(&tc.id, result));
            }
        }

        // Only warn when all iterations were exhausted without a final text response.
        if final_response.is_none() {
            warn!(
                MAX_TOOL_ITERATIONS,
                "tool loop exhausted without final response"
            );
        }

        let response_content = final_response.unwrap_or_default();

        // ── Step 5: Emit response ─────────────────────────────────────────────
        info!(
            sender,
            channel,
            length = response_content.len(),
            "on_message: emitting response"
        );
        let response = Event::Message {
            id: format!("{id}-response"),
            channel: channel.clone(),
            sender: "agent".into(),
            content: response_content.clone(),
        };

        // ── Step 6: Capture memory ────────────────────────────────────────────
        if let Err(e) = self
            .memory
            .capture(MemoryCapture {
                role: "user".into(),
                content: content.clone(),
            })
            .await
        {
            error!(error = %e, "on_message: failed to capture user turn in memory");
        }
        if let Err(e) = self
            .memory
            .capture(MemoryCapture {
                role: "assistant".into(),
                content: response_content,
            })
            .await
        {
            error!(error = %e, "on_message: failed to capture assistant turn in memory");
        }

        vec![response]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use crate::model::{ChatOptions, ModelCompletion, ToolCall, ToolDefinition};
    use serde_json::json;
    use std::sync::Mutex;

    // ── Mocks ─────────────────────────────────────────────────────────────────

    struct MockMemory {
        recalls: Vec<Memory>,
        captured: Mutex<Vec<MemoryCapture>>,
    }

    impl MockMemory {
        fn empty() -> Self {
            Self {
                recalls: vec![],
                captured: Mutex::new(vec![]),
            }
        }

        fn with_history(recalls: Vec<Memory>) -> Self {
            Self {
                recalls,
                captured: Mutex::new(vec![]),
            }
        }

        fn captured_count(&self) -> usize {
            self.captured.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl MemoryClient for MockMemory {
        async fn recall(&self, _query: &str, _limit: usize) -> Vec<Memory> {
            self.recalls.clone()
        }

        async fn capture(&self, item: MemoryCapture) -> Result<(), String> {
            self.captured.lock().unwrap().push(item);
            Ok(())
        }
    }

    struct MockModel {
        response: String,
    }

    impl MockModel {
        fn with_response(text: &str) -> Self {
            Self {
                response: text.into(),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModel {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some(self.response.clone()),
                tool_calls: vec![],
                logprobs: None,
            })
        }
    }

    struct MockModelWithTool {
        tool_name: String,
        final_response: String,
        call_count: Mutex<usize>,
    }

    impl MockModelWithTool {
        fn new(tool_name: &str, final_response: &str) -> Self {
            Self {
                tool_name: tool_name.into(),
                final_response: final_response.into(),
                call_count: Mutex::new(0),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelWithTool {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            let mut count = self.call_count.lock().unwrap();
            *count += 1;
            if *count == 1 {
                // First call: request a tool
                Ok(ModelCompletion {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "tc-1".into(),
                        name: self.tool_name.clone(),
                        arguments: json!({"q": "test"}),
                    }],
                    logprobs: None,
                })
            } else {
                // Second call: final response after tool result
                Ok(ModelCompletion {
                    content: Some(self.final_response.clone()),
                    tool_calls: vec![],
                    logprobs: None,
                })
            }
        }
    }

    struct MockTools {
        tools: Vec<ToolDefinition>,
        result: String,
        called: Mutex<Vec<String>>,
    }

    impl MockTools {
        fn empty() -> Self {
            Self {
                tools: vec![],
                result: String::new(),
                called: Mutex::new(vec![]),
            }
        }

        fn with_tool(name: &str, result: &str) -> Self {
            Self {
                tools: vec![ToolDefinition {
                    name: name.into(),
                    description: "mock tool".into(),
                    parameters: json!({"type": "object", "properties": {}}),
                }],
                result: result.into(),
                called: Mutex::new(vec![]),
            }
        }

        fn called_tools(&self) -> Vec<String> {
            self.called.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ToolDispatcher for MockTools {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            self.tools.clone()
        }

        async fn call_tool(&self, name: &str, _arguments: serde_json::Value) -> String {
            self.called.lock().unwrap().push(name.into());
            self.result.clone()
        }
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    fn make_message(content: &str) -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn returns_model_response() {
        let handler = OnMessage::new(
            Arc::new(MockMemory::empty()),
            Arc::new(MockModel::with_response("Pong!")),
            Arc::new(MockTools::empty()),
            "You are a helpful assistant.",
        );

        let results = handler.execute(&make_message("ping")).await;

        assert_eq!(results.len(), 1);
        if let Event::Message {
            content, sender, ..
        } = &results[0]
        {
            assert_eq!(content, "Pong!");
            assert_eq!(sender, "agent");
        } else {
            panic!("expected Message event");
        }
    }

    #[tokio::test]
    async fn captures_user_and_assistant_memory() {
        let memory = Arc::new(MockMemory::empty());
        let handler = OnMessage::new(
            memory.clone(),
            Arc::new(MockModel::with_response("Hi there!")),
            Arc::new(MockTools::empty()),
            "You are helpful.",
        );

        handler.execute(&make_message("Hello")).await;

        assert_eq!(
            memory.captured_count(),
            2,
            "must capture user + assistant turns"
        );
    }

    #[tokio::test]
    async fn includes_recalled_memories_in_context() {
        let memory = Arc::new(MockMemory::with_history(vec![Memory {
            id: "m1".into(),
            role: "user".into(),
            content: "previous question".into(),
        }]));
        let handler = OnMessage::new(
            memory.clone(),
            Arc::new(MockModel::with_response("Sure!")),
            Arc::new(MockTools::empty()),
            "System prompt.",
        );

        let results = handler.execute(&make_message("follow-up")).await;
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn executes_tool_calls_and_returns_final_response() {
        let tools = Arc::new(MockTools::with_tool("web_search", r#"{"results": []}"#));
        let model = Arc::new(MockModelWithTool::new(
            "web_search",
            "Here is what I found.",
        ));
        let handler = OnMessage::new(
            Arc::new(MockMemory::empty()),
            model,
            tools.clone(),
            "You are helpful.",
        );

        let results = handler.execute(&make_message("Search for something")).await;

        assert_eq!(results.len(), 1);
        if let Event::Message { content, .. } = &results[0] {
            assert_eq!(content, "Here is what I found.");
        } else {
            panic!("expected Message event");
        }
        assert_eq!(tools.called_tools(), vec!["web_search"]);
    }

    #[tokio::test]
    async fn ignores_non_message_events() {
        let handler = OnMessage::new(
            Arc::new(MockMemory::empty()),
            Arc::new(MockModel::with_response("oops")),
            Arc::new(MockTools::empty()),
            "System.",
        );
        let timer = Event::Timer {
            id: "t1".into(),
            name: "tick".into(),
            recurring: false,
        };
        let results = handler.execute(&timer).await;
        assert!(results.is_empty());
    }
}
