use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A single memory item recalled from PluresLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    /// Unique identifier for this memory.
    pub id: String,
    /// The role that produced this memory (`"user"` or `"assistant"`).
    pub role: String,
    /// The recalled text content.
    pub content: String,
}

/// A memory item to be stored in PluresLM after a conversation turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCapture {
    /// `"user"` or `"assistant"`.
    pub role: String,
    /// The text to store.
    pub content: String,
}

/// Abstraction over the PluresLM long-term memory store.
///
/// In production this will be backed by the real PluresLM client.  Tests use
/// mock implementations.
#[async_trait]
pub trait MemoryClient: Send + Sync {
    /// Recall up to `limit` memories relevant to `query`.
    async fn recall(&self, query: &str, limit: usize) -> Vec<Memory>;

    /// Persist a new memory item.
    async fn capture(&self, item: MemoryCapture);
}
