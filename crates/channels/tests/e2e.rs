use pares_agens_channels::stdin::StdinAdapter;
use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_core::{Agent, Event, InMemory, Memory};
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn e2e_message_echo_and_memory_capture() {
    // Arrange
    let memory = Arc::new(InMemory::new());
    let agent = Agent::new(Arc::clone(&memory) as Arc<dyn Memory + Send + Sync>);

    // Act — send a message event directly
    let msg = Event::Message {
        id: Uuid::new_v4(),
        content: "hello world".to_string(),
        from: "tester".to_string(),
    };
    let response = agent.handle_event(msg).await;

    // Assert — response is an echo
    assert!(matches!(response, Some(Event::ModelResponse { ref content, .. }) if content == "Echo: hello world"));

    // Assert — memory captured the event
    let recalled = memory.recall("hello").await.expect("recall failed");
    assert!(!recalled.is_empty(), "memory should have captured the message");
}

#[test]
fn stdin_adapter_name() {
    let adapter = StdinAdapter::new("test");
    assert_eq!(adapter.name(), "stdin");
}
