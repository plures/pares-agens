use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
use pluresdb_sync::{create_transport, GunMessage, Replicator, TransportConfig, TransportMode};
use tokio::sync::RwLock;

use super::{
    entry::{ChatTurn, MemoryEntry},
    Error,
};

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

    /// Persist a conversation turn.
    async fn insert_turn(&self, turn: ChatTurn) -> Result<(), Error>;

    /// Return the most recent `limit` conversation turns for `channel`,
    /// ordered oldest-first (chronological).
    async fn recent_turns(&self, channel: &str, limit: usize) -> Result<Vec<ChatTurn>, Error>;
}

/// Thread-safe in-memory store backed by a `RwLock<Vec<MemoryEntry>>`.
///
/// Suitable for tests and single-process deployments without PluresDB.
pub struct InMemoryStore {
    entries: RwLock<Vec<MemoryEntry>>,
    turns: RwLock<Vec<ChatTurn>>,
}

impl InMemoryStore {
    /// Create a new, empty store.
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(Vec::new()),
            turns: RwLock::new(Vec::new()),
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

    async fn insert_turn(&self, turn: ChatTurn) -> Result<(), Error> {
        self.turns.write().await.push(turn);
        Ok(())
    }

    async fn recent_turns(&self, channel: &str, limit: usize) -> Result<Vec<ChatTurn>, Error> {
        let turns = self.turns.read().await;
        let mut channel_turns: Vec<ChatTurn> = turns
            .iter()
            .filter(|t| t.channel == channel)
            .cloned()
            .collect();
        channel_turns.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        let start = channel_turns.len().saturating_sub(limit);
        Ok(channel_turns[start..].to_vec())
    }
}

// ---------------------------------------------------------------------------
// PluresDbStore
// ---------------------------------------------------------------------------

/// The PluresDB actor ID used for all write operations.
const ACTOR: &str = "pares-agens";

/// The PluresDB key prefix for conversation turn entries.
const TURN_PREFIX: &str = "turn:";

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
    store: Arc<CrdtStore>,
    _sync_task: Option<tokio::task::JoinHandle<()>>,
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
        Ok(Self {
            store: Arc::new(store),
            _sync_task: None,
        })
    }

    /// Open a PluresDB store with native fastembed embeddings.
    ///
    /// Every `put()` call automatically generates embeddings via
    /// BAAI/bge-small-en-v1.5 (384-dim, ONNX Runtime) and indexes them
    /// in HNSW for vector search.  A background worker processes the
    /// embedding queue.
    ///
    /// This is the recommended way to open a store for production use.
    #[cfg(feature = "embeddings")]
    pub fn open_with_embeddings(path: impl AsRef<Path>) -> Result<Self, Error> {
        use pluresdb::FastEmbedder;

        let storage: Arc<dyn StorageEngine> = Arc::new(
            SledStorage::open(&path).map_err(|e| Error::Store(format!("open failed: {e}")))?,
        );
        let embedder = FastEmbedder::new("BAAI/bge-small-en-v1.5")
            .map_err(|e| Error::Store(format!("embedder init failed: {e}")))?;
        let store = CrdtStore::default()
            .with_persistence(storage)
            .with_embedder(Arc::new(embedder));
        // Spawn the background embedding worker.
        // We need an Arc temporarily for the worker, then clone for our store.
        let arc_store = Arc::new(store);
        CrdtStore::spawn_embedding_worker(Arc::clone(&arc_store));
        tracing::info!(
            path = %path.as_ref().display(),
            "PluresDB opened with native fastembed (BAAI/bge-small-en-v1.5, 384-dim)"
        );
        // The worker holds an Arc, and we hold a clone of the CrdtStore.
        // CrdtStore is Clone — both share the same underlying data via Arc internals.
        Ok(Self {
            store: arc_store,
            _sync_task: None,
        })
    }

    /// Open a PluresDB store with Hyperswarm peer sync enabled.
    ///
    /// Joins the Hyperswarm DHT topic identified by `topic_key` so that peer
    /// instances sharing the same key will automatically replicate memory
    /// entries.  The local database is persisted at `path`.
    ///
    /// # Errors
    /// Returns [`Error::Store`] if the store cannot be opened or sync cannot
    /// be initialized.
    pub fn open_with_sync(path: impl AsRef<Path>, topic_key: &[u8; 32]) -> Result<Self, Error> {
        let mut store = Self::open(path)?;
        store._sync_task = Some(
            spawn_sync_task(Arc::clone(&store.store), *topic_key)
                .map_err(|e| Error::Store(format!("sync init failed: {e}")))?,
        );
        Ok(store)
    }

    /// Create an ephemeral in-memory PluresDB store.
    ///
    /// Useful for integration tests that need a real [`CrdtStore`] without
    /// touching the filesystem.
    pub fn in_memory() -> Self {
        let storage: Arc<dyn StorageEngine> = Arc::new(MemoryStorage::default());
        let store = CrdtStore::default().with_persistence(storage);
        Self {
            store: Arc::new(store),
            _sync_task: None,
        }
    }
}

fn spawn_sync_task(
    store: Arc<CrdtStore>,
    topic_key: [u8; 32],
) -> Result<tokio::task::JoinHandle<()>, String> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|e| format!("open_with_sync requires an active Tokio runtime: {e}"))?;
    Ok(runtime.spawn(async move {
        let mut transport = create_transport(TransportConfig {
            mode: TransportMode::Hyperswarm,
            ..Default::default()
        });
        let mut connections = match transport.connect(topic_key).await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::error!("failed to connect Hyperswarm transport: {e}");
                return;
            }
        };
        tracing::info!("PluresDbStore: Hyperswarm sync active");
        while let Some(mut connection) = connections.recv().await {
            if let Err(e) = sync_connection(Arc::clone(&store), &mut *connection).await {
                tracing::warn!("sync connection failed: {e}");
            }
        }
    }))
}

async fn sync_connection(
    store: Arc<CrdtStore>,
    connection: &mut dyn pluresdb_sync::Connection,
) -> Result<(), String> {
    let replicator = Replicator::new(ACTOR);
    for record in store.list() {
        let payload = replicator
            .encode_put(&record.id, record.data)
            .map_err(|e| format!("encode_put failed: {e}"))?;
        connection
            .send(&payload)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
    }
    connection
        .close()
        .await
        .map_err(|e| format!("close failed: {e}"))?;

    loop {
        let maybe_payload = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connection.receive(),
        )
        .await
        {
            Ok(result) => result.map_err(|e| format!("receive failed: {e}"))?,
            Err(_) => {
                tracing::debug!("sync receive timeout reached; ending peer sync loop");
                break;
            }
        };
        let Some(payload) = maybe_payload else {
            break;
        };
        let message = GunMessage::decode(&payload).map_err(|e| format!("decode failed: {e}"))?;
        if let GunMessage::Put(put) = message {
            for (id, node) in put.put {
                store.put(
                    id,
                    ACTOR,
                    serde_json::Value::Object(node.fields.into_iter().collect()),
                );
            }
        }
    }
    Ok(())
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
            // Skip conversation turn entries (prefixed with "turn:").
            if record.id.starts_with(TURN_PREFIX) {
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<MemoryEntry>(record.data) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    async fn remove(&self, id: &str) -> Result<bool, Error> {
        match self.store.delete(id) {
            Ok(()) => Ok(true),
            // StoreError::NotFound is the only variant — entry did not exist.
            Err(_) => Ok(false),
        }
    }

    async fn insert_turn(&self, turn: ChatTurn) -> Result<(), Error> {
        let key = format!("{TURN_PREFIX}{}", turn.id);
        let data = serde_json::to_value(&turn)
            .map_err(|e| Error::Store(format!("serialise turn failed: {e}")))?;
        // Turns don't need embeddings — they're retrieved by channel+time, not similarity.
        self.store.put(key, ACTOR, data);
        Ok(())
    }

    async fn recent_turns(&self, channel: &str, limit: usize) -> Result<Vec<ChatTurn>, Error> {
        let records = self.store.list();
        let mut turns: Vec<ChatTurn> = records
            .into_iter()
            .filter(|r| r.id.starts_with(TURN_PREFIX))
            .filter_map(|r| serde_json::from_value::<ChatTurn>(r.data).ok())
            .filter(|t| t.channel == channel)
            .collect();
        turns.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        let start = turns.len().saturating_sub(limit);
        Ok(turns[start..].to_vec())
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

    #[tokio::test]
    async fn pluresdb_store_open_with_sync_replicates_existing_entries() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let key = [7u8; 32];

        let store_a = PluresDbStore::open_with_sync(dir_a.path(), &key).unwrap();
        store_a
            .insert(make_entry("shared-1", "from-a"))
            .await
            .unwrap();

        let store_b = PluresDbStore::open_with_sync(dir_b.path(), &key).unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let all = store_b.all().await.unwrap();
            if all.iter().any(|entry| entry.id == "shared-1") {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("expected synced entry to replicate to peer store");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
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
