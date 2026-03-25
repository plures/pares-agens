//! Error types for `pares-agens-dmem`.

use thiserror::Error;

/// All errors that can be produced by the distributed memory crate.
#[derive(Debug, Error)]
pub enum DmemError {
    /// A P2P fetch operation failed.
    #[error("peer fetch failed: {0}")]
    PeerFetch(String),

    /// A serialisation or deserialisation error.
    #[error("serialisation error: {0}")]
    Serialise(String),

    /// An embedding index operation failed.
    #[error("index error: {0}")]
    Index(String),

    /// The cache is over its storage budget and no entry can be evicted.
    #[error("cache full: no evictable entries")]
    CacheFull,
}
