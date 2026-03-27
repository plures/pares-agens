//! Audit trail — every deletion and restore is logged here.
//!
//! The [`AuditLog`] acts as the "praxis ledger" referenced in the issue.
//! Every destructive (and restorative) action performed by
//! [`super::engine::ForgettingEngine`] appends an immutable [`AuditEntry`].
//!
//! The log is intentionally append-only: entries are never removed.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::memory::entry::MemoryCategory;

// ---------------------------------------------------------------------------
// AuditAction
// ---------------------------------------------------------------------------

/// The kind of action that was taken on a memory entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// Entry was soft-deleted (recoverable within the recovery window).
    SoftDeleted,
    /// Entry was permanently purged from the store.
    HardPurged,
    /// Soft-deleted entry was restored.
    Restored,
    /// Entry was virtually "lost" during a simulation drill (non-destructive).
    SimulatedLoss,
    /// A scheduled purge pass ran (no-op entry when 0 entries were affected).
    ScheduledPurgeRan,
}

impl AuditAction {
    /// Human-readable label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SoftDeleted => "soft-deleted",
            Self::HardPurged => "hard-purged",
            Self::Restored => "restored",
            Self::SimulatedLoss => "simulated-loss",
            Self::ScheduledPurgeRan => "scheduled-purge-ran",
        }
    }
}

// ---------------------------------------------------------------------------
// AuditEntry
// ---------------------------------------------------------------------------

/// A single immutable record in the forgetting audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Unique identifier for this log entry.
    pub id: String,
    /// ID of the memory entry this action was taken on.
    ///
    /// Empty string for aggregate actions such as [`AuditAction::ScheduledPurgeRan`].
    pub memory_id: String,
    /// Category of the affected memory entry.
    ///
    /// `None` for system-level aggregate events (e.g. `ScheduledPurgeRan`)
    /// that are not associated with a single memory entry.
    pub category: Option<MemoryCategory>,
    /// The action that was taken.
    pub action: AuditAction,
    /// Human-readable reason supplied by the policy engine or operator.
    pub reason: String,
    /// RFC 3339 timestamp of when the action was recorded.
    pub timestamp: String,
    /// `true` when the action was part of a simulation drill and did **not**
    /// modify real data.
    pub is_simulation: bool,
}

impl AuditEntry {
    /// Create a new per-entry audit record stamped with the current UTC time.
    pub fn new(
        memory_id: impl Into<String>,
        category: MemoryCategory,
        action: AuditAction,
        reason: impl Into<String>,
        is_simulation: bool,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            memory_id: memory_id.into(),
            category: Some(category),
            action,
            reason: reason.into(),
            timestamp: Utc::now().to_rfc3339(),
            is_simulation,
        }
    }

    /// Create a system-level aggregate audit entry (no category, no memory ID).
    pub fn aggregate(action: AuditAction, reason: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            memory_id: String::new(),
            category: None,
            action,
            reason: reason.into(),
            timestamp: Utc::now().to_rfc3339(),
            is_simulation: false,
        }
    }
}

// ---------------------------------------------------------------------------
// AuditLog
// ---------------------------------------------------------------------------

/// Thread-safe, append-only audit log for forgetting operations.
///
/// Backed by a `RwLock<Vec<AuditEntry>>` so multiple readers can inspect the
/// log concurrently while a single writer appends.
///
/// # Praxis ledger integration
///
/// This struct is intentionally self-contained.  Persisting to the praxis
/// ledger is done by calling [`AuditLog::entries`] and pushing the results
/// into a `PraxisStore` (see `crates/praxis`).
#[derive(Default)]
pub struct AuditLog {
    entries: RwLock<Vec<AuditEntry>>,
}

impl AuditLog {
    /// Create an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one entry to the log.
    pub async fn append(&self, entry: AuditEntry) {
        self.entries.write().await.push(entry);
    }

    /// Return a snapshot of all entries in insertion order.
    pub async fn entries(&self) -> Vec<AuditEntry> {
        self.entries.read().await.clone()
    }

    /// Return entries filtered by `action`.
    pub async fn entries_by_action(&self, action: AuditAction) -> Vec<AuditEntry> {
        self.entries
            .read()
            .await
            .iter()
            .filter(|e| e.action == action)
            .cloned()
            .collect()
    }

    /// Number of entries currently in the log.
    pub async fn len(&self) -> usize {
        self.entries.read().await.len()
    }

    /// `true` when the log is empty.
    pub async fn is_empty(&self) -> bool {
        self.entries.read().await.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn conv_entry(memory_id: &str, action: AuditAction) -> AuditEntry {
        AuditEntry::new(
            memory_id,
            MemoryCategory::Conversation,
            action,
            "test",
            false,
        )
    }

    #[tokio::test]
    async fn empty_log_has_zero_entries() {
        let log = AuditLog::new();
        assert_eq!(log.len().await, 0);
        assert!(log.is_empty().await);
    }

    #[tokio::test]
    async fn append_increases_len() {
        let log = AuditLog::new();
        log.append(conv_entry("m1", AuditAction::SoftDeleted)).await;
        log.append(conv_entry("m2", AuditAction::HardPurged)).await;
        assert_eq!(log.len().await, 2);
    }

    #[tokio::test]
    async fn filter_by_action() {
        let log = AuditLog::new();
        log.append(conv_entry("m1", AuditAction::SoftDeleted)).await;
        log.append(conv_entry("m2", AuditAction::HardPurged)).await;
        log.append(conv_entry("m3", AuditAction::SoftDeleted)).await;

        let purged = log.entries_by_action(AuditAction::HardPurged).await;
        assert_eq!(purged.len(), 1);
        assert_eq!(purged[0].memory_id, "m2");
    }

    #[tokio::test]
    async fn simulation_flag_is_preserved() {
        let log = AuditLog::new();
        let e = AuditEntry::new(
            "sim-1",
            MemoryCategory::CodePattern,
            AuditAction::SimulatedLoss,
            "drill",
            true,
        );
        log.append(e).await;
        let entries = log.entries().await;
        assert!(entries[0].is_simulation);
    }

    #[test]
    fn audit_action_labels_are_correct() {
        assert_eq!(AuditAction::SoftDeleted.as_str(), "soft-deleted");
        assert_eq!(AuditAction::HardPurged.as_str(), "hard-purged");
        assert_eq!(AuditAction::Restored.as_str(), "restored");
        assert_eq!(AuditAction::SimulatedLoss.as_str(), "simulated-loss");
        assert_eq!(
            AuditAction::ScheduledPurgeRan.as_str(),
            "scheduled-purge-ran"
        );
    }

    #[test]
    fn aggregate_entry_has_none_category() {
        let e = AuditEntry::aggregate(
            AuditAction::ScheduledPurgeRan,
            "soft_deleted=3, hard_purged=1",
        );
        assert!(e.category.is_none());
        assert!(e.memory_id.is_empty());
        assert!(!e.is_simulation);
        assert_eq!(e.action, AuditAction::ScheduledPurgeRan);
    }

    #[test]
    fn per_entry_audit_has_some_category() {
        let e = AuditEntry::new(
            "mem-1",
            MemoryCategory::Conversation,
            AuditAction::SoftDeleted,
            "expired",
            false,
        );
        assert_eq!(e.category, Some(MemoryCategory::Conversation));
        assert_eq!(e.memory_id, "mem-1");
    }
}
