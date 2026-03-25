//! # pares-agens-gpu
//!
//! GPU inference pool — run multiple BitNet models simultaneously on a single
//! consumer GPU.  BitNet's 1.58-bit weights keep each 8B model at ~2 GB, so a
//! 20 GB GPU can hold 4–5 concurrent models with room for KV caches.
//!
//! | Type | Description |
//! |------|-------------|
//! | [`GpuConfig`] | `[inference.gpu]` TOML section: `vram_budget_mb`, `max_models`, `eviction_policy`. |
//! | [`ModelBackend`] | Async trait implemented by every model loaded onto the GPU. |
//! | [`SimulatedModelBackend`] | No-GPU stub for tests and CI. |
//! | [`GpuModelPool`] | Manages loaded models, tracks VRAM budget, enforces LRU eviction. |
//! | [`KvCacheManager`] | Per-request KV cache allocation/free from a shared VRAM pool. |
//! | [`CapacityPlanner`] | Given a VRAM budget and a list of model specs, recommends an optimal mix. |
//! | [`FallbackRunner`] | Cascade: GPU → CPU → cloud API. |
//! | [`LruEviction`] | LRU eviction bookkeeping. |
//! | [`GpuError`] | Unified error type. |
//!
//! # Feature flags
//!
//! | Feature | Description |
//! |---------|-------------|
//! | `cuda`  | Enable real CUDA W1.58A8 kernel integration (requires GPU + CUDA toolkit). |
//!
//! Without the `cuda` feature every pool operation is handled by
//! [`SimulatedModelBackend`], keeping CI fast without a physical GPU.
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_gpu::{
//!     GpuConfig, GpuModelPool, SimulatedModelBackend, EvictionPolicy,
//! };
//! use std::sync::Arc;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), pares_agens_gpu::GpuError> {
//! let config = GpuConfig {
//!     vram_budget_mb: 20_000,
//!     max_models: 5,
//!     eviction_policy: EvictionPolicy::Lru,
//!     kv_cache_mb: 4_096,
//! };
//!
//! let mut pool = GpuModelPool::new(config);
//!
//! let model: Arc<dyn pares_agens_gpu::ModelBackend> = Arc::new(
//!     SimulatedModelBackend::new("code-8b", 2_000),
//! );
//! pool.load(Arc::clone(&model))?;
//!
//! let output = pool.infer("code-8b", "fn hello()", Default::default()).await?;
//! println!("{output}");
//! # Ok(())
//! # }
//! ```

pub mod backend;
pub mod config;
pub mod eviction;
pub mod fallback;
pub mod kv_cache;
pub mod planner;
pub mod pool;

pub use backend::{ModelBackend, SimulatedModelBackend};
pub use config::{EvictionPolicy, GpuConfig};
pub use error::GpuError;
pub use eviction::LruEviction;
pub use fallback::{FallbackRunner, FallbackTier};
pub use kv_cache::KvCacheManager;
pub use planner::{CapacityPlanner, ModelSpec, PlannedAllocation};
pub use pool::{GpuModelPool, InferenceParams};

mod error {
    use thiserror::Error;

    /// All errors that can surface from the GPU inference pool.
    #[derive(Debug, Error)]
    pub enum GpuError {
        /// Not enough VRAM to load the requested model.
        #[error("insufficient VRAM: need {needed_mb} MB but only {available_mb} MB available")]
        InsufficientVram { needed_mb: u64, available_mb: u64 },

        /// The pool already holds the maximum number of models.
        #[error("model pool is full: max_models={max_models}")]
        PoolFull { max_models: usize },

        /// A model with the given ID is not currently loaded.
        #[error("model `{model_id}` is not loaded in the GPU pool")]
        ModelNotLoaded { model_id: String },

        /// A model with the same ID is already loaded.
        #[error("model `{model_id}` is already loaded")]
        AlreadyLoaded { model_id: String },

        /// The KV cache has no space left for this request.
        #[error("KV cache exhausted: need {needed_mb} MB but only {available_mb} MB available")]
        KvCacheExhausted { needed_mb: u64, available_mb: u64 },

        /// Inference failed on the GPU backend.
        #[error("GPU inference failed for model `{model_id}`: {reason}")]
        InferenceFailed { model_id: String, reason: String },

        /// All fallback tiers (GPU, CPU, cloud) failed.
        #[error("all fallback tiers failed: {reason}")]
        AllFallbacksFailed { reason: String },

        /// The `cuda` Cargo feature is not enabled.
        #[error("CUDA is unavailable: recompile with the `cuda` feature enabled")]
        CudaUnavailable,
    }
}
