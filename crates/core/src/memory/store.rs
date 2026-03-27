use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
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

    /// Remove a memory entry by ID.  Returns `true` if the entry existed.
    async fn remove(&self, id: &str) -> Result<bool, Error>;
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

    async fn remove(&self, id: &str) -> Result<bool, Error> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|e| e.id != id);
        Ok(entries.len() < before)
    }
}

// ---------------------------------------------------------------------------
// PluresDbStore
// ---------------------------------------------------------------------------

/// The PluresDB actor ID used for all write operations.
const ACTOR: &str = "pares-agens";

/// A [`MemoryStore`] backed by a PluresDB [`CrdtStore`].
///
/// Uses [`SledStorage`] for durable on-disk persistence when opened via
/// [`PluresDbStore::open`].  An ephemeral variant (backed by
/// [`MemoryStorage`]) is available via [`PluresDbStore::in_memory`].
///
/// Memory entries are serialised to JSON and stored as node payloads inside
/// PluresDB.  The embedding vector is stored both in the payload (for
/// round-trip fidelity) **and** in the HNSW vector index (via
/// [`CrdtStore::put_with_embedding`]) so that future vector-search queries can
/// leverage the index directly.
pub struct PluresDbStore {
    store: CrdtStore,
}

impl PluresDbStore {
    /// Open or create a PluresDB-backed store at `path`.
    ///
    /// # Errors
    /// Returns [`Error::Store`] if the underlying [`SledStorage`] cannot be
    /// opened (e.g. permission denied, corrupted database).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let storage: Arc<dyn StorageEngine> = Arc::new(
            SledStorage::open(path).map_err(|e| Error::Store(format!("open failed: {e}")))?,
        );
        let store = CrdtStore::default().with_persistence(storage);
        Ok(Self { store })
    }

    /// Open a PluresDB store with Hyperswarm peer sync enabled.
    ///
    /// Joins the Hyperswarm DHT topic identified by `topic_key` so that peer
    /// instances sharing the same key will automatically replicate memory
    /// entries.  The local database is persisted at `path`.
    ///
    /// > **Note:** Full Hyperswarm transport is pending the `hyperswarm-rs`
    /// > integration in `pluresdb-sync`.  Until that integration lands this
    /// > constructor behaves identically to [`open`][Self::open] but is
    /// > provided now so call-sites do not need to change later.
    ///
    /// # Errors
    /// Returns [`Error::Store`] if the store cannot be opened.
    pub fn open_with_sync(path: impl AsRef<Path>, _topic_key: &[u8; 32]) -> Result<Self, Error> {
        // Hyperswarm DHT transport is a stub inside pluresdb-sync while the
        // hyperswarm-rs crate is being finalised.  We open the persistent store
        // normally; the sync layer will be wired in transparently once the
        // transport implementation lands.
        tracing::info!(
            "PluresDbStore: Hyperswarm sync requested; \
             transport stub active — opening persistent store only"
        );
        Self::open(path)
    }

    /// Create an ephemeral in-memory PluresDB store.
    ///
    /// Useful for integration tests that need a real [`CrdtStore`] without
    /// touching the filesystem.
    pub fn in_memory() -> Self {
        let storage: Arc<dyn StorageEngine> = Arc::new(MemoryStorage::default());
        let store = CrdtStore::default().with_persistence(storage);
        Self { store }
    }
}

#[async_trait]
impl MemoryStore for PluresDbStore {
    async fn insert(&self, entry: MemoryEntry) -> Result<(), Error> {
        let id = entry.id.clone();
        let embedding = entry.embedding.clone();
        let data = serde_json::to_value(&entry)
            .map_err(|e| Error::Store(format!("serialise failed: {e}")))?;
        self.store.put_with_embedding(id, ACTOR, data, embedding);
        Ok(())
    }

    async fn all(&self) -> Result<Vec<MemoryEntry>, Error> {
        let records = self.store.list();
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            let entry = serde_json::from_value::<MemoryEntry>(record.data)
                .map_err(|e| Error::Store(format!("deserialise failed: {e}")))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    async fn remove(&self, id: &str) -> Result<bool, Error> {
        self.store
            .delete(id)
            .map(|_| true)
            .or_else(|_| Ok(false))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::entry::MemoryCategory;

    fn make_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![0.1_f32, 0.2, 0.3],
            score: 0.0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    // ── InMemoryStore (existing behaviour preserved) ──────────────────────

    #[tokio::test]
    async fn in_memory_store_insert_and_all() {
        let store = InMemoryStore::new();
        store.insert(make_entry("a", "alpha")).await.unwrap();
        store.insert(make_entry("b", "beta")).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn in_memory_store_default_is_empty() {
        let store = InMemoryStore::default();
        assert!(store.all().await.unwrap().is_empty());
    }

    // ── PluresDbStore ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn pluresdb_store_insert_and_all() {
        let store = PluresDbStore::in_memory();
        store.insert(make_entry("1", "first entry")).await.unwrap();
        store.insert(make_entry("2", "second entry")).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn pluresdb_store_roundtrip_preserves_fields() {
        let store = PluresDbStore::in_memory();
        let entry = MemoryEntry {
            id: "rt-1".to_string(),
            content: "roundtrip test".to_string(),
            category: MemoryCategory::CodePattern,
            tags: vec!["lang:rust".to_string()],
            embedding: vec![0.5, 0.5],
            score: 0.0,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        store.insert(entry.clone()).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
        let got = &all[0];
        assert_eq!(got.id, entry.id);
        assert_eq!(got.content, entry.content);
        assert_eq!(got.category, entry.category);
        assert_eq!(got.tags, entry.tags);
        assert_eq!(got.embedding, entry.embedding);
    }

    #[tokio::test]
    async fn pluresdb_store_empty_initially() {
        let store = PluresDbStore::in_memory();
        assert!(store.all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pluresdb_store_open_creates_persistent_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluresDbStore::open(dir.path()).unwrap();
        store.insert(make_entry("p1", "persistent")).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn pluresdb_store_open_with_sync_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let key = [0u8; 32];
        let store = PluresDbStore::open_with_sync(dir.path(), &key).unwrap();
        store.insert(make_entry("s1", "synced")).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
    }

    // ── remove ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn in_memory_store_remove_existing() {
        let store = InMemoryStore::new();
        store.insert(make_entry("a", "alpha")).await.unwrap();
        store.insert(make_entry("b", "beta")).await.unwrap();
        let removed = store.remove("a").await.unwrap();
        assert!(removed);
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "b");
    }

    #[tokio::test]
    async fn in_memory_store_remove_nonexistent() {
        let store = InMemoryStore::new();
        let removed = store.remove("nope").await.unwrap();
        assert!(!removed);
    }

    #[tokio::test]
    async fn pluresdb_store_remove_existing() {
        let store = PluresDbStore::in_memory();
        store.insert(make_entry("1", "first")).await.unwrap();
        let removed = store.remove("1").await.unwrap();
        assert!(removed);
        assert!(store.all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pluresdb_store_remove_nonexistent() {
        let store = PluresDbStore::in_memory();
        let removed = store.remove("nope").await.unwrap();
        assert!(!removed);
    }
}
