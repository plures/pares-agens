//! Conflict resolution — CRDT-merge strategy for incoming sync payloads.
//!
//! When two devices make concurrent changes to the same entry, the sync
//! engine calls the appropriate [`MergeStrategy`] to produce a single
//! authoritative value.  The default strategy for PluresDB data is
//! [`CrdtMerge`], which uses a last-write-wins approach based on the
//! `updated_at` timestamp embedded in the payload.

use serde_json::Value;

use crate::SyncError;

// ── MergeStrategy ─────────────────────────────────────────────────────────────

/// Trait for conflict resolution strategies.
///
/// Implementors receive the `local` and `remote` JSON payloads and must
/// return a single merged value.  Returning an `Err` signals that conflict
/// resolution failed for this entry; the error is propagated to the caller
/// of [`SyncEngine::apply_remote_change`](crate::engine::SyncEngine), which
/// may choose to skip the entry and emit a warning.
pub trait MergeStrategy: Send + Sync {
    /// Merge `local` and `remote` payloads into a single authoritative value.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::ConflictResolution`] when merging is not possible.
    fn merge(&self, local: &Value, remote: &Value) -> Result<Value, SyncError>;

    /// Human-readable name of this strategy (used in logs and telemetry).
    fn name(&self) -> &str;
}

// ── CrdtMerge ─────────────────────────────────────────────────────────────────

/// Last-write-wins CRDT merge strategy using the `updated_at` timestamp.
///
/// This is the default strategy for PluresDB-backed entries.  Both payloads
/// must contain an `"updated_at"` field with an ISO 8601 timestamp string.
/// The payload with the more recent timestamp wins.
///
/// When timestamps are equal or missing, the `remote` value is preferred
/// (safe default that avoids data loss on the local device).
#[derive(Debug, Default)]
pub struct CrdtMerge;

impl MergeStrategy for CrdtMerge {
    fn name(&self) -> &str {
        "crdt-last-write-wins"
    }

    fn merge(&self, local: &Value, remote: &Value) -> Result<Value, SyncError> {
        let local_ts = timestamp_str(local);
        let remote_ts = timestamp_str(remote);
        match (local_ts, remote_ts) {
            (Some(l), Some(r)) => {
                // Lexicographic comparison works for ISO 8601 with equal precision.
                if l > r {
                    Ok(local.clone())
                } else {
                    Ok(remote.clone())
                }
            }
            // If only remote has a timestamp, prefer remote.
            (None, Some(_)) => Ok(remote.clone()),
            // If only local has a timestamp, prefer local.
            (Some(_), None) => Ok(local.clone()),
            // No timestamps — prefer remote (safe default).
            (None, None) => Ok(remote.clone()),
        }
    }
}

fn timestamp_str(v: &Value) -> Option<&str> {
    v.get("updated_at").and_then(|t| t.as_str())
}

// ── LastWriteWins ─────────────────────────────────────────────────────────────

/// Always-remote strategy: the incoming remote payload unconditionally wins.
///
/// Useful for configuration topics where the most recently pushed config
/// should always propagate to all devices.
#[derive(Debug, Default)]
pub struct LastWriteWins;

impl MergeStrategy for LastWriteWins {
    fn name(&self) -> &str {
        "last-write-wins-remote"
    }

    fn merge(&self, _local: &Value, remote: &Value) -> Result<Value, SyncError> {
        Ok(remote.clone())
    }
}

// ── ConflictResolution ────────────────────────────────────────────────────────

/// Selects and applies the appropriate [`MergeStrategy`] for a given payload.
///
/// The engine uses [`ConflictResolution::resolve`] when it receives an
/// incoming change event that conflicts with a locally-held value.
pub struct ConflictResolution {
    strategy: Box<dyn MergeStrategy>,
}

impl ConflictResolution {
    /// Create a `ConflictResolution` instance backed by the given strategy.
    pub fn new(strategy: impl MergeStrategy + 'static) -> Self {
        Self {
            strategy: Box::new(strategy),
        }
    }

    /// Create a `ConflictResolution` instance using the default [`CrdtMerge`]
    /// strategy.
    #[must_use]
    pub fn default_crdt() -> Self {
        Self::new(CrdtMerge)
    }

    /// Apply the configured strategy to `local` and `remote` payloads.
    ///
    /// # Errors
    ///
    /// Propagates errors from the underlying [`MergeStrategy`].
    pub fn resolve(&self, local: &Value, remote: &Value) -> Result<Value, SyncError> {
        self.strategy.merge(local, remote)
    }

    /// Return the name of the active merge strategy.
    #[must_use]
    pub fn strategy_name(&self) -> &str {
        self.strategy.name()
    }
}

impl std::fmt::Debug for ConflictResolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConflictResolution")
            .field("strategy", &self.strategy.name())
            .finish()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn crdt_merge_picks_remote_when_newer() {
        let strategy = CrdtMerge;
        let local = json!({"updated_at": "2024-01-01T10:00:00Z", "val": "old"});
        let remote = json!({"updated_at": "2024-01-01T11:00:00Z", "val": "new"});
        let result = strategy.merge(&local, &remote).unwrap();
        assert_eq!(result["val"], "new");
    }

    #[test]
    fn crdt_merge_picks_local_when_newer() {
        let strategy = CrdtMerge;
        let local = json!({"updated_at": "2024-01-02T10:00:00Z", "val": "local-wins"});
        let remote = json!({"updated_at": "2024-01-01T10:00:00Z", "val": "remote-loses"});
        let result = strategy.merge(&local, &remote).unwrap();
        assert_eq!(result["val"], "local-wins");
    }

    #[test]
    fn crdt_merge_prefers_remote_on_equal_timestamps() {
        let strategy = CrdtMerge;
        let ts = "2024-06-01T00:00:00Z";
        let local = json!({"updated_at": ts, "val": "local"});
        let remote = json!({"updated_at": ts, "val": "remote"});
        let result = strategy.merge(&local, &remote).unwrap();
        assert_eq!(result["val"], "remote");
    }

    #[test]
    fn crdt_merge_falls_back_to_remote_when_no_timestamps() {
        let strategy = CrdtMerge;
        let local = json!({"val": "local"});
        let remote = json!({"val": "remote"});
        let result = strategy.merge(&local, &remote).unwrap();
        assert_eq!(result["val"], "remote");
    }

    #[test]
    fn last_write_wins_always_picks_remote() {
        let strategy = LastWriteWins;
        let local = json!({"updated_at": "2099-01-01T00:00:00Z", "val": "local"});
        let remote = json!({"val": "remote"});
        let result = strategy.merge(&local, &remote).unwrap();
        assert_eq!(result["val"], "remote");
    }

    #[test]
    fn conflict_resolution_default_crdt_strategy_name() {
        let cr = ConflictResolution::default_crdt();
        assert_eq!(cr.strategy_name(), "crdt-last-write-wins");
    }

    #[test]
    fn conflict_resolution_resolve_delegates_to_strategy() {
        let cr = ConflictResolution::default_crdt();
        let local = json!({"updated_at": "2024-01-01T00:00:00Z"});
        let remote = json!({"updated_at": "2024-01-02T00:00:00Z"});
        let result = cr.resolve(&local, &remote).unwrap();
        assert_eq!(result["updated_at"], "2024-01-02T00:00:00Z");
    }

    #[test]
    fn conflict_resolution_debug_includes_strategy_name() {
        let cr = ConflictResolution::default_crdt();
        let debug = format!("{cr:?}");
        assert!(debug.contains("crdt-last-write-wins"));
    }
}
