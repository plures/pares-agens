//! `pares-agens-gpu` — GPU inference pool for multi-model BitNet on a single consumer GPU.
//!
//! BitNet's 1.58-bit weight format makes it practical to hold several models in VRAM
//! simultaneously (an 8 B model occupies only ~2 GB).  This crate provides:
//!
//! | Component | Description |
//! |-----------|-------------|
//! | [`GpuModelPool`](pool::GpuModelPool) | Loads/evicts models, tracks VRAM budget, dispatches concurrent inference |
//! | [`KvCacheManager`](kv_cache::KvCacheManager) | Per-request KV-cache allocations from a shared VRAM pool |
//! | [`CapacityPlanner`](planner::CapacityPlanner) | Recommends an optimal model mix for a given VRAM budget |
//! | [`FallbackRunner`](fallback::FallbackRunner) | GPU full → CPU runner → cloud API cascade |
//! | [`EvictionPolicy`](eviction::EvictionPolicy) | Pluggable eviction strategy (LRU built-in) |
//!
//! # Architecture
//!
//! ```text
//! Request
//!   │
//!   ▼
//! GpuModelPool ──── VRAM budget tracker
//!   │        └───── LRU eviction (EvictionPolicy)
//!   │
//!   ├── per-model CUDA stream (tokio task)
//!   │        └── KvCacheManager  (shared KV pool)
//!   │
//!   └── FallbackRunner (GPU full → CPU → cloud)
//! ```
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_gpu::{
//!     GpuConfig, EvictionPolicyKind,
//!     pool::GpuModelPool,
//!     planner::CapacityPlanner,
//! };
//!
//! // Ask the planner which models fit in 20 GB.
//! let plan = CapacityPlanner::new(20_480).recommend_mix(&[
//!     ("code-8b".into(), 8_000),
//!     ("chat-8b".into(), 8_000),
//!     ("fast-2b".into(), 2_000),
//! ]);
//! println!("{plan:?}");
//!
//! // Build a pool and load a model.
//! let config = GpuConfig {
//!     vram_budget_mb: 20_480,
//!     max_models: 5,
//!     eviction_policy: EvictionPolicyKind::Lru,
//!     kv_cache_budget_mb: 5_120,
//! };
//! let pool = GpuModelPool::new(config);
//! ```

pub mod eviction;
pub mod fallback;
pub mod kv_cache;
pub mod planner;
pub mod pool;

pub use eviction::EvictionPolicyKind;
pub use fallback::{FallbackRunner, FallbackTarget};
pub use kv_cache::KvCacheManager;
pub use planner::CapacityPlanner;
pub use pool::GpuModelPool;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors produced by the GPU inference pool.
#[derive(Debug, Error)]
pub enum GpuError {
    /// The requested model is not currently loaded in the pool.
    #[error("model not loaded: {0}")]
    ModelNotLoaded(String),

    /// The VRAM budget would be exceeded by the requested allocation.
    #[error("VRAM budget exceeded: requested {requested_mb} MB, available {available_mb} MB")]
    VramBudgetExceeded {
        /// MB requested.
        requested_mb: u64,
        /// MB currently available.
        available_mb: u64,
    },

    /// The KV-cache pool is full; all allocations are in use.
    #[error("KV-cache pool exhausted: requested {requested_mb} MB, available {available_mb} MB")]
    KvCacheExhausted {
        /// MB requested.
        requested_mb: u64,
        /// MB currently available.
        available_mb: u64,
    },

    /// The pool has reached its maximum concurrent-model limit.
    #[error("model pool is full (max_models = {0})")]
    PoolFull(usize),

    /// All fallback targets have been exhausted without a successful response.
    #[error("all fallback targets exhausted for model {0}")]
    FallbackExhausted(String),

    /// An inference run was cancelled or timed out.
    #[error("inference cancelled or timed out for model {0}")]
    InferenceCancelled(String),
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Top-level GPU inference pool configuration (`[inference.gpu]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuConfig {
    /// Total VRAM budget in megabytes available for model weights.
    ///
    /// Defaults to `20480` (20 GB).
    #[serde(default = "default_vram_budget_mb")]
    pub vram_budget_mb: u64,

    /// Maximum number of models to hold loaded simultaneously.
    ///
    /// Defaults to `5`.
    #[serde(default = "default_max_models")]
    pub max_models: usize,

    /// Eviction policy applied when the pool is full and a new model must be
    /// loaded.
    ///
    /// Defaults to [`EvictionPolicyKind::Lru`].
    #[serde(default)]
    pub eviction_policy: EvictionPolicyKind,

    /// VRAM budget reserved for KV caches across all active requests.
    ///
    /// Defaults to `5120` (5 GB).
    #[serde(default = "default_kv_cache_budget_mb")]
    pub kv_cache_budget_mb: u64,
}

fn default_vram_budget_mb() -> u64 {
    20_480
}

fn default_max_models() -> usize {
    5
}

fn default_kv_cache_budget_mb() -> u64 {
    5_120
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            vram_budget_mb: default_vram_budget_mb(),
            max_models: default_max_models(),
            eviction_policy: EvictionPolicyKind::Lru,
            kv_cache_budget_mb: default_kv_cache_budget_mb(),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = GpuConfig::default();
        assert_eq!(cfg.vram_budget_mb, 20_480);
        assert_eq!(cfg.max_models, 5);
        assert_eq!(cfg.kv_cache_budget_mb, 5_120);
        assert!(matches!(cfg.eviction_policy, EvictionPolicyKind::Lru));
    }

    #[test]
    fn gpu_config_roundtrips_json() {
        let cfg = GpuConfig {
            vram_budget_mb: 8_192,
            max_models: 3,
            eviction_policy: EvictionPolicyKind::Lru,
            kv_cache_budget_mb: 2_048,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let roundtrip: GpuConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.vram_budget_mb, 8_192);
        assert_eq!(roundtrip.max_models, 3);
        assert_eq!(roundtrip.kv_cache_budget_mb, 2_048);
    }

    #[test]
    fn vram_budget_exceeded_error_message() {
        let err = GpuError::VramBudgetExceeded {
            requested_mb: 4_096,
            available_mb: 1_024,
        };
        let msg = err.to_string();
        assert!(msg.contains("4096"));
        assert!(msg.contains("1024"));
    }
}
