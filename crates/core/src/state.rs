//! PluresDB-backed `StateStore` implementations.
//!
//! # PluresDB schema
//!
//! Agent state is persisted inside a [`pluresdb::CrdtStore`] using the
//! following conventions:
//!
//! | Concept       | Mapping                                         |
//! |---------------|-------------------------------------------------|
//! | Table/bucket  | All state nodes share a common `actor` tag      |
//! |               | `"pares-agens-state"` for write attribution.    |
//! | Row key       | The state key string, used directly as the      |
//! |               | [`NodeId`] so lookups are O(1) hash-map reads.  |
//! | Row value     | The [`serde_json::Value`] stored verbatim as    |
//! |               | the node's [`NodeData`] payload.                |
//!
//! Because [`CrdtStore`] is an append-merge CRDT, repeated `set` calls on
//! the same key perform a merge-update rather than a destructive overwrite,
//! which is exactly the semantics we need for distributed agent state.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
use serde_json::Value;
use tokio::sync::RwLock;

/// The PluresDB actor ID used for all state write operations.
const ACTOR: &str = "pares-agens-state";

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Minimal key-value state interface backed by PluresDB.
///
/// Both [`InMemoryStateStore`] (for tests / embedded use) and
/// [`PluresDbStateStore`] (for production) implement this trait so that
/// call-sites do not depend on a specific storage backend.
#[async_trait::async_trait]
pub trait StateStore: Send + Sync {
    /// Retrieve the value stored under `key`, or `None` if absent.
    async fn get(&self, key: &str) -> Option<Value>;
    /// Persist `value` under `key`, replacing any previous value.
    async fn set(&self, key: &str, value: Value);
}

// ---------------------------------------------------------------------------
// InMemoryStateStore
// ---------------------------------------------------------------------------

/// A [`StateStore`] backed by an in-process `HashMap` protected by a
/// [`tokio::sync::RwLock`].
///
/// This implementation is designed for unit tests and single-process
/// deployments that do not require durable persistence.
pub struct InMemoryStateStore {
    data: RwLock<HashMap<String, Value>>,
}

impl InMemoryStateStore {
    /// Create a new, empty store.
    pub fn new() -> Self {
        Self {
            data: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStateStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl StateStore for InMemoryStateStore {
    async fn get(&self, key: &str) -> Option<Value> {
        self.data.read().await.get(key).cloned()
    }

    async fn set(&self, key: &str, value: Value) {
        self.data.write().await.insert(key.to_string(), value);
    }
}

// ---------------------------------------------------------------------------
// PluresDbStateStore
// ---------------------------------------------------------------------------

/// A [`StateStore`] backed by a PluresDB [`CrdtStore`].
///
/// Uses [`SledStorage`] for durable on-disk persistence when opened via
/// [`PluresDbStateStore::open`].  An ephemeral variant (backed by
/// [`MemoryStorage`]) is available via [`PluresDbStateStore::in_memory`].
///
/// State entries are stored as CRDT nodes where the state key is the
/// [`pluresdb::NodeId`] and the JSON value is the node payload.  Repeated
/// writes to the same key perform a CRDT merge-update, preserving the
/// last-writer-wins semantics expected by agents.
pub struct PluresDbStateStore {
    store: CrdtStore,
}

impl PluresDbStateStore {
    /// Open or create a durable PluresDB-backed state store at `path`.
    ///
    /// # Errors
    /// Returns an error string if [`SledStorage`] cannot be opened (e.g.
    /// permission denied, corrupted database).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let storage: Arc<dyn StorageEngine> =
            Arc::new(SledStorage::open(path).map_err(|e| format!("open failed: {e}"))?);

        let store = CrdtStore::default().with_persistence(storage);
        Ok(Self { store })
    }

    /// Create an ephemeral in-memory PluresDB state store.
    ///
    /// Useful for integration tests that need a real [`CrdtStore`] without
    /// touching the filesystem.
    pub fn in_memory() -> Self {
        let storage: Arc<dyn StorageEngine> = Arc::new(MemoryStorage::default());
        let store = CrdtStore::default().with_persistence(storage);
        Self { store }
    }
}

#[async_trait::async_trait]
impl StateStore for PluresDbStateStore {
    async fn get(&self, key: &str) -> Option<Value> {
        self.store.get(key).map(|record| record.data)
    }

    async fn set(&self, key: &str, value: Value) {
        self.store.put(key, ACTOR, value);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── InMemoryStateStore ────────────────────────────────────────────────

    #[tokio::test]
    async fn in_memory_get_returns_none_when_empty() {
        let store = InMemoryStateStore::new();
        assert!(store.get("missing").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_set_then_get_roundtrip() {
        let store = InMemoryStateStore::new();
        store.set("greeting", json!("hello")).await;
        assert_eq!(store.get("greeting").await, Some(json!("hello")));
    }

    #[tokio::test]
    async fn in_memory_set_overwrites_previous_value() {
        let store = InMemoryStateStore::new();
        store.set("counter", json!(1)).await;
        store.set("counter", json!(2)).await;
        assert_eq!(store.get("counter").await, Some(json!(2)));
    }

    #[tokio::test]
    async fn in_memory_multiple_keys_are_independent() {
        let store = InMemoryStateStore::new();
        store.set("a", json!(true)).await;
        store.set("b", json!(42)).await;
        assert_eq!(store.get("a").await, Some(json!(true)));
        assert_eq!(store.get("b").await, Some(json!(42)));
        assert!(store.get("c").await.is_none());
    }

    #[tokio::test]
    async fn in_memory_default_is_empty() {
        let store = InMemoryStateStore::default();
        assert!(store.get("x").await.is_none());
    }

    // ── PluresDbStateStore ────────────────────────────────────────────────

    #[tokio::test]
    async fn pluresdb_get_returns_none_when_empty() {
        let store = PluresDbStateStore::in_memory();
        assert!(store.get("missing").await.is_none());
    }

    #[tokio::test]
    async fn pluresdb_set_then_get_roundtrip() {
        let store = PluresDbStateStore::in_memory();
        store.set("greeting", json!("hello")).await;
        assert_eq!(store.get("greeting").await, Some(json!("hello")));
    }

    #[tokio::test]
    async fn pluresdb_set_overwrites_previous_value() {
        let store = PluresDbStateStore::in_memory();
        store.set("counter", json!(1)).await;
        store.set("counter", json!(2)).await;
        assert_eq!(store.get("counter").await, Some(json!(2)));
    }

    #[tokio::test]
    async fn pluresdb_multiple_keys_are_independent() {
        let store = PluresDbStateStore::in_memory();
        store.set("name", json!("aria")).await;
        store.set("version", json!(3)).await;
        assert_eq!(store.get("name").await, Some(json!("aria")));
        assert_eq!(store.get("version").await, Some(json!(3)));
        assert!(store.get("absent").await.is_none());
    }

    #[tokio::test]
    async fn pluresdb_open_creates_persistent_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluresDbStateStore::open(dir.path()).unwrap();
        store.set("persistent", json!({ "ok": true })).await;
        assert_eq!(store.get("persistent").await, Some(json!({ "ok": true })));
    }

    #[tokio::test]
    async fn pluresdb_stores_complex_json_values() {
        let store = PluresDbStateStore::in_memory();
        let complex = json!({
            "nested": { "list": [1, 2, 3] },
            "flag": false,
            "score": 0.95
        });
        store.set("complex", complex.clone()).await;
        assert_eq!(store.get("complex").await, Some(complex));
    }
}
