use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::stdin::StdinAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::Memory;
use pares_agens_core::model::{
    ChatMessage, ChatOptions, ModelClient, ModelCompletion, ToolDefinition, ToolDispatcher,
};
use pares_agens_core::{Agent, Event, InMemory};
use std::sync::Arc;
use uuid::Uuid;

struct MockModel;

#[async_trait::async_trait]
impl ModelClient for MockModel {
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        _options: &ChatOptions,
    ) -> Result<ModelCompletion, String> {
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.clone())
            .unwrap_or_default();
        Ok(ModelCompletion {
            content: Some(format!("Echo: {last_user}")),
            tool_calls: vec![],
            logprobs: None,
        })
    }
}

struct MockTools;

#[async_trait::async_trait]
impl ToolDispatcher for MockTools {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        vec![]
    }

    async fn call_tool(&self, _name: &str, _arguments: serde_json::Value) -> String {
        "ok".into()
    }
}

#[tokio::test]
async fn e2e_message_echo_and_memory_capture() {
    let memory = Arc::new(InMemory::new());
    let agent = Agent::new(Arc::clone(&memory) as Arc<dyn Memory + Send + Sync>).with_model(
        Arc::new(MockModel),
        Arc::new(MockTools),
        "You are a test agent.".into(),
    );

    let msg = Event::Message {
        id: Uuid::new_v4().to_string(),
        channel: "direct".to_string(),
        sender: "tester".to_string(),
        content: "hello world".to_string(),
    };
    let response = agent.handle_event(msg).await;

    assert!(
        matches!(response, Some(Event::ModelResponse { ref content, .. }) if content == "Echo: hello world")
    );

    let recalled = memory.recall("hello").await.expect("recall failed");
    assert!(
        !recalled.is_empty(),
        "memory should have captured the message"
    );
}

#[test]
fn stdin_adapter_name() {
    let adapter = StdinAdapter::new("test");
    assert_eq!(adapter.name(), "stdin");
}

#[test]
fn telegram_adapter_name() {
    let adapter = TelegramAdapter::new(TelegramConfig::new("fake-token"));
    assert_eq!(adapter.name(), "telegram");
}
