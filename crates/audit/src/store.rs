//! Append-only audit store trait and in-memory implementation.
//!
//! [`AuditStore`] is the backing-store abstraction for the audit log.  The
//! design deliberately mirrors the existing `MemoryStore` pattern so that
//! adopters can swap in a PluresDB-backed implementation without changing call
//! sites.
//!
//! [`InMemoryAuditStore`] is the default implementation — it keeps all events
//! in a `RwLock<Vec<AuditEvent>>` and is suitable for tests and single-process
//! deployments where persistence is handled by the caller.

use std::sync::Arc;

use async_trait::async_trait;
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
}
