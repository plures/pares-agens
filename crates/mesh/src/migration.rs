//! Expert migration: move or copy a model from one device to another on demand.
//!
//! Actual byte transfer is delegated to the [`MigrationTransport`] trait,
//! keeping this module transport-agnostic (Hyperswarm, loopback for tests,
//! etc.).

use crate::MeshError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── MigrationRequest ──────────────────────────────────────────────────────────

/// Describes a request to move (or copy) an expert between devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRequest {
    /// The expert to migrate.
    pub expert_id: String,
    /// Source device identifier (where the expert currently lives).
    pub source_device_id: String,
    /// Destination device identifier (where the expert should be placed).
    pub target_device_id: String,
    /// When `true`, retain the expert on the source device after migration
    /// (copy semantics).  When `false`, unload it from the source (move
    /// semantics).
    pub keep_source: bool,
}

// ── MigrationResult ───────────────────────────────────────────────────────────

/// Outcome of a completed expert migration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationResult {
    /// The migrated expert.
    pub expert_id: String,
    /// Source device.
    pub from_device: String,
    /// Destination device.
    pub to_device: String,
    /// Whether the original was retained on the source device.
    pub source_retained: bool,
    /// Total bytes transferred (may be 0 for in-memory moves).
    pub bytes_transferred: u64,
}

// ── MigrationTransport ────────────────────────────────────────────────────────

/// Transport abstraction for moving expert model data between devices.
///
/// Implementations handle the actual serialisation and transmission (e.g. via
/// Hyperswarm data channels) while this module owns the request/result
/// contract.
#[async_trait]
pub trait MigrationTransport: Send + Sync {
    /// Transfer the expert model data and notify both devices.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError`] on transport failure.
    async fn migrate(&self, req: &MigrationRequest) -> Result<MigrationResult, MeshError>;
}

// ── NoopMigrationTransport ────────────────────────────────────────────────────

/// No-op transport that acknowledges migration requests without transferring
/// bytes.  Useful for unit testing.
pub struct NoopMigrationTransport;

#[async_trait]
impl MigrationTransport for NoopMigrationTransport {
    async fn migrate(&self, req: &MigrationRequest) -> Result<MigrationResult, MeshError> {
        Ok(MigrationResult {
            expert_id: req.expert_id.clone(),
            from_device: req.source_device_id.clone(),
            to_device: req.target_device_id.clone(),
            source_retained: req.keep_source,
            bytes_transferred: 0,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn req(keep: bool) -> MigrationRequest {
        MigrationRequest {
            expert_id: "code-30b".into(),
            source_device_id: "desktop".into(),
            target_device_id: "laptop".into(),
            keep_source: keep,
        }
    }

    #[tokio::test]
    async fn noop_transport_returns_correct_result_move_semantics() {
        let transport = NoopMigrationTransport;
        let result = transport.migrate(&req(false)).await.unwrap();
        assert_eq!(result.expert_id, "code-30b");
        assert_eq!(result.from_device, "desktop");
        assert_eq!(result.to_device, "laptop");
        assert!(!result.source_retained);
        assert_eq!(result.bytes_transferred, 0);
    }

    #[tokio::test]
    async fn noop_transport_returns_correct_result_copy_semantics() {
        let transport = NoopMigrationTransport;
        let result = transport.migrate(&req(true)).await.unwrap();
        assert!(result.source_retained);
    }

    #[test]
    fn migration_request_roundtrips_serde() {
        let r = req(true);
        let json = serde_json::to_string(&r).unwrap();
        let back: MigrationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expert_id, r.expert_id);
        assert!(back.keep_source);
    }

    #[test]
    fn migration_result_roundtrips_serde() {
        let result = MigrationResult {
            expert_id: "code-30b".into(),
            from_device: "desktop".into(),
            to_device: "laptop".into(),
            source_retained: false,
            bytes_transferred: 1_234_567,
        };
        let json = serde_json::to_string(&result).unwrap();
        let back: MigrationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bytes_transferred, 1_234_567);
    }
}
