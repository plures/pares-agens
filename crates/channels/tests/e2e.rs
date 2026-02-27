use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::stdin::StdinAdapter;
use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
use pares_agens_core::agent::Memory;
use pares_agens_core::{Agent, Event, InMemory};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn e2e_message_echo_and_memory_capture() {
    let memory = Arc::new(InMemory::new());
    let agent = Agent::new(Arc::clone(&memory) as Arc<dyn Memory + Send + Sync>);

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
    assert!(!recalled.is_empty(), "memory should have captured the message");
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
