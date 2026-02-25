use async_trait::async_trait;
use pares_agens_core::Event;
use uuid::Uuid;
use tokio::io::{AsyncBufReadExt, BufReader};
use std::sync::Arc;
use crate::adapter::{ChannelAdapter, ChannelError};

/// Reads lines from stdin, emits Message events, prints responses to stdout.
pub struct StdinAdapter {
    pub from: String,
}

impl StdinAdapter {
    pub fn new(from: impl Into<String>) -> Self {
        Self { from: from.into() }
    }
}

#[async_trait]
impl ChannelAdapter for StdinAdapter {
    fn name(&self) -> &str { "stdin" }

    async fn run(
        &self,
        on_event: impl Fn(Event) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send>> + Send + Sync + 'static,
    ) -> Result<(), ChannelError> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        let on_event = Arc::new(on_event);
        while let Some(line) = reader.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() { continue; }
            let event = Event::Message {
                id: Uuid::new_v4(),
                content: line,
                from: self.from.clone(),
            };
            let on_event = Arc::clone(&on_event);
            if let Some(Event::ModelResponse { content, .. }) = on_event(event).await {
                println!("{}", content);
            }
        }
        Ok(())
    }
}
