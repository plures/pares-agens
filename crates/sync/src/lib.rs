//! `pares-agens-sync` — P2P device sync via Hyperswarm DHT for Pares Agens.
//!
//! Implements cross-device memory and state synchronisation using the
//! Hyperswarm DHT.  The crate provides a pure-Rust abstraction over the
//! Hyperswarm P2P transport so that higher-level crates (e.g. `tauri-app`)
//! can drive the sync lifecycle without coupling to a specific FFI backend.
//!
//! # Architecture
//!
//! ```text
//! SyncEngine
//!   ├── TopicManager   — subscribe/unsubscribe Hyperswarm topics
//!   ├── DeviceRegistry — track paired devices and their sync state
//!   ├── PairingSession — generate/share sync keys, approve devices
//!   ├── OfflineQueue   — buffer ChangeEvents while peers are offline
//!   └── ConflictResolution — CRDT-merge incoming payloads
//! ```
//!
//! # Sync topics
//!
//! | [`SyncTopic`]             | Content synced                         |
//! |---------------------------|----------------------------------------|
//! | `MemoryEntries`           | PluresDB memory entries + embeddings   |
//! | `Procedures`              | Agent procedure registry               |
//! | `AgentConfig`             | Agent configuration and settings       |
//! | `ConversationHistory`     | Chat / conversation turn records       |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_sync::{SyncEngine, SyncTopic};
//!
//! let mut engine = SyncEngine::new("my-device");
//! engine.subscribe_topic(SyncTopic::MemoryEntries);
//! engine.subscribe_topic(SyncTopic::AgentConfig);
//!
//! // Queue a local change while offline
//! engine.enqueue_change(SyncTopic::MemoryEntries, serde_json::json!({"id": "m1"}));
//!
//! // Drain queued events once a peer connects
//! let pending = engine.drain_queue();
//! assert_eq!(pending.len(), 1);
//! ```

#![warn(missing_docs)]

pub mod conflict;
pub mod engine;
pub mod pairing;
pub mod peer;
pub mod queue;
pub mod topic;

pub use engine::SyncEngine;
pub use pairing::{ApprovalState, PairingSession, SyncKey};
pub use peer::{DeviceRegistry, DeviceStatus, PairedDevice, TopicSyncState};
pub use queue::{ChangeEvent, OfflineQueue};
pub use topic::{TopicManager, TopicSubscription};

use thiserror::Error;

// ── SyncTopic ─────────────────────────────────────────────────────────────────

/// Hyperswarm DHT topics that Pares Agens devices synchronise over.
///
/// Each topic corresponds to a logical data domain.  Devices must subscribe to
/// a topic before they send or receive sync payloads for that domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncTopic {
    /// PluresDB memory entries (including embeddings).
    MemoryEntries,
    /// Agent procedure registry.
    Procedures,
    /// Agent configuration and settings.
    AgentConfig,
    /// Conversation / chat history turns.
    ConversationHistory,
}

impl SyncTopic {
    /// Return a stable string identifier for use as a DHT topic key.
    #[must_use]
    pub fn as_key(&self) -> &'static str {
        match self {
            Self::MemoryEntries => "pares-agens/memory-entries/v1",
            Self::Procedures => "pares-agens/procedures/v1",
            Self::AgentConfig => "pares-agens/agent-config/v1",
            Self::ConversationHistory => "pares-agens/conversation-history/v1",
        }
    }

    /// Return all known topics.
    #[must_use]
    pub fn all() -> &'static [SyncTopic] {
        &[
            Self::MemoryEntries,
            Self::Procedures,
            Self::AgentConfig,
            Self::ConversationHistory,
        ]
    }
}

impl std::fmt::Display for SyncTopic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_key())
    }
}

// ── SyncError ─────────────────────────────────────────────────────────────────

/// Errors that can occur during P2P sync operations.
#[derive(Debug, Error)]
pub enum SyncError {
    /// The requested device is not paired with this device.
    #[error("unknown device: {0}")]
    UnknownDevice(String),

    /// The pairing code or sync key is malformed.
    #[error("invalid pairing material: {0}")]
    InvalidPairingMaterial(String),

    /// A pairing approval was requested for a session that does not exist.
    #[error("pairing session not found: {0}")]
    PairingSessionNotFound(String),

    /// An operation was attempted on a topic that has not been subscribed.
    #[error("topic not subscribed: {0}")]
    TopicNotSubscribed(String),

    /// Conflict resolution failed for the given entry.
    #[error("conflict resolution failed: {0}")]
    ConflictResolution(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_topic_as_key_is_stable() {
        assert_eq!(
            SyncTopic::MemoryEntries.as_key(),
            "pares-agens/memory-entries/v1"
        );
        assert_eq!(SyncTopic::Procedures.as_key(), "pares-agens/procedures/v1");
        assert_eq!(
            SyncTopic::AgentConfig.as_key(),
            "pares-agens/agent-config/v1"
        );
        assert_eq!(
            SyncTopic::ConversationHistory.as_key(),
            "pares-agens/conversation-history/v1"
        );
    }

    #[test]
    fn sync_topic_all_returns_four_variants() {
        assert_eq!(SyncTopic::all().len(), 4);
    }

    #[test]
    fn sync_topic_display_equals_as_key() {
        for topic in SyncTopic::all() {
            assert_eq!(format!("{topic}"), topic.as_key());
        }
    }

    #[test]
    fn sync_topic_roundtrip_json() {
        let topic = SyncTopic::ConversationHistory;
        let json = serde_json::to_string(&topic).unwrap();
        let decoded: SyncTopic = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, topic);
    }
}
