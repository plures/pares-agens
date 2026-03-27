//! Stdin/stdout channel adapter.
//!
//! Reads newline-delimited messages from standard input, emits them as
//! [`Event::Message`] events, and prints [`Event::ModelResponse`] content
//! back to standard output.
use async_trait::async_trait;
use pares_agens_core::Event;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

use crate::adapter::{ChannelAdapter, ChannelError};

/// Reads lines from stdin, emits Message events, prints responses to stdout.
pub struct StdinAdapter {
    /// Display name used as the `sender` field in emitted [`Event::Message`] events.
    pub from: String,
}

impl StdinAdapter {
    /// Create a new [`StdinAdapter`] that identifies its messages with the given sender name.
    pub fn new(from: impl Into<String>) -> Self {
        Self { from: from.into() }
    }
}

#[async_trait]
impl ChannelAdapter for StdinAdapter {
    fn name(&self) -> &str {
        "stdin"
    }

    async fn run(
        &self,
        on_event: impl Fn(Event) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ChannelError> {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin).lines();
        while let Some(line) = reader.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            let event = Event::Message {
                id: Uuid::new_v4().to_string(),
                content: line,
                channel: "stdin".to_string(),
                sender: self.from.clone(),
            };
            if let Some(Event::ModelResponse { content, .. }) = on_event(event).await {
                println!("{}", content);
            }
        }
        Ok(())
    }
}
