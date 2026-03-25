//! GPU pool configuration (`[inference.gpu]`).

use serde::{Deserialize, Serialize};

/// Policy controlling which model is evicted when VRAM pressure arises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvictionPolicy {
    /// Evict the model that was used least recently.
    #[default]
    Lru,
}

/// Configuration for the GPU inference pool.
///
/// Maps to the `[inference.gpu]` section in the application config file.
///
/// # TOML example
///
/// ```toml
/// [inference.gpu]
/// vram_budget_mb = 20000
/// max_models = 5
/// eviction_policy = "lru"
/// kv_cache_mb = 4096
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuConfig {
    /// Total VRAM available for the model pool (weights + activations), in MiB.
    ///
    /// Defaults to 20 000 MiB (≈20 GB) — a common consumer GPU size.
    #[serde(default = "default_vram_budget_mb")]
    pub vram_budget_mb: u64,

    /// Maximum number of models that may be simultaneously resident in the pool.
    ///
    /// The pool evicts the LRU model when this limit is reached.
    #[serde(default = "default_max_models")]
    pub max_models: usize,

    /// Eviction policy applied when VRAM or `max_models` limits are hit.
    #[serde(default)]
    pub eviction_policy: EvictionPolicy,

    /// VRAM reserved for the shared KV cache, in MiB.
    ///
    /// This budget is subtracted from `vram_budget_mb` before model weights
    /// are loaded, so the pool always has headroom for running requests.
    ///
    /// Defaults to 4 096 MiB (4 GiB).
    #[serde(default = "default_kv_cache_mb")]
    pub kv_cache_mb: u64,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            vram_budget_mb: default_vram_budget_mb(),
            max_models: default_max_models(),
            eviction_policy: EvictionPolicy::Lru,
            kv_cache_mb: default_kv_cache_mb(),
        }
    }
}

fn default_vram_budget_mb() -> u64 {
    20_000
}

fn default_max_models() -> usize {
    5
}

fn default_kv_cache_mb() -> u64 {
    4_096
}
