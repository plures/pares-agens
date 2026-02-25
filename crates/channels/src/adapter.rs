use async_trait::async_trait;
use pares_agens_core::Event;

/// A channel adapter bridges an external communication channel to the agent event loop.
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Name of this adapter (e.g. "stdin", "telegram").
    fn name(&self) -> &str;
    /// Run the adapter loop. Reads input, calls `on_event`, writes responses.
    async fn run(
        &self,
        on_event: impl Fn(Event) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), ChannelError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Channel closed")]
    Closed,
    #[error("Telegram error: {0}")]
    Telegram(String),
}
