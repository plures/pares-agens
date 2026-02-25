use async_trait::async_trait;

use crate::{event::Event, procedure::Procedure};

/// Built-in `on_message` procedure.
///
/// Stub implementation: echoes the message content back as a new
/// [`Event::Message`].  A real implementation would perform PluresLM recall,
/// call the model, handle tool calls, and capture memory.
pub struct OnMessage;

#[async_trait]
impl Procedure for OnMessage {
    fn name(&self) -> &str {
        "on_message"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Message {
            id,
            channel,
            sender,
            content,
        } = event
        {
            tracing::info!(
                sender,
                channel,
                content,
                "on_message: echoing message"
            );
            vec![Event::Message {
                id: format!("{}-echo", id),
                channel: channel.clone(),
                sender: "agent".into(),
                content: content.clone(),
            }]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::Procedure;

    #[tokio::test]
    async fn on_message_echoes_content() {
        let handler = OnMessage;
        let event = Event::Message {
            id: "42".into(),
            channel: "stdin".into(),
            sender: "bob".into(),
            content: "ping".into(),
        };
        let result = handler.execute(&event).await;
        assert_eq!(result.len(), 1);
        if let Event::Message { content, sender, .. } = &result[0] {
            assert_eq!(content, "ping");
            assert_eq!(sender, "agent");
        } else {
            panic!("expected Message event");
        }
    }

    #[tokio::test]
    async fn on_message_ignores_non_message_events() {
        let handler = OnMessage;
        let timer = Event::Timer {
            id: "t1".into(),
            name: "tick".into(),
            recurring: false,
        };
        let result = handler.execute(&timer).await;
        assert!(result.is_empty());
    }
}
