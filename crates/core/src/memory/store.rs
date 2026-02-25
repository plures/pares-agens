use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{entry::MemoryEntry, Error};

/// Backing store for [`super::PluresLm`].
///
/// In production this will delegate to PluresDB. The trait allows swapping
/// in `InMemoryStore` for tests and embedded use-cases.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Persist a new memory entry.
    async fn insert(&self, entry: MemoryEntry) -> Result<(), Error>;

    /// Return all stored entries (unordered).
    async fn all(&self) -> Result<Vec<MemoryEntry>, Error>;
}

/// Thread-safe in-memory store backed by a `RwLock<Vec<MemoryEntry>>`.
///
/// Suitable for tests and single-process deployments without PluresDB.
pub struct InMemoryStore {
    entries: RwLock<Vec<MemoryEntry>>,
}

impl InMemoryStore {
    /// Create a new, empty store.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MemoryStore for InMemoryStore {
    async fn insert(&self, entry: MemoryEntry) -> Result<(), Error> {
        self.entries.write().await.push(entry);
        Ok(())
    }

    async fn all(&self) -> Result<Vec<MemoryEntry>, Error> {
        Ok(self.entries.read().await.clone())
    }
}
