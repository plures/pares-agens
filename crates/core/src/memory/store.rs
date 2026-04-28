use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
use pluresdb_sea::{sea_decrypt_wire, sea_encrypt_wire, SeaKeyPair};
use pluresdb_sync::{create_transport, GunMessage, Replicator, TransportConfig, TransportMode};
use tokio::sync::RwLock;
use uuid::Uuid;

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
/// The PluresDB key prefix for per-host adapter configuration.
const HOST_PREFIX: &str = "host/";
/// The PluresDB key suffix for per-host adapter configuration.
const ADAPTERS_SUFFIX: &str = "/adapters";
/// The PluresDB key suffix for per-host inference capability advertisements.
const INFERENCE_CAPABILITIES_SUFFIX: &str = "/inference-capabilities";
const SEA_DATA_FIELD: &str = "_sea";
const SEA_HOST_KEY_FILE: &str = ".sea-host-key.json";
const SEA_SYNC_PAYLOAD_ENCODING: &str = "base64url";

/// Configuration for a single channel adapter connection.
///
/// This schema is adapter-agnostic so single-connection constraints can be
/// enforced for Telegram today and extended to Slack/Discord/WhatsApp later.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostAdapterConfig {
    /// Adapter kind, e.g. `"telegram"` or `"slack_rtm"`.
    pub kind: String,
    /// Stable connection identifier used for uniqueness checks.
    ///
    /// For Telegram this is the bot token.
    pub connection_id: String,
    /// Whether this adapter requires unique connection ownership in a swarm.
    #[serde(default)]
    pub single_connection: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HostAdaptersPayload {
    adapters: Vec<HostAdapterConfig>,
}

/// Distributed inference capability advertisement for a host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostInferenceCapability {
    /// Hostname or IP advertised for inference calls.
    pub host: String,
    /// Port exposed by the host's inference endpoint.
    pub port: u16,
    /// Expert names currently available on this host.
    #[serde(default)]
    pub experts: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HostInferenceCapabilityPayload {
    capability: HostInferenceCapability,
}

/// Adapter configuration snapshot for a specific host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAdapterRecord {
    /// Host identifier from the key path: `host/<hostname>/adapters`.
    pub host: String,
    /// Adapters configured for this host.
    pub adapters: Vec<HostAdapterConfig>,
}

/// Inference capability snapshot for a specific host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInferenceCapabilityRecord {
    /// Host identifier from the key path: `host/<hostname>/inference-capabilities`.
    pub host: String,
    /// Capability payload advertised by this host.
    pub capability: HostInferenceCapability,
}

/// Generate a new random Hyperswarm topic key as a 64-character lowercase hex string.
#[must_use]
pub fn generate_sync_topic_key_hex() -> String {
    let a = Uuid::new_v4().as_simple().to_string();
    let b = Uuid::new_v4().as_simple().to_string();
    format!("{a}{b}")
}

/// Parse a 32-byte Hyperswarm topic key from hex.
///
/// Accepts values with or without a leading `0x` prefix.
///
/// # Errors
/// Returns [`Error::Store`] when the value is not 64 hex characters.
pub fn parse_sync_topic_key_hex(raw: &str) -> Result<[u8; 32], Error> {
    let trimmed = raw.trim();
    let value = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if value.len() != 64 {
        return Err(Error::Store(
            "sync topic key must be 64 hex characters (32 bytes)".to_string(),
        ));
    }

    let mut key = [0u8; 32];
    for i in 0..32 {
        let pair = &value[(i * 2)..(i * 2 + 2)];
        key[i] = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::Store(format!("invalid hex byte at position {}: {pair}", i * 2)))?;
    }
    Ok(key)
}

/// Generate a new sync shared key payload accepted by [`PluresDbStore::open_with_sync`].
///
/// The result is a base64url-encoded JSON payload containing a generated SEA key pair.
pub fn generate_sync_shared_key() -> Result<String, Error> {
    let json = serde_json::to_vec(&SeaKeyPair::generate())
        .map_err(|e| Error::Store(format!("failed to serialise sync shared key: {e}")))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

/// Validate that `shared_key` is a parseable sync shared key.
///
/// # Errors
/// Returns [`Error::Store`] when the key is malformed.
pub fn validate_sync_shared_key(shared_key: &str) -> Result<(), Error> {
    parse_sea_key(shared_key).map(|_| ())
}

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
    host_sea_key: Option<SeaKeyPair>,
    _sync_task: Option<tokio::task::JoinHandle<()>>,
}

impl PluresDbStore {
    /// Return a reference to the underlying [`CrdtStore`].
    ///
    /// This is used by the event spine to initialize `AgensRuntime` on top of
    /// the same CRDT store that backs memory.
    pub fn crdt_store(&self) -> &CrdtStore {
        &self.store
    }

    /// Return a shared reference-counted handle to the underlying [`CrdtStore`].
    pub fn crdt_store_arc(&self) -> Arc<CrdtStore> {
        Arc::clone(&self.store)
    }

    /// Open or create a PluresDB-backed store at `path`.
    ///
    /// # Errors
    /// Returns [`Error::Store`] if the underlying [`SledStorage`] cannot be
    /// opened (e.g. permission denied, corrupted database).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let host_sea_key = load_or_create_host_sea_key(path.as_ref())?;
        let storage: Arc<dyn StorageEngine> = Arc::new(
            SledStorage::open(path).map_err(|e| Error::Store(format!("open failed: {e}")))?,
        );
        let store = CrdtStore::default().with_persistence(storage);
        Ok(Self {
            store: Arc::new(store),
            host_sea_key: Some(host_sea_key),
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

        let host_sea_key = load_or_create_host_sea_key(path.as_ref())?;
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
            host_sea_key: Some(host_sea_key),
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
    pub fn open_with_sync(
        path: impl AsRef<Path>,
        topic_key: &[u8; 32],
        shared_key: &str,
    ) -> Result<Self, Error> {
        let mut store = Self::open(path)?;
        let sync_sea_key = parse_sea_key(shared_key)?;
        let host_sea_key = store.host_sea_key.clone();
        store._sync_task = Some(
            spawn_sync_task(
                Arc::clone(&store.store),
                *topic_key,
                sync_sea_key,
                host_sea_key,
            )
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
            host_sea_key: None,
            _sync_task: None,
        }
    }

    /// Persist adapter configuration under `host/<hostname>/adapters`.
    pub async fn set_host_adapters(
        &self,
        hostname: &str,
        adapters: Vec<HostAdapterConfig>,
    ) -> Result<(), Error> {
        if hostname.trim().is_empty()
            || hostname.contains('/')
            || hostname.chars().any(|c| c.is_control())
        {
            return Err(Error::Store(
                "hostname must be non-empty and must not contain '/' or control characters".into(),
            ));
        }
        let payload = HostAdaptersPayload { adapters };
        let data = seal_value_for_storage(
            serde_json::to_value(payload)
                .map_err(|e| Error::Store(format!("serialise host adapters failed: {e}")))?,
            self.host_sea_key.as_ref(),
        )?;
        let key = format!("{HOST_PREFIX}{hostname}{ADAPTERS_SUFFIX}");
        self.store.put(key, ACTOR, data);
        Ok(())
    }

    /// List all host adapter configuration snapshots from PluresDB.
    pub async fn list_host_adapters(&self) -> Result<Vec<HostAdapterRecord>, Error> {
        let mut records = Vec::new();
        for record in self.store.list() {
            if !(record.id.starts_with(HOST_PREFIX) && record.id.ends_with(ADAPTERS_SUFFIX)) {
                continue;
            }
            let Some(host) = record
                .id
                .strip_prefix(HOST_PREFIX)
                .and_then(|id| id.strip_suffix(ADAPTERS_SUFFIX))
            else {
                continue;
            };
            let value = unseal_value_from_storage(record.data, self.host_sea_key.as_ref())?;
            if let Ok(payload) = serde_json::from_value::<HostAdaptersPayload>(value) {
                records.push(HostAdapterRecord {
                    host: host.to_string(),
                    adapters: payload.adapters,
                });
            }
        }
        Ok(records)
    }

    /// Persist distributed inference capability under
    /// `host/<hostname>/inference-capabilities`.
    pub async fn set_host_inference_capability(
        &self,
        hostname: &str,
        capability: HostInferenceCapability,
    ) -> Result<(), Error> {
        if hostname.trim().is_empty()
            || hostname.contains('/')
            || hostname.chars().any(|c| c.is_control())
        {
            return Err(Error::Store(
                "hostname must be non-empty and must not contain '/' or control characters".into(),
            ));
        }
        let payload = HostInferenceCapabilityPayload { capability };
        let data = seal_value_for_storage(
            serde_json::to_value(payload)
                .map_err(|e| Error::Store(format!("serialise host inference capability failed: {e}")))?,
            self.host_sea_key.as_ref(),
        )?;
        let key = format!("{HOST_PREFIX}{hostname}{INFERENCE_CAPABILITIES_SUFFIX}");
        self.store.put(key, ACTOR, data);
        Ok(())
    }

    /// List all host distributed inference capability snapshots from PluresDB.
    pub async fn list_host_inference_capabilities(
        &self,
    ) -> Result<Vec<HostInferenceCapabilityRecord>, Error> {
        let mut records = Vec::new();
        for record in self.store.list() {
            if !(record.id.starts_with(HOST_PREFIX)
                && record.id.ends_with(INFERENCE_CAPABILITIES_SUFFIX))
            {
                continue;
            }
            let Some(host) = record
                .id
                .strip_prefix(HOST_PREFIX)
                .and_then(|id| id.strip_suffix(INFERENCE_CAPABILITIES_SUFFIX))
            else {
                continue;
            };
            let value = unseal_value_from_storage(record.data, self.host_sea_key.as_ref())?;
            if let Ok(payload) = serde_json::from_value::<HostInferenceCapabilityPayload>(value) {
                records.push(HostInferenceCapabilityRecord {
                    host: host.to_string(),
                    capability: payload.capability,
                });
            }
        }
        Ok(records)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SeaSyncEnvelope {
    sender_epub: String,
    wire: String,
    payload_b64: String,
}

fn load_or_create_host_sea_key(path: &Path) -> Result<SeaKeyPair, Error> {
    std::fs::create_dir_all(path).map_err(|e| {
        Error::Store(format!(
            "failed to create store directory for host key {}: {e}",
            path.display()
        ))
    })?;
    let key_path = path.join(SEA_HOST_KEY_FILE);
    if key_path.exists() {
        let raw = std::fs::read_to_string(&key_path).map_err(|e| {
            Error::Store(format!(
                "failed to read host key {}: {e}",
                key_path.display()
            ))
        })?;
        return serde_json::from_str::<SeaKeyPair>(&raw).map_err(|e| {
            Error::Store(format!(
                "failed to parse host key {}: {e}",
                key_path.display()
            ))
        });
    }

    let pair = SeaKeyPair::generate();
    let raw = serde_json::to_string(&pair).map_err(|e| {
        Error::Store(format!(
            "failed to serialise generated host key {}: {e}",
            key_path.display()
        ))
    })?;
    std::fs::write(&key_path, raw).map_err(|e| {
        Error::Store(format!(
            "failed to persist host key {}: {e}",
            key_path.display()
        ))
    })?;
    Ok(pair)
}

fn parse_sea_key(shared_key: &str) -> Result<SeaKeyPair, Error> {
    if let Ok(pair) = serde_json::from_str::<SeaKeyPair>(shared_key) {
        return Ok(pair);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(shared_key)
        .map_err(|e| Error::Store(format!("invalid sync shared key encoding: {e}")))?;
    let json = String::from_utf8(decoded)
        .map_err(|e| Error::Store(format!("invalid sync shared key UTF-8: {e}")))?;
    serde_json::from_str::<SeaKeyPair>(&json)
        .map_err(|e| Error::Store(format!("invalid sync shared key payload: {e}")))
}

fn seal_value_for_storage(
    value: serde_json::Value,
    host_key: Option<&SeaKeyPair>,
) -> Result<serde_json::Value, Error> {
    match host_key {
        None => Ok(value),
        Some(key) => {
            let plaintext = serde_json::to_string(&value).map_err(|e| {
                Error::Store(format!("failed to serialise value for encryption: {e}"))
            })?;
            let wire = sea_encrypt_wire(&plaintext, key, &key.epub)
                .map_err(|e| Error::Store(format!("failed to encrypt value for storage: {e}")))?;
            Ok(serde_json::json!({ SEA_DATA_FIELD: wire }))
        }
    }
}

fn unseal_value_from_storage(
    value: serde_json::Value,
    host_key: Option<&SeaKeyPair>,
) -> Result<serde_json::Value, Error> {
    match host_key {
        None => Ok(value),
        Some(key) => {
            let Some(wire) = value
                .get(SEA_DATA_FIELD)
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(value);
            };
            let plaintext = sea_decrypt_wire(wire, key, &key.epub)
                .map_err(|e| Error::Store(format!("failed to decrypt value from storage: {e}")))?;
            serde_json::from_str(&plaintext)
                .map_err(|e| Error::Store(format!("failed to parse decrypted value JSON: {e}")))
        }
    }
}

fn encrypt_sync_payload(payload: &[u8], sync_sea_key: &SeaKeyPair) -> Result<Vec<u8>, String> {
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload);
    let wire = sea_encrypt_wire(&payload_b64, sync_sea_key, &sync_sea_key.epub)
        .map_err(|e| format!("SEA payload encryption failed: {e}"))?;
    serde_json::to_vec(&SeaSyncEnvelope {
        sender_epub: sync_sea_key.epub.clone(),
        wire,
        payload_b64: SEA_SYNC_PAYLOAD_ENCODING.to_string(),
    })
    .map_err(|e| format!("SEA sync envelope serialise failed: {e}"))
}

fn decrypt_sync_payload(payload: &[u8], sync_sea_key: &SeaKeyPair) -> Result<Vec<u8>, String> {
    let envelope: SeaSyncEnvelope = serde_json::from_slice(payload)
        .map_err(|e| format!("SEA sync envelope parse failed: {e}"))?;
    if envelope.payload_b64 != SEA_SYNC_PAYLOAD_ENCODING {
        return Err(format!(
            "unsupported SEA sync payload encoding: {}",
            envelope.payload_b64
        ));
    }
    let plaintext_b64 = sea_decrypt_wire(&envelope.wire, sync_sea_key, &envelope.sender_epub)
        .map_err(|e| format!("SEA payload decryption failed: {e}"))?;
    URL_SAFE_NO_PAD
        .decode(plaintext_b64)
        .map_err(|e| format!("sync payload base64 decode failed: {e}"))
}

fn spawn_sync_task(
    store: Arc<CrdtStore>,
    topic_key: [u8; 32],
    sync_sea_key: SeaKeyPair,
    host_sea_key: Option<SeaKeyPair>,
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
            if let Err(e) = sync_connection(
                Arc::clone(&store),
                &mut *connection,
                &sync_sea_key,
                host_sea_key.as_ref(),
            )
            .await
            {
                tracing::warn!("sync connection failed: {e}");
            }
        }
    }))
}

async fn sync_connection(
    store: Arc<CrdtStore>,
    connection: &mut dyn pluresdb_sync::Connection,
    sync_sea_key: &SeaKeyPair,
    host_sea_key: Option<&SeaKeyPair>,
) -> Result<(), String> {
    let replicator = Replicator::new(ACTOR);
    for record in store.list() {
        let data = unseal_value_from_storage(record.data, host_sea_key)
            .map_err(|e| format!("unseal local value failed before sync: {e}"))?;
        let payload = replicator
            .encode_put(&record.id, data)
            .map_err(|e| format!("encode_put failed: {e}"))?;
        let encrypted_payload = encrypt_sync_payload(&payload, sync_sea_key)?;
        connection
            .send(&encrypted_payload)
            .await
            .map_err(|e| format!("send failed: {e}"))?;
    }
    connection
        .close()
        .await
        .map_err(|e| format!("close failed: {e}"))?;

    loop {
        let maybe_payload =
            match tokio::time::timeout(std::time::Duration::from_secs(5), connection.receive())
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
        let decrypted_payload = decrypt_sync_payload(&payload, sync_sea_key)?;
        let message =
            GunMessage::decode(&decrypted_payload).map_err(|e| format!("decode failed: {e}"))?;
        if let GunMessage::Put(put) = message {
            for (id, node) in put.put {
                let value = seal_value_for_storage(
                    serde_json::Value::Object(node.fields.into_iter().collect()),
                    host_sea_key,
                )
                .map_err(|e| format!("seal synced value for local storage failed: {e}"))?;
                store.put(id, ACTOR, value);
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
        let data = seal_value_for_storage(
            serde_json::to_value(&entry)
                .map_err(|e| Error::Store(format!("serialise failed: {e}")))?,
            self.host_sea_key.as_ref(),
        )?;
        self.store.put_with_embedding(id, ACTOR, data, embedding);
        Ok(())
    }

    async fn all(&self) -> Result<Vec<MemoryEntry>, Error> {
        let records = self.store.list();
        let mut entries = Vec::with_capacity(records.len());
        for record in records {
            let value = unseal_value_from_storage(record.data, self.host_sea_key.as_ref())?;
            // Skip conversation turn entries (prefixed with "turn:").
            if record.id.starts_with(TURN_PREFIX) {
                continue;
            }
            if let Ok(entry) = serde_json::from_value::<MemoryEntry>(value) {
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
        let data = seal_value_for_storage(
            serde_json::to_value(&turn)
                .map_err(|e| Error::Store(format!("serialise turn failed: {e}")))?,
            self.host_sea_key.as_ref(),
        )?;
        // Turns don't need embeddings — they're retrieved by channel+time, not similarity.
        self.store.put(key, ACTOR, data);
        Ok(())
    }

    async fn recent_turns(&self, channel: &str, limit: usize) -> Result<Vec<ChatTurn>, Error> {
        let records = self.store.list();
        let mut turns = Vec::new();
        for record in records
            .into_iter()
            .filter(|r| r.id.starts_with(TURN_PREFIX))
        {
            let value = unseal_value_from_storage(record.data, self.host_sea_key.as_ref())?;
            if let Ok(turn) = serde_json::from_value::<ChatTurn>(value) {
                if turn.channel == channel {
                    turns.push(turn);
                }
            }
        }
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
    use std::time::Duration;
    const SYNC_TIMEOUT_SECS: u64 = 10;

    fn encode_shared_key(pair: &SeaKeyPair) -> String {
        let json = serde_json::to_vec(pair).unwrap();
        URL_SAFE_NO_PAD.encode(json)
    }

    #[test]
    fn generate_sync_topic_key_hex_produces_64_hex_chars() {
        let topic = generate_sync_topic_key_hex();
        assert_eq!(topic.len(), 64);
        assert!(topic.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(topic, topic.to_lowercase(), "topic hex should be lowercase");
    }

    #[test]
    fn parse_sync_topic_key_hex_accepts_prefixed_and_plain() {
        let topic = generate_sync_topic_key_hex();
        let plain = parse_sync_topic_key_hex(&topic).unwrap();
        let prefixed = parse_sync_topic_key_hex(&format!("0x{topic}")).unwrap();
        assert_eq!(plain, prefixed);
    }

    #[test]
    fn parse_sync_topic_key_hex_rejects_bad_input() {
        let err = parse_sync_topic_key_hex("xyz").unwrap_err().to_string();
        assert!(err.contains("64 hex characters"), "unexpected error: {err}");
    }

    #[test]
    fn generate_and_validate_sync_shared_key_round_trip() {
        let key = generate_sync_shared_key().unwrap();
        validate_sync_shared_key(&key).unwrap();
    }

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
        let sync_key = encode_shared_key(&SeaKeyPair::generate());
        let store = PluresDbStore::open_with_sync(dir.path(), &key, &sync_key).unwrap();
        store.insert(make_entry("s1", "synced")).await.unwrap();
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
    }

    #[tokio::test]
    async fn pluresdb_store_open_with_sync_replicates_existing_entries() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let key = [7u8; 32];
        let sync_key = encode_shared_key(&SeaKeyPair::generate());

        let store_a = PluresDbStore::open_with_sync(dir_a.path(), &key, &sync_key).unwrap();
        store_a
            .insert(make_entry("shared-1", "from-a"))
            .await
            .unwrap();

        let store_b = PluresDbStore::open_with_sync(dir_b.path(), &key, &sync_key).unwrap();

        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(SYNC_TIMEOUT_SECS);
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

    #[tokio::test]
    async fn pluresdb_store_open_with_sync_rejects_wrong_shared_key() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let topic = [11u8; 32];
        let sync_key_a = encode_shared_key(&SeaKeyPair::generate());
        let sync_key_b = encode_shared_key(&SeaKeyPair::generate());

        let store_a = PluresDbStore::open_with_sync(dir_a.path(), &topic, &sync_key_a).unwrap();
        store_a
            .insert(make_entry("shared-2", "from-a"))
            .await
            .unwrap();
        let store_b = PluresDbStore::open_with_sync(dir_b.path(), &topic, &sync_key_b).unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let all = store_b.all().await.unwrap();
            if all.iter().any(|entry| entry.id == "shared-2") {
                panic!("peer without shared key should not decrypt replicated payloads");
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn pluresdb_store_persists_encrypted_values_at_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluresDbStore::open(dir.path()).unwrap();
        store
            .insert(make_entry("enc-1", "highly-sensitive-memory"))
            .await
            .unwrap();

        // Read raw sled files directly to verify plaintext is not present on disk.
        let contains_plaintext = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .any(|bytes| String::from_utf8_lossy(&bytes).contains("highly-sensitive-memory"));
        assert!(
            !contains_plaintext,
            "sled value should be encrypted at rest"
        );
    }

    #[tokio::test]
    async fn pluresdb_store_reopen_decrypts_existing_values() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = PluresDbStore::open(dir.path()).unwrap();
            store
                .insert(make_entry("persist-1", "persisted"))
                .await
                .unwrap();
        }
        let reopened = PluresDbStore::open(dir.path()).unwrap();
        let all = reopened.all().await.unwrap();
        assert!(all.iter().any(|entry| entry.id == "persist-1"));
    }

    #[tokio::test]
    async fn pluresdb_store_set_and_list_host_adapters_roundtrip() {
        let store = PluresDbStore::in_memory();
        let adapters = vec![HostAdapterConfig {
            kind: "telegram".to_string(),
            connection_id: "123:abc".to_string(),
            single_connection: true,
        }];
        store
            .set_host_adapters("host-a", adapters.clone())
            .await
            .unwrap();
        let configs = store.list_host_adapters().await.unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].host, "host-a");
        assert_eq!(configs[0].adapters, adapters);
    }

    #[tokio::test]
    async fn pluresdb_store_list_host_adapters_ignores_memory_entries() {
        let store = PluresDbStore::in_memory();
        store.insert(make_entry("m1", "memory")).await.unwrap();
        let configs = store.list_host_adapters().await.unwrap();
        assert!(configs.is_empty());
    }

    #[tokio::test]
    async fn pluresdb_store_set_and_list_host_inference_capabilities_roundtrip() {
        let store = PluresDbStore::in_memory();
        let capability = HostInferenceCapability {
            host: "10.0.0.10".to_string(),
            port: 8081,
            experts: vec!["routing".to_string(), "monitoring".to_string()],
        };
        store
            .set_host_inference_capability("host-a", capability.clone())
            .await
            .unwrap();
        let configs = store.list_host_inference_capabilities().await.unwrap();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].host, "host-a");
        assert_eq!(configs[0].capability, capability);
    }

    #[tokio::test]
    async fn pluresdb_store_list_host_inference_capabilities_ignores_other_nodes() {
        let store = PluresDbStore::in_memory();
        store.insert(make_entry("m2", "memory")).await.unwrap();
        store
            .set_host_adapters(
                "host-b",
                vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "abc".to_string(),
                    single_connection: true,
                }],
            )
            .await
            .unwrap();

        let configs = store.list_host_inference_capabilities().await.unwrap();
        assert!(configs.is_empty());
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
