use async_trait::async_trait;

use crate::event::Event;

/// Abstraction over PluresDB for polling events.
///
/// In production this will be backed by the real PluresDB client; in tests a
/// mock implementation is used.
#[async_trait]
pub trait EventSource: Send + Sync {
    /// Poll for new events.  Returns an empty vec when there is nothing to
    /// process.
    async fn poll_events(&self) -> Vec<Event>;
}
