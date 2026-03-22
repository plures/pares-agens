//! GPU model pool — loads, caches, and evicts BitNet models on a single GPU.
//!
//! [`GpuModelPool`] is the central component of the GPU inference pipeline.
//! It:
//!
//! * Tracks which models are currently loaded and their VRAM footprints.
//! * Enforces the VRAM budget and `max_models` cap.
//! * Evicts models according to the configured [`EvictionPolicy`] when the
//!   pool is full.
//! * Dispatches inference via per-model slots (modelling separate CUDA
//!   streams as `tokio` tasks in this implementation).
//! * Exposes VRAM accounting helpers for observability.
//!
//! # Backend abstraction
//!
//! Real CUDA kernel calls are abstracted behind the [`ModelBackend`] trait so
//! that test code (and CPU-only builds) can supply a [`SimulatedModelBackend`]
//! without linking against any GPU libraries.
//!
//! # Example
//!
//! ```no_run
//! use pares_agens_gpu::{GpuConfig, pool::{GpuModelPool, SimulatedModelBackend}};
//!
//! # async fn example() {
//! let config = GpuConfig::default();
//! let pool = GpuModelPool::new_with_backend(config, SimulatedModelBackend);
//!
//! pool.load_model("chat-8b", 2_048).await.unwrap();
//!
//! let output = pool.infer("chat-8b", "Hello!").await.unwrap();
//! assert!(output.contains("chat-8b"));
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::{
    eviction::{EvictionPolicy, EvictionPolicyKind, LruEviction},
    GpuConfig, GpuError,
};

// ── ModelBackend trait ────────────────────────────────────────────────────────

/// Abstraction over the hardware backend used to load and run models.
///
/// On real GPU hardware this would call into the CUDA W1.58A8 kernel.
/// In tests, use [`SimulatedModelBackend`].
#[async_trait::async_trait]
pub trait ModelBackend: Send + Sync {
    /// Load the model with the given identifier into VRAM.
    ///
    /// `weight_mb` is the VRAM footprint reported by the caller.
    async fn load(&self, model_id: &str, weight_mb: u64) -> Result<(), GpuError>;

    /// Unload the model from VRAM.
    async fn unload(&self, model_id: &str) -> Result<(), GpuError>;

    /// Run a forward pass and return generated text.
    async fn infer(&self, model_id: &str, prompt: &str) -> Result<String, GpuError>;
}

// ── SimulatedModelBackend ─────────────────────────────────────────────────────

/// No-op backend for tests and CPU-only builds.
///
/// `load`/`unload` are instant; `infer` echoes the model name and prompt length.
pub struct SimulatedModelBackend;

#[async_trait::async_trait]
impl ModelBackend for SimulatedModelBackend {
    async fn load(&self, model_id: &str, weight_mb: u64) -> Result<(), GpuError> {
        debug!(model_id, weight_mb, "SimulatedModelBackend: load");
        Ok(())
    }

    async fn unload(&self, model_id: &str) -> Result<(), GpuError> {
        debug!(model_id, "SimulatedModelBackend: unload");
        Ok(())
    }

    async fn infer(&self, model_id: &str, prompt: &str) -> Result<String, GpuError> {
        debug!(model_id, "SimulatedModelBackend: infer");
        Ok(format!(
            "[simulated:{model_id}] {} tokens",
            prompt.split_whitespace().count()
        ))
    }
}

// ── ModelSlot (internal) ──────────────────────────────────────────────────────

/// Metadata for a model currently loaded in the pool.
#[derive(Debug, Clone)]
pub struct ModelSlot {
    /// Model identifier.
    pub model_id: String,
    /// VRAM consumed by this model's weights (MB).
    pub weight_mb: u64,
}

// ── PoolState (internal) ──────────────────────────────────────────────────────

struct PoolState {
    slots: HashMap<String, ModelSlot>,
    used_weight_mb: u64,
    eviction: Box<dyn EvictionPolicy>,
}

impl PoolState {
    fn new(policy: EvictionPolicyKind) -> Self {
        let eviction: Box<dyn EvictionPolicy> = match policy {
            EvictionPolicyKind::Lru => Box::new(LruEviction::default()),
        };
        Self {
            slots: HashMap::new(),
            used_weight_mb: 0,
            eviction,
        }
    }

    fn available_mb(&self, budget: u64) -> u64 {
        budget.saturating_sub(self.used_weight_mb)
    }
}

// ── GpuModelPool ─────────────────────────────────────────────────────────────

/// VRAM-aware pool that holds multiple BitNet models loaded on a single GPU.
pub struct GpuModelPool<B: ModelBackend = SimulatedModelBackend> {
    config: GpuConfig,
    state: Arc<Mutex<PoolState>>,
    backend: Arc<B>,
}

impl<B: ModelBackend + 'static> GpuModelPool<B> {
    /// Build a pool with an explicit backend (useful for testing).
    pub fn new_with_backend(config: GpuConfig, backend: B) -> Self {
        let policy = config.eviction_policy;
        Self {
            state: Arc::new(Mutex::new(PoolState::new(policy))),
            config,
            backend: Arc::new(backend),
        }
    }

    /// Load a model into the pool.
    ///
    /// If the model is already loaded, this is a no-op.
    /// If the pool is full or out of VRAM budget, the least-recently-used
    /// model is evicted first.
    ///
    /// # Errors
    ///
    /// - [`GpuError::VramBudgetExceeded`] — model is too large even after eviction.
    /// - [`GpuError::PoolFull`] — `max_models` reached and eviction yielded nothing.
    pub async fn load_model(&self, model_id: &str, weight_mb: u64) -> Result<(), GpuError> {
        {
            // Lock is intentionally dropped at the end of this block so that
            // `make_room` (which also acquires the lock) can proceed without deadlock.
            let mut state = self.state.lock().await;
            if state.slots.contains_key(model_id) {
                debug!(model_id, "model already loaded");
                state.eviction.record_access(model_id);
                return Ok(());
            }
        }

        // Evict until we have enough VRAM and a free slot.
        self.make_room(weight_mb).await?;

        // Call the backend to actually load.
        self.backend.load(model_id, weight_mb).await?;

        let mut state = self.state.lock().await;
        state.slots.insert(
            model_id.to_owned(),
            ModelSlot {
                model_id: model_id.to_owned(),
                weight_mb,
            },
        );
        state.used_weight_mb += weight_mb;
        state.eviction.insert(model_id);
        info!(model_id, weight_mb, used_mb = state.used_weight_mb, "model loaded");
        Ok(())
    }

    /// Evict a model from the pool, freeing its VRAM.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::ModelNotLoaded`] if the model is not in the pool.
    pub async fn evict_model(&self, model_id: &str) -> Result<(), GpuError> {
        let slot = {
            let mut state = self.state.lock().await;
            match state.slots.remove(model_id) {
                Some(slot) => {
                    state.used_weight_mb =
                        state.used_weight_mb.saturating_sub(slot.weight_mb);
                    state.eviction.remove(model_id);
                    slot
                }
                None => return Err(GpuError::ModelNotLoaded(model_id.to_owned())),
            }
        };
        self.backend.unload(model_id).await?;
        info!(model_id, freed_mb = slot.weight_mb, "model evicted");
        Ok(())
    }

    /// Run inference on a loaded model.
    ///
    /// Records the access so the LRU tracker stays up to date.
    ///
    /// # Errors
    ///
    /// - [`GpuError::ModelNotLoaded`] — model is not currently in the pool.
    pub async fn infer(&self, model_id: &str, prompt: &str) -> Result<String, GpuError> {
        {
            let mut state = self.state.lock().await;
            if !state.slots.contains_key(model_id) {
                return Err(GpuError::ModelNotLoaded(model_id.to_owned()));
            }
            state.eviction.record_access(model_id);
        }
        self.backend.infer(model_id, prompt).await
    }

    /// List currently loaded model slots.
    pub async fn loaded_models(&self) -> Vec<ModelSlot> {
        let state = self.state.lock().await;
        state.slots.values().cloned().collect()
    }

    /// VRAM currently consumed by model weights (MB).
    pub async fn used_weight_mb(&self) -> u64 {
        self.state.lock().await.used_weight_mb
    }

    /// VRAM available for additional model weights (MB).
    pub async fn available_weight_mb(&self) -> u64 {
        let state = self.state.lock().await;
        state.available_mb(self.config.vram_budget_mb)
    }

    /// Returns `true` if the given model is currently loaded.
    pub async fn is_loaded(&self, model_id: &str) -> bool {
        self.state.lock().await.slots.contains_key(model_id)
    }

    // ── private helpers ────────────────────────────────────────────────────

    /// Evict models until there is room for `needed_mb` of weights and the
    /// slot count is below `max_models`.
    async fn make_room(&self, needed_mb: u64) -> Result<(), GpuError> {
        loop {
            let (slot_count, available, candidate) = {
                let mut state = self.state.lock().await;
                let available = state.available_mb(self.config.vram_budget_mb);
                let count = state.slots.len();
                let fits_in_budget = available >= needed_mb;
                let fits_in_slots = count < self.config.max_models;

                if fits_in_budget && fits_in_slots {
                    return Ok(());
                }

                (count, available, state.eviction.evict_candidate())
            };

            match candidate {
                None => {
                    // Nothing to evict.
                    if slot_count >= self.config.max_models {
                        return Err(GpuError::PoolFull(self.config.max_models));
                    }
                    return Err(GpuError::VramBudgetExceeded {
                        requested_mb: needed_mb,
                        available_mb: available,
                    });
                }
                Some(victim) => {
                    warn!(victim, "evicting model to make room");
                    self.evict_model(&victim).await?;
                }
            }
        }
    }
}

impl GpuModelPool<SimulatedModelBackend> {
    /// Build a pool using the simulated (no-GPU) backend.
    pub fn new(config: GpuConfig) -> Self {
        Self::new_with_backend(config, SimulatedModelBackend)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GpuConfig;

    fn small_config(max_models: usize, vram_budget_mb: u64) -> GpuConfig {
        GpuConfig {
            vram_budget_mb,
            max_models,
            ..GpuConfig::default()
        }
    }

    #[tokio::test]
    async fn load_and_infer_single_model() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        pool.load_model("chat-8b", 2_048).await.unwrap();
        assert!(pool.is_loaded("chat-8b").await);
        let output = pool.infer("chat-8b", "Hello world").await.unwrap();
        assert!(output.contains("chat-8b"));
    }

    #[tokio::test]
    async fn vram_accounting_updated_on_load() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        pool.load_model("m1", 1_024).await.unwrap();
        pool.load_model("m2", 512).await.unwrap();
        assert_eq!(pool.used_weight_mb().await, 1_536);
        assert_eq!(pool.available_weight_mb().await, 10_000 - 1_536);
    }

    #[tokio::test]
    async fn evict_frees_vram() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        pool.load_model("m1", 2_000).await.unwrap();
        assert_eq!(pool.used_weight_mb().await, 2_000);

        pool.evict_model("m1").await.unwrap();
        assert!(!pool.is_loaded("m1").await);
        assert_eq!(pool.used_weight_mb().await, 0);
    }

    #[tokio::test]
    async fn evict_not_loaded_returns_error() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        let err = pool.evict_model("ghost").await.unwrap_err();
        assert!(matches!(err, GpuError::ModelNotLoaded(_)));
    }

    #[tokio::test]
    async fn load_same_model_twice_is_idempotent() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        pool.load_model("m1", 1_000).await.unwrap();
        pool.load_model("m1", 1_000).await.unwrap(); // second call is no-op
        assert_eq!(pool.used_weight_mb().await, 1_000);
        assert_eq!(pool.loaded_models().await.len(), 1);
    }

    #[tokio::test]
    async fn pool_evicts_lru_when_max_models_reached() {
        // max_models = 2, load three → first loaded should be evicted.
        let pool = GpuModelPool::new(small_config(2, 10_000));
        pool.load_model("a", 500).await.unwrap();
        pool.load_model("b", 500).await.unwrap();

        // Access "a" so "b" becomes LRU.
        pool.infer("a", "ping").await.unwrap();

        // Loading "c" should evict "b" (LRU).
        pool.load_model("c", 500).await.unwrap();

        assert!(pool.is_loaded("a").await);
        assert!(!pool.is_loaded("b").await, "b should have been evicted");
        assert!(pool.is_loaded("c").await);
    }

    #[tokio::test]
    async fn vram_budget_exceeded_error_when_model_too_large() {
        let pool = GpuModelPool::new(small_config(4, 100)); // tiny budget
        let err = pool.load_model("huge", 200).await.unwrap_err();
        assert!(matches!(err, GpuError::VramBudgetExceeded { .. }));
    }

    #[tokio::test]
    async fn infer_not_loaded_model_returns_error() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        let err = pool.infer("ghost", "hi").await.unwrap_err();
        assert!(matches!(err, GpuError::ModelNotLoaded(_)));
    }

    #[tokio::test]
    async fn pool_reports_loaded_models() {
        let pool = GpuModelPool::new(small_config(4, 10_000));
        pool.load_model("x", 500).await.unwrap();
        pool.load_model("y", 600).await.unwrap();
        let models = pool.loaded_models().await;
        assert_eq!(models.len(), 2);
        let names: Vec<&str> = models.iter().map(|s| s.model_id.as_str()).collect();
        assert!(names.contains(&"x"));
        assert!(names.contains(&"y"));
    }
}
