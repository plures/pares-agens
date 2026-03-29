//! Append-only audit store trait and in-memory / PluresDB implementations.
//!
//! [`AuditStore`] is the backing-store abstraction for the audit log.  The
//! design deliberately mirrors the existing `MemoryStore` pattern so that
//! adopters can swap implementations without changing call sites.
//!
//! Two implementations are provided:
//!
//! * [`InMemoryAuditStore`] — keeps all events in a `RwLock<Vec<AuditEvent>>`.
//!   Suitable for tests and single-process deployments where persistence is
//!   handled by the caller.
//! * [`PluresDbAuditStore`] — persists events durably in a PluresDB
//!   [`CrdtStore`].  Use [`PluresDbAuditStore::open`] for on-disk storage and
//!   [`PluresDbAuditStore::in_memory`] for ephemeral (test) use.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use pluresdb::{CrdtStore, MemoryStorage, SledStorage, StorageEngine};
use tokio::sync::RwLock;

use crate::event::AuditEvent;
use crate::query::AuditQuery;

// ---------------------------------------------------------------------------
// AuditStore trait
// ---------------------------------------------------------------------------

/// Backing-store abstraction for the comprehensive audit log.
///
/// Implementations **must** be append-only: once an [`AuditEvent`] has been
/// stored it must never be modified or removed (except via the retention API
/// in [`crate::retention`]).
#[async_trait]
pub trait AuditStore: Send + Sync {
    /// Append a single event to the store.
    async fn append(&self, event: AuditEvent);

    /// Return all events that match `query` in chronological order.
    async fn query(&self, query: &AuditQuery) -> Vec<AuditEvent>;

    /// Return every event in the store, in insertion order.
    async fn all(&self) -> Vec<AuditEvent>;

    /// Total number of events currently in the store.
    async fn len(&self) -> usize;

    /// `true` when the store has no events.
    async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    /// Remove events that are older than the retention window.
    ///
    /// The default implementation is a no-op.  Persistent back-ends should
    /// override this to actually delete rows.
    async fn purge_before(&self, _cutoff_rfc3339: &str) {}
}

// ---------------------------------------------------------------------------
// InMemoryAuditStore
// ---------------------------------------------------------------------------

/// Thread-safe, append-only in-memory audit store.
///
/// All events are held in a `RwLock<Vec<AuditEvent>>`.  This is the reference
/// implementation used by tests and single-node deployments.
#[derive(Default)]
pub struct InMemoryAuditStore {
    events: RwLock<Vec<AuditEvent>>,
}

impl InMemoryAuditStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap the store in an [`Arc`] for shared ownership.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

#[async_trait]
impl AuditStore for InMemoryAuditStore {
    async fn append(&self, event: AuditEvent) {
        self.events.write().await.push(event);
    }

    async fn query(&self, query: &AuditQuery) -> Vec<AuditEvent> {
        self.events
            .read()
            .await
            .iter()
            .filter(|e| query.matches(e))
            .cloned()
            .collect()
    }

    async fn all(&self) -> Vec<AuditEvent> {
        self.events.read().await.clone()
    }

    async fn len(&self) -> usize {
        self.events.read().await.len()
    }

    async fn purge_before(&self, cutoff_rfc3339: &str) {
        let mut events = self.events.write().await;
        events.retain(|e| e.timestamp.as_str() >= cutoff_rfc3339);
    }
}

// ---------------------------------------------------------------------------
// PluresDbAuditStore
// ---------------------------------------------------------------------------

/// The PluresDB actor ID used for all write operations.
const AUDIT_ACTOR: &str = "pares-agens-audit";

/// A [`AuditStore`] backed by a PluresDB [`CrdtStore`].
///
/// Uses [`SledStorage`] for durable on-disk persistence when opened via
/// [`PluresDbAuditStore::open`].  An ephemeral variant (backed by
/// [`MemoryStorage`]) is available via [`PluresDbAuditStore::in_memory`] and
/// is useful for integration tests.
///
/// Audit events are serialised to JSON and stored as node payloads inside
/// PluresDB, keyed by the event's UUID.
///
/// # Persistence
///
/// ```rust,no_run
/// # use pares_agens_audit::{
/// #     event::{AuditEvent, EventKind},
/// #     store::{AuditStore, PluresDbAuditStore},
/// # };
/// # #[tokio::main] async fn main() {
/// let store = PluresDbAuditStore::open("/var/lib/pares-agens/audit").unwrap();
/// store.append(AuditEvent::new(EventKind::ModelCall, "agent-1", "gpt-4o", "tokens: 512", false)).await;
/// # }
/// ```
pub struct PluresDbAuditStore {
    store: CrdtStore,
}

impl PluresDbAuditStore {
    /// Open or create a durable PluresDB-backed audit store at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error string if the underlying [`SledStorage`] cannot be
    /// opened (e.g. permission denied, corrupted database).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let storage: Arc<dyn StorageEngine> =
            Arc::new(SledStorage::open(path).map_err(|e| format!("open failed: {e}"))?);
        let store = CrdtStore::default().with_persistence(storage);
        Ok(Self { store })
    }

    /// Create an ephemeral in-memory PluresDB audit store.
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
impl AuditStore for PluresDbAuditStore {
    async fn append(&self, event: AuditEvent) {
        let id = event.id.clone();
        match serde_json::to_value(&event) {
            Ok(data) => {
                self.store.put(&id, AUDIT_ACTOR, data);
            }
            Err(e) => tracing::error!("audit: failed to serialise event {id}: {e}"),
        }
    }

    async fn query(&self, query: &AuditQuery) -> Vec<AuditEvent> {
        self.store
            .list()
            .into_iter()
            .filter_map(|record| {
                serde_json::from_value::<AuditEvent>(record.data)
                    .map_err(|e| tracing::warn!("audit: deserialise failed: {e}"))
                    .ok()
            })
            .filter(|e| query.matches(e))
            .collect()
    }

    async fn all(&self) -> Vec<AuditEvent> {
        let mut events: Vec<AuditEvent> = self
            .store
            .list()
            .into_iter()
            .filter_map(|record| {
                serde_json::from_value::<AuditEvent>(record.data)
                    .map_err(|e| tracing::warn!("audit: deserialise failed: {e}"))
                    .ok()
            })
            .collect();
        // Sort by timestamp to give a consistent chronological order.
        events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        events
    }

    async fn len(&self) -> usize {
        self.store.list().len()
    }

    async fn purge_before(&self, cutoff_rfc3339: &str) {
        let to_delete: Vec<String> = self
            .store
            .list()
            .into_iter()
            .filter_map(|record| {
                serde_json::from_value::<AuditEvent>(record.data)
                    .ok()
                    .filter(|e| e.timestamp.as_str() < cutoff_rfc3339)
                    .map(|e| e.id)
            })
            .collect();
        for id in to_delete {
            if let Err(e) = self.store.delete(&id) {
                tracing::warn!(
                    "audit: failed to delete event {id} during purge (non-fatal, will retry on next purge): {e}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventKind;

    fn make_event(kind: EventKind, actor: &str) -> AuditEvent {
        AuditEvent::new(kind, actor, "dest", "summary", false)
    }

    #[tokio::test]
    async fn empty_store() {
        let store = InMemoryAuditStore::new();
        assert_eq!(store.len().await, 0);
        assert!(store.is_empty().await);
    }

    #[tokio::test]
    async fn append_increases_len() {
        let store = InMemoryAuditStore::new();
        store.append(make_event(EventKind::ModelCall, "a1")).await;
        store.append(make_event(EventKind::MemoryWrite, "a2")).await;
        assert_eq!(store.len().await, 2);
        assert!(!store.is_empty().await);
    }

    #[tokio::test]
    async fn all_returns_in_insertion_order() {
        let store = InMemoryAuditStore::new();
        store
            .append(make_event(EventKind::ModelCall, "first"))
            .await;
        store
            .append(make_event(EventKind::ToolExec, "second"))
            .await;
        let all = store.all().await;
        assert_eq!(all[0].actor, "first");
        assert_eq!(all[1].actor, "second");
    }

    #[tokio::test]
    async fn query_filters_by_kind() {
        let store = InMemoryAuditStore::new();
        store.append(make_event(EventKind::ModelCall, "a")).await;
        store.append(make_event(EventKind::MemoryWrite, "b")).await;
        store.append(make_event(EventKind::ModelCall, "c")).await;

        let q = AuditQuery::new().with_kind(EventKind::ModelCall);
        let results = store.query(&q).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.kind == EventKind::ModelCall));
    }

    #[tokio::test]
    async fn purge_before_removes_old_events() {
        let store = InMemoryAuditStore::new();
        // Append an event with a past timestamp by inserting directly.
        let mut old = make_event(EventKind::ToolExec, "old-actor");
        old.timestamp = "2020-01-01T00:00:00+00:00".to_string();
        let recent = make_event(EventKind::ModelCall, "new-actor");
        store.append(old).await;
        store.append(recent).await;

        store.purge_before("2023-01-01T00:00:00+00:00").await;
        assert_eq!(store.len().await, 1);
        assert_eq!(store.all().await[0].actor, "new-actor");
    }

    // ── PluresDbAuditStore ────────────────────────────────────────────────

    #[tokio::test]
    async fn pluresdb_empty_store() {
        let store = PluresDbAuditStore::in_memory();
        assert_eq!(store.len().await, 0);
        assert!(store.is_empty().await);
    }

    #[tokio::test]
    async fn pluresdb_append_increases_len() {
        let store = PluresDbAuditStore::in_memory();
        store.append(make_event(EventKind::ModelCall, "a1")).await;
        store.append(make_event(EventKind::MemoryWrite, "a2")).await;
        assert_eq!(store.len().await, 2);
        assert!(!store.is_empty().await);
    }

    #[tokio::test]
    async fn pluresdb_all_returns_chronological_order() {
        let store = PluresDbAuditStore::in_memory();
        let mut first = make_event(EventKind::ModelCall, "first");
        first.timestamp = "2024-01-01T00:00:00+00:00".to_string();
        let mut second = make_event(EventKind::ToolExec, "second");
        second.timestamp = "2024-06-01T00:00:00+00:00".to_string();
        store.append(first).await;
        store.append(second).await;
        let all = store.all().await;
        assert_eq!(all.len(), 2);
        assert!(all[0].timestamp <= all[1].timestamp);
    }

    #[tokio::test]
    async fn pluresdb_query_filters_by_kind() {
        let store = PluresDbAuditStore::in_memory();
        store.append(make_event(EventKind::ModelCall, "a")).await;
        store.append(make_event(EventKind::MemoryWrite, "b")).await;
        store.append(make_event(EventKind::ModelCall, "c")).await;

        let q = AuditQuery::new().with_kind(EventKind::ModelCall);
        let results = store.query(&q).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|e| e.kind == EventKind::ModelCall));
    }

    #[tokio::test]
    async fn pluresdb_query_filters_by_actor() {
        let store = PluresDbAuditStore::in_memory();
        store
            .append(make_event(EventKind::ToolExec, "agent-1"))
            .await;
        store
            .append(make_event(EventKind::ToolExec, "agent-2"))
            .await;

        let q = AuditQuery::new().with_actor("agent-1");
        let results = store.query(&q).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].actor, "agent-1");
    }

    #[tokio::test]
    async fn pluresdb_purge_before_removes_old_events() {
        let store = PluresDbAuditStore::in_memory();
        let mut old = make_event(EventKind::ToolExec, "old-actor");
        old.timestamp = "2020-01-01T00:00:00+00:00".to_string();
        let recent = make_event(EventKind::ModelCall, "new-actor");
        store.append(old).await;
        store.append(recent).await;

        store.purge_before("2023-01-01T00:00:00+00:00").await;
        assert_eq!(store.len().await, 1);
        let all = store.all().await;
        assert_eq!(all[0].actor, "new-actor");
    }

    #[tokio::test]
    async fn pluresdb_purge_before_keeps_all_when_all_recent() {
        let store = PluresDbAuditStore::in_memory();
        store.append(make_event(EventKind::ModelCall, "a")).await;
        store.append(make_event(EventKind::ToolExec, "b")).await;
        // Cutoff in the past — all events are recent relative to it.
        store.purge_before("2000-01-01T00:00:00+00:00").await;
        assert_eq!(store.len().await, 2);
    }

    #[tokio::test]
    async fn pluresdb_open_creates_persistent_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = PluresDbAuditStore::open(dir.path()).unwrap();
        store
            .append(make_event(EventKind::ModelCall, "persist"))
            .await;
        assert_eq!(store.len().await, 1);
    }

    #[tokio::test]
    async fn pluresdb_roundtrip_preserves_fields() {
        let store = PluresDbAuditStore::in_memory();
        let mut ev = AuditEvent::new(
            EventKind::ChannelSend,
            "agent-x",
            "telegram",
            "msg len: 10",
            true,
        );
        ev.timestamp = "2025-01-01T00:00:00+00:00".to_string();
        store.append(ev.clone()).await;

        let all = store.all().await;
        assert_eq!(all.len(), 1);
        let got = &all[0];
        assert_eq!(got.id, ev.id);
        assert_eq!(got.actor, ev.actor);
        assert_eq!(got.kind, ev.kind);
        assert_eq!(got.destination, ev.destination);
        assert_eq!(got.data_summary, ev.data_summary);
        assert!(got.pii_flag);
    }
}
