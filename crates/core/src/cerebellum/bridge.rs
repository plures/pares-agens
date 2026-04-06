//! Bridge between pares-agens cerebellum and PluresDB procedure engine.
//!
//! The bridge exposes two execution paths:
//!
//! - [`PluresDbBridge::run_steps`] — execute a raw pipeline of [`Step`]s.
//! - [`PluresDbBridge::run_procedure`] — execute a named procedure by looking
//!   up its DSL string in the in-process procedure registry.
//!
//! # Store adapter
//!
//! [`StoreAdapter`] populates an in-process [`CrdtStore`] from a pares-agens
//! [`MemoryStore`], making the full corpus of memory entries available to the
//! PluresDB procedure engine without any network hop.
//!
//! # Example
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use pares_agens_core::cerebellum::bridge::{PluresDbBridge, BridgeError};
//! # use pares_agens_core::memory::store::InMemoryStore;
//! # #[tokio::main] async fn main() -> Result<(), BridgeError> {
//! let store = Arc::new(InMemoryStore::new());
//! let bridge = PluresDbBridge::new(store).await?;
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

use std::collections::HashMap;
use std::sync::Arc;

use pluresdb::CrdtStore;
use pluresdb_procedures::{
    engine::ProcedureEngine,
    ir::{ProcedureResult, Step},
};

use crate::memory::store::MemoryStore;

// ---------------------------------------------------------------------------
// BridgeError
// ---------------------------------------------------------------------------

/// Errors that can occur when using the PluresDB bridge.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// A named procedure was requested but could not be found in the registry.
    #[error("procedure not found: {0}")]
    NotFound(String),
    /// The procedure engine reported an execution failure.
    #[error("execution failed: {0}")]
    Execution(String),
    /// The underlying memory store returned an error.
    #[error("store error: {0}")]
    Store(String),
}

// ---------------------------------------------------------------------------
// StoreAdapter
// ---------------------------------------------------------------------------

/// Populates a [`CrdtStore`] with entries from a pares-agens [`MemoryStore`].
///
/// This adapter is the glue layer between the pares-agens memory subsystem and
/// the PluresDB procedure engine.  The [`CrdtStore`] it builds is a snapshot
/// of the pares-agens store at the moment [`StoreAdapter::load`] is called.
/// Subsequent writes to the pares-agens store are not reflected automatically;
/// callers should construct a fresh [`PluresDbBridge`] (or call
/// [`PluresDbBridge::reload`]) when a fresh view is required.
pub struct StoreAdapter {
    inner: Arc<dyn MemoryStore>,
}

impl StoreAdapter {
    /// Create a new adapter wrapping the given pares-agens memory store.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { inner: store }
    }

    /// Load all memory entries from the pares-agens store into a freshly
    /// created [`CrdtStore`] and return it.
    ///
    /// Each [`MemoryEntry`][crate::memory::entry::MemoryEntry] is stored as a
    /// JSON node under its own ID with actor `"pares-agens"`.  The `category`,
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
    /// Returns [`BridgeError::Store`] if the underlying pares-agens store
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
                crdt.put(&entry.id, "pares-agens", data);
            } else {
                crdt.put_with_embedding(&entry.id, "pares-agens", data, embedding);
            }
        }

        Ok(crdt)
    }
}

// ---------------------------------------------------------------------------
// PluresDbBridge
// ---------------------------------------------------------------------------

/// Bridge between the pares-agens cerebellum and the PluresDB procedure engine.
///
/// The bridge maintains:
/// - An in-process [`CrdtStore`] snapshot of the pares-agens memory store.
/// - A named-procedure registry (mapping procedure names to DSL strings).
/// - A reference to the pares-agens [`MemoryStore`] for reloading on demand.
pub struct PluresDbBridge {
    crdt: CrdtStore,
    store: Arc<dyn MemoryStore>,
    /// Named procedure registry: maps a procedure name to its DSL string.
    procedures: HashMap<String, String>,
}

impl PluresDbBridge {
    /// Create a bridge connected to the given pares-agens memory store.
    ///
    /// All existing memory entries are loaded into the internal [`CrdtStore`]
    /// immediately so they are available to procedure pipelines.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Store`] if the initial load fails.
    pub async fn new(store: Arc<dyn MemoryStore>) -> Result<Self, BridgeError> {
        let adapter = StoreAdapter::new(Arc::clone(&store));
        let crdt = adapter.load().await?;
        Ok(Self {
            crdt,
            store,
            procedures: HashMap::new(),
        })
    }

    /// Register a named procedure DSL string.
    ///
    /// Replaces any existing registration for `name`.
    pub fn register_procedure(&mut self, name: impl Into<String>, dsl: impl Into<String>) {
        self.procedures.insert(name.into(), dsl.into());
    }

    /// Reload the internal [`CrdtStore`] snapshot from the pares-agens store.
    ///
    /// Call this when the pares-agens store has been updated and you want the
    /// bridge to reflect the latest entries.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Store`] if the reload fails.
    pub async fn reload(&mut self) -> Result<(), BridgeError> {
        let adapter = StoreAdapter::new(Arc::clone(&self.store));
        self.crdt = adapter.load().await?;
        Ok(())
    }

    /// Run a named procedure pipeline.
    ///
    /// Looks up `name` in the registered procedure DSL table and executes the
    /// corresponding DSL string against the current [`CrdtStore`] snapshot.
    ///
    /// # Errors
    ///
    /// - [`BridgeError::NotFound`] if `name` is not registered.
    /// - [`BridgeError::Execution`] if the DSL parse or execution fails.
    pub async fn run_procedure(&self, name: &str) -> Result<ProcedureResult, BridgeError> {
        let dsl = self
            .procedures
            .get(name)
            .ok_or_else(|| BridgeError::NotFound(name.to_string()))?;

        let engine = ProcedureEngine::new(&self.crdt, "pares-agens");
        engine
            .exec_dsl(dsl)
            .map_err(|e| BridgeError::Execution(e.to_string()))
    }

    /// Run a raw pipeline of [`Step`]s against the current [`CrdtStore`] snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::Execution`] if the pipeline fails.
    pub async fn run_steps(&self, steps: Vec<Step>) -> Result<ProcedureResult, BridgeError> {
        let engine = ProcedureEngine::new(&self.crdt, "pares-agens");
        engine
            .exec(&steps)
            .map_err(|e| BridgeError::Execution(e.to_string()))
    }

    /// Load praxis constraints stored in PluresDB.
    ///
    /// Queries for nodes with type `praxis:constraint` and deserializes them
    /// into [`Constraint`] records.  Returns an empty vec if no constraints
    /// are stored (the caller should merge with seed constraints).
    pub fn load_constraints(
        &self,
    ) -> Result<Vec<pares_agens_praxis::db::schema::Constraint>, BridgeError> {
        use pluresdb_procedures::ir::{Predicate, Step};

        let steps = vec![Step::Filter {
            predicate: Predicate::eq("type", "praxis:constraint"),
        }];

        let engine = ProcedureEngine::new(&self.crdt, "pares-agens");
        let result = engine
            .exec(&steps)
            .map_err(|e| BridgeError::Execution(e.to_string()))?;

        let mut constraints = Vec::new();
        for node in &result.nodes {
            // Each node is a serde_json::Value; try to deserialize the constraint data
            match serde_json::from_value::<pares_agens_praxis::db::schema::Constraint>(node.clone())
            {
                Ok(c) => constraints.push(c),
                Err(e) => {
                    tracing::warn!("failed to deserialize praxis constraint from node: {e}");
                }
            }
        }

        Ok(constraints)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::entry::{MemoryCategory, MemoryEntry};
    use crate::memory::store::InMemoryStore;
    use pluresdb_procedures::ir::{Predicate, SortDir, Step};

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

    async fn bridge_with_entries() -> PluresDbBridge {
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
        PluresDbBridge::new(store).await.unwrap()
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

    // ── PluresDbBridge::run_steps ─────────────────────────────────────────────

    #[tokio::test]
    async fn run_steps_filter_by_category() {
        let bridge = bridge_with_entries().await;
        let steps = vec![Step::Filter {
            predicate: Predicate::eq("category", "decision"),
        }];
        let result = bridge.run_steps(steps).await.unwrap();
        assert_eq!(result.nodes.len(), 2, "expected 2 decision entries");
    }

    #[tokio::test]
    async fn run_steps_filter_sort_limit() {
        let bridge = bridge_with_entries().await;
        let steps = vec![
            Step::Filter {
                predicate: Predicate::eq("category", "decision"),
            },
            Step::Sort {
                by: "score".to_string(),
                dir: SortDir::Desc,
                after: None,
            },
            Step::Limit { n: 1 },
        ];
        let result = bridge.run_steps(steps).await.unwrap();
        assert_eq!(result.nodes.len(), 1);
    }

    #[tokio::test]
    async fn run_steps_empty_pipeline_returns_all() {
        let bridge = bridge_with_entries().await;
        let result = bridge.run_steps(vec![]).await.unwrap();
        assert_eq!(result.nodes.len(), 3);
    }

    // ── PluresDbBridge::run_procedure ─────────────────────────────────────────

    #[tokio::test]
    async fn run_procedure_not_found_returns_error() {
        let bridge = bridge_with_entries().await;
        let err = bridge.run_procedure("unknown-proc").await.unwrap_err();
        assert!(
            matches!(err, BridgeError::NotFound(_)),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn run_procedure_registered_dsl_executes() {
        let mut bridge = bridge_with_entries().await;
        bridge.register_procedure(
            "recall-decisions",
            r#"filter(category == "decision") |> limit(10)"#,
        );
        let result = bridge.run_procedure("recall-decisions").await.unwrap();
        assert_eq!(result.nodes.len(), 2);
    }

    #[tokio::test]
    async fn run_procedure_invalid_dsl_returns_execution_error() {
        let mut bridge = bridge_with_entries().await;
        bridge.register_procedure("bad", "this is not valid DSL !!!");
        let err = bridge.run_procedure("bad").await.unwrap_err();
        assert!(
            matches!(err, BridgeError::Execution(_)),
            "unexpected error: {err}"
        );
    }

    // ── PluresDbBridge::reload ────────────────────────────────────────────────

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

        let mut bridge = PluresDbBridge::new(Arc::clone(&store) as Arc<dyn MemoryStore>)
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
        bridge.reload().await.unwrap();
        let after = bridge.run_steps(vec![]).await.unwrap();
        assert_eq!(after.nodes.len(), 2);
    }
}
