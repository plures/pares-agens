//! Per-device storage policies.
//!
//! A [`StoragePolicy`] describes *how* a device applies its capacity tier:
//! which memories are eligible for eviction, whether compression is enabled,
//! and whether compact embedding indexes are preferred.

use serde::{Deserialize, Serialize};

use crate::capacity::StorageTier;

// ── IndexKind ─────────────────────────────────────────────────────────────────

/// The embedding index variant this device maintains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexKind {
    /// Full-resolution floating-point vectors — highest accuracy.
    Full,
    /// Product-quantized or binary-quantized vectors — lower memory footprint.
    Quantized,
    /// No local index; all semantic searches are delegated to mesh peers.
    None,
}

// ── CompressionPolicy ─────────────────────────────────────────────────────────

/// When to compress stored memories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionPolicy {
    /// Never compress (hot-tier devices, instant access needed).
    Never,
    /// Compress memories in the warm tier only.
    WarmAndCold,
    /// Compress all memories.
    Always,
}

// ── StoragePolicy ─────────────────────────────────────────────────────────────

/// Full configuration for how a device stores and manages memories.
///
/// Build one directly or use [`StoragePolicy::for_tier`] to derive a sensible
/// default from a [`StorageTier`].
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::policy::{StoragePolicy, IndexKind, CompressionPolicy};
/// use pares_agens_dmem::capacity::StorageTier;
///
/// let policy = StoragePolicy::for_tier(&StorageTier::Warm);
/// assert_eq!(policy.index_kind, IndexKind::Quantized);
/// assert_eq!(policy.compression, CompressionPolicy::WarmAndCold);
/// assert!(!policy.store_full_embeddings);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoragePolicy {
    /// Which embedding index to maintain locally.
    pub index_kind: IndexKind,
    /// Compression strategy for stored memory bytes.
    pub compression: CompressionPolicy,
    /// Whether to persist the full embedding vector alongside the memory entry.
    ///
    /// On constrained devices, embeddings are kept on the high-capacity peer
    /// and only fetched on demand.
    pub store_full_embeddings: bool,
    /// Minimum relevance score in `[0, 1]` below which unpinned warm entries
    /// are eligible for early eviction (before the age window expires).
    pub early_eviction_score_threshold: f32,
    /// Whether this device participates as a fetch target for mesh peers.
    pub serve_remote_fetches: bool,
    /// Whether prefetch is enabled for this device.
    pub enable_prefetch: bool,
}

impl StoragePolicy {
    /// Derive a sensible default policy for the given [`StorageTier`].
    #[must_use]
    pub fn for_tier(tier: &StorageTier) -> Self {
        match tier {
            StorageTier::Hot => Self {
                index_kind: IndexKind::Quantized,
                compression: CompressionPolicy::Never,
                store_full_embeddings: false,
                early_eviction_score_threshold: 0.2,
                serve_remote_fetches: false,
                enable_prefetch: false,
            },
            StorageTier::Warm => Self {
                index_kind: IndexKind::Quantized,
                compression: CompressionPolicy::WarmAndCold,
                store_full_embeddings: false,
                early_eviction_score_threshold: 0.15,
                serve_remote_fetches: true,
                enable_prefetch: true,
            },
            StorageTier::Full | StorageTier::Custom { .. } => Self {
                index_kind: IndexKind::Full,
                compression: CompressionPolicy::WarmAndCold,
                store_full_embeddings: true,
                early_eviction_score_threshold: 0.05,
                serve_remote_fetches: true,
                enable_prefetch: true,
            },
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_for_hot_tier() {
        let p = StoragePolicy::for_tier(&StorageTier::Hot);
        assert_eq!(p.index_kind, IndexKind::Quantized);
        assert_eq!(p.compression, CompressionPolicy::Never);
        assert!(!p.store_full_embeddings);
        assert!(!p.serve_remote_fetches);
    }

    #[test]
    fn policy_for_warm_tier() {
        let p = StoragePolicy::for_tier(&StorageTier::Warm);
        assert_eq!(p.index_kind, IndexKind::Quantized);
        assert_eq!(p.compression, CompressionPolicy::WarmAndCold);
        assert!(p.serve_remote_fetches);
        assert!(p.enable_prefetch);
    }

    #[test]
    fn policy_for_full_tier() {
        let p = StoragePolicy::for_tier(&StorageTier::Full);
        assert_eq!(p.index_kind, IndexKind::Full);
        assert!(p.store_full_embeddings);
        assert!(p.serve_remote_fetches);
    }

    #[test]
    fn policy_for_custom_tier() {
        let p = StoragePolicy::for_tier(&StorageTier::Custom { max_age_days: 30 });
        assert_eq!(p.index_kind, IndexKind::Full);
        assert!(p.serve_remote_fetches);
    }
}
