//! Cognition-side adapter onto the PluresDB procedure engine.
//!
//! The platform execution half of the bridge now lives in
//! [`pares_radix_core::pluresdb_bridge`] (procedure / constraint execution against a
//! [`CrdtStore`]). This module keeps the **cognition seam**:
//!
//! - [`StoreAdapter`] populates an in-process [`CrdtStore`] from a pares-radix
//!   [`MemoryStore`], making the full corpus of memory entries available to the
//!   PluresDB procedure engine without any network hop.
//! - Cognition-side convenience constructors on [`PluresDbBridge`]
//!   ([`PluresDbBridgeExt::from_memory_store`] /
//!   [`PluresDbBridgeExt::reload_from_memory_store`]) that snapshot a
//!   `MemoryStore` first, then delegate to the platform bridge.
//!
//! [`PluresDbBridge`] and [`BridgeError`] are re-exported from
//! [`pares_radix_core::pluresdb_bridge`] so existing cognition callers continue to use
//! `crate::cerebellum::bridge::{PluresDbBridge, BridgeError}` unchanged.
//!
//! # Example
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use pares_agens_core::cerebellum::bridge::{PluresDbBridge, PluresDbBridgeExt, BridgeError};
//! # use pares_agens_core::memory::store::InMemoryStore;
//! # #[tokio::main] async fn main() -> Result<(), BridgeError> {
//! let store = Arc::new(InMemoryStore::new());
//! let bridge = PluresDbBridge::from_memory_store(store).await?;
//!
//! use pluresdb_procedures::ir::{Predicate, Step};
//! let steps = vec![
//!     Step::Filter { predicate: Predicate::eq("category", "decision") },
//! ];
//! let result = bridge.run_steps(steps).await?;
//! println!("{} nodes returned", result.nodes.len());
//! # Ok(())
//! # }
//! ```

use std::sync::Arc;

use pluresdb::CrdtStore;

use crate::memory::store::MemoryStore;

// Re-export the platform bridge types so existing cognition callers that use
// `crate::cerebellum::bridge::{PluresDbBridge, BridgeError}` keep working.
pub use pares_radix_core::pluresdb_bridge::{BridgeError, PluresDbBridge};

// ---------------------------------------------------------------------------
// StoreAdapter (cognition seam: MemoryStore -> CrdtStore)
// ---------------------------------------------------------------------------

/// Populates a [`CrdtStore`] with entries from a pares-radix [`MemoryStore`].
///
/// This adapter is the glue layer between the pares-radix memory subsystem and
/// the PluresDB procedure engine.  The [`CrdtStore`] it builds is a snapshot
/// of the pares-radix store at the moment [`StoreAdapter::load`] is called.
/// Subsequent writes to the pares-radix store are not reflected automatically;
/// callers should construct a fresh [`PluresDbBridge`] (or call
/// [`PluresDbBridge::reload`]) when a fresh view is required.
pub struct StoreAdapter {
    inner: Arc<dyn MemoryStore>,
}

impl StoreAdapter {
    /// Create a new adapter wrapping the given pares-radix memory store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { inner: store }
    }

    /// Load all memory entries from the pares-radix store into a freshly
    /// created [`CrdtStore`] and return it.
    ///
    /// Each [`MemoryEntry`][crate::memory::entry::MemoryEntry] is stored as a
    /// JSON node under its own ID with actor `"pares-radix"`.  The `category`,
    /// `content`, `score`, `tags`, and `created_at` fields are preserved in
    /// the node payload so that PluresDB filter / sort / project steps can
    /// reference them.
    ///
    /// The `embedding` vector is passed to
    /// [`CrdtStore::put_with_embedding`] so that the HNSW index is populated
    /// for any future vector-search steps.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Store`] if the underlying pares-radix store
    /// fails to enumerate its entries.
    pub async fn load(&self) -> Result<CrdtStore, BridgeError> {
        let crdt = CrdtStore::default();
        let entries = self
            .inner
            .all()
            .await
            .map_err(|e| BridgeError::Store(e.to_string()))?;

        for entry in entries {
            let embedding = entry.embedding.clone();
            let data = serde_json::json!({
                "category": entry.category.as_str(),
                "content":  entry.content,
                "score":    entry.score,
                "tags":     entry.tags,
                "created_at": entry.created_at,
            });
            if embedding.is_empty() {
                crdt.put(&entry.id, "pares-radix", data);
            } else {
                crdt.put_with_embedding(&entry.id, "pares-radix", data, embedding);
            }
        }

        Ok(crdt)
    }
}

// ---------------------------------------------------------------------------
// Cognition-side convenience constructors on the platform bridge.
//
// `PluresDbBridge` is now defined in the platform crate
// (`pares_radix_core::pluresdb_bridge`), so the orphan rule forbids an inherent
// `impl` here. Instead these `MemoryStore`-aware helpers are exposed via the
// [`PluresDbBridgeExt`] extension trait, implemented for the platform bridge.
// This keeps the cognition seam (no `crate::memory` dependency leaks into the
// platform module) while remaining ergonomic for cognition callers.
// ---------------------------------------------------------------------------

/// Cognition-side extension methods for the platform [`PluresDbBridge`].
///
/// Brings `MemoryStore`-aware constructors to the platform bridge without
/// adding a cognition dependency to `pares_radix_core`.
#[async_trait::async_trait]
pub trait PluresDbBridgeExt: Sized {
    /// Create a bridge connected to the given pares-radix memory store.
    ///
    /// All existing memory entries are snapshotted into the internal
    /// [`CrdtStore`] immediately so they are available to procedure pipelines.
    /// Delegates to [`PluresDbBridge::from_crdt`] after snapshotting.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Store`] if the initial load fails.
    async fn from_memory_store(store: Arc<dyn MemoryStore>) -> Result<Self, BridgeError>;

    /// Reload the internal [`CrdtStore`] snapshot from a pares-radix store.
    ///
    /// Call this when the pares-radix store has been updated and you want the
    /// bridge to reflect the latest entries. Registered procedures are
    /// preserved. Delegates to [`PluresDbBridge::reload_from`].
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Store`] if the reload fails.
    async fn reload_from_memory_store(
        &mut self,
        store: Arc<dyn MemoryStore>,
    ) -> Result<(), BridgeError>;
}

#[async_trait::async_trait]
impl PluresDbBridgeExt for PluresDbBridge {
    async fn from_memory_store(store: Arc<dyn MemoryStore>) -> Result<Self, BridgeError> {
        let adapter = StoreAdapter::new(store);
        let crdt = adapter.load().await?;
        Ok(Self::from_crdt(crdt))
    }

    async fn reload_from_memory_store(
        &mut self,
        store: Arc<dyn MemoryStore>,
    ) -> Result<(), BridgeError> {
        let adapter = StoreAdapter::new(store);
        let crdt = adapter.load().await?;
        self.reload_from(crdt);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests (cognition: StoreAdapter + MemoryStore-driven)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::entry::{MemoryCategory, MemoryEntry};
    use crate::memory::store::InMemoryStore;

    /// Build a [`MemoryEntry`] suitable for testing.
    fn make_entry(id: &str, content: &str, category: MemoryCategory) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            category,
            tags: vec![],
            embedding: vec![0.1_f32, 0.2, 0.3],
            score: 0.9,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    async fn store_with_entries() -> Arc<InMemoryStore> {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(make_entry(
                "d1",
                "Use tokio for async",
                MemoryCategory::Decision,
            ))
            .await
            .unwrap();
        store
            .insert(make_entry(
                "d2",
                "Avoid blocking calls",
                MemoryCategory::Decision,
            ))
            .await
            .unwrap();
        store
            .insert(make_entry(
                "c1",
                "fn main() {}",
                MemoryCategory::CodePattern,
            ))
            .await
            .unwrap();
        store
    }

    // ── StoreAdapter ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn store_adapter_loads_all_entries() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(make_entry("e1", "alpha", MemoryCategory::Conversation))
            .await
            .unwrap();
        store
            .insert(make_entry("e2", "beta", MemoryCategory::Conversation))
            .await
            .unwrap();

        let adapter = StoreAdapter::new(store);
        let crdt = adapter.load().await.unwrap();
        assert_eq!(crdt.list().len(), 2);
    }

    #[tokio::test]
    async fn store_adapter_empty_store_produces_empty_crdt() {
        let store = Arc::new(InMemoryStore::new());
        let adapter = StoreAdapter::new(store);
        let crdt = adapter.load().await.unwrap();
        assert!(crdt.list().is_empty());
    }

    #[tokio::test]
    async fn store_adapter_preserves_category_field() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(make_entry("p1", "prefer tabs", MemoryCategory::Preference))
            .await
            .unwrap();

        let adapter = StoreAdapter::new(store);
        let crdt = adapter.load().await.unwrap();
        let records = crdt.list();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].data["category"], "preference");
    }

    // ── PluresDbBridge::new (cognition constructor over StoreAdapter) ─────────

    #[tokio::test]
    async fn new_snapshots_memory_store() {
        let store = store_with_entries().await;
        let bridge = PluresDbBridge::from_memory_store(store as Arc<dyn MemoryStore>)
            .await
            .unwrap();
        let result = bridge.run_steps(vec![]).await.unwrap();
        assert_eq!(result.nodes.len(), 3);
    }

    // ── PluresDbBridge::reload (cognition reload over StoreAdapter) ───────────

    #[tokio::test]
    async fn reload_reflects_new_entries() {
        let store = Arc::new(InMemoryStore::new());
        store
            .insert(make_entry(
                "r1",
                "initial entry",
                MemoryCategory::Conversation,
            ))
            .await
            .unwrap();

        let mut bridge = PluresDbBridge::from_memory_store(Arc::clone(&store) as Arc<dyn MemoryStore>)
            .await
            .unwrap();
        // Before reload, only 1 entry is visible.
        let before = bridge.run_steps(vec![]).await.unwrap();
        assert_eq!(before.nodes.len(), 1);

        // Add a second entry to the backing store.
        store
            .insert(make_entry("r2", "new entry", MemoryCategory::Conversation))
            .await
            .unwrap();

        // After reload, both entries are visible.
        bridge
            .reload_from_memory_store(Arc::clone(&store) as Arc<dyn MemoryStore>)
            .await
            .unwrap();
        let after = bridge.run_steps(vec![]).await.unwrap();
        assert_eq!(after.nodes.len(), 2);
    }
}
