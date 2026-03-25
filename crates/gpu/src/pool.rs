//! GPU model pool — loads, evicts, and runs concurrent inference on multiple
//! BitNet models resident on a single GPU.
//!
//! [`GpuModelPool`] is the central type of this crate.  It:
//!
//! - Enforces the VRAM budget from [`GpuConfig`].
//! - Applies LRU eviction when the pool is full.
//! - Dispatches inference requests to the correct [`ModelBackend`].
//! - Delegates KV cache allocation to a shared [`KvCacheManager`].

use std::collections::HashMap;
use std::sync::Arc;

use crate::{
    backend::ModelBackend,
    config::GpuConfig,
    error::GpuError,
    eviction::LruEviction,
    kv_cache::KvCacheManager,
};

// ── InferenceParams ───────────────────────────────────────────────────────────

/// Parameters controlling a single inference request.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::InferenceParams;
///
/// let params = InferenceParams {
///     max_tokens: 256,
///     temperature: 0.7,
///     kv_cache_mb: 256,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct InferenceParams {
    /// Maximum number of tokens to generate.
    pub max_tokens: u32,

    /// Sampling temperature (0 = greedy, 1 = full distribution).
    pub temperature: f32,

    /// KV cache VRAM to reserve for this request, in MiB.
    ///
    /// The pool will try to allocate this from [`KvCacheManager`] before
    /// dispatching to the backend.  Set to 0 to skip KV cache reservation.
    pub kv_cache_mb: u64,
}

impl Default for InferenceParams {
    fn default() -> Self {
        Self {
            max_tokens: 256,
            temperature: 0.8,
            kv_cache_mb: 128,
        }
    }
}

// ── GpuModelPool ─────────────────────────────────────────────────────────────

/// Manages multiple [`ModelBackend`]s resident on a single GPU.
///
/// # Behaviour
///
/// - [`load`] checks VRAM headroom and `max_models`.  If the pool is full it
///   evicts the LRU model before loading the new one.
/// - [`infer`] touches the model in the LRU tracker, allocates KV cache,
///   dispatches to the backend, and releases the KV cache on completion.
/// - [`unload`] explicitly removes a model and reclaims its VRAM.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::{GpuConfig, GpuModelPool, SimulatedModelBackend};
/// use std::sync::Arc;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), pares_agens_gpu::GpuError> {
/// let mut pool = GpuModelPool::new(GpuConfig::default());
/// pool.load(Arc::new(SimulatedModelBackend::new("my-model", 2_000)))?;
///
/// let out = pool.infer("my-model", "Hello!", Default::default()).await?;
/// assert!(!out.is_empty());
/// # Ok(())
/// # }
/// ```
pub struct GpuModelPool {
    config: GpuConfig,
    loaded: HashMap<String, Arc<dyn ModelBackend>>,
    /// Tracks how much VRAM is currently used by loaded model weights.
    vram_used_mb: u64,
    eviction: LruEviction,
    kv_cache: KvCacheManager,
}

impl std::fmt::Debug for GpuModelPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuModelPool")
            .field("vram_budget_mb", &self.config.vram_budget_mb)
            .field("vram_used_mb", &self.vram_used_mb)
            .field("loaded_count", &self.loaded.len())
            .finish_non_exhaustive()
    }
}

impl GpuModelPool {
    /// Create a new, empty pool with the given configuration.
    pub fn new(config: GpuConfig) -> Self {
        let kv_cache = KvCacheManager::new(config.kv_cache_mb);
        Self {
            config,
            loaded: HashMap::new(),
            vram_used_mb: 0,
            eviction: LruEviction::new(),
            kv_cache,
        }
    }

    /// VRAM budget available for model weights (total budget minus KV cache
    /// reservation minus currently loaded weights), in MiB.
    pub fn vram_available_mb(&self) -> u64 {
        self.config
            .vram_budget_mb
            .saturating_sub(self.config.kv_cache_mb)
            .saturating_sub(self.vram_used_mb)
    }

    /// Total VRAM consumed by loaded model weights, in MiB.
    pub fn vram_used_mb(&self) -> u64 {
        self.vram_used_mb
    }

    /// IDs of all currently loaded models.
    pub fn loaded_models(&self) -> Vec<&str> {
        self.loaded.keys().map(String::as_str).collect()
    }

    /// `true` if a model with the given ID is currently loaded.
    pub fn is_loaded(&self, model_id: &str) -> bool {
        self.loaded.contains_key(model_id)
    }

    /// Load a model into the pool.
    ///
    /// If adding this model would exceed the `max_models` limit **or** the
    /// VRAM budget, the LRU model is evicted first.  If eviction is still
    /// insufficient (e.g. the new model is larger than the entire budget)
    /// an error is returned.
    ///
    /// # Errors
    ///
    /// - [`GpuError::AlreadyLoaded`] — a model with this ID is already loaded.
    /// - [`GpuError::InsufficientVram`] — not enough VRAM even after eviction.
    pub fn load(&mut self, backend: Arc<dyn ModelBackend>) -> Result<(), GpuError> {
        let model_id = backend.model_id().to_owned();

        if self.loaded.contains_key(&model_id) {
            return Err(GpuError::AlreadyLoaded { model_id });
        }

        // Evict LRU models until we have enough space.
        self.make_room_for(backend.vram_usage_mb())?;

        tracing::debug!(
            model_id = %model_id,
            vram_mb = backend.vram_usage_mb(),
            "loading model onto GPU pool",
        );

        self.vram_used_mb += backend.vram_usage_mb();
        self.eviction.insert(&model_id);
        self.loaded.insert(model_id, backend);
        Ok(())
    }

    /// Explicitly unload a model, reclaiming its VRAM.
    ///
    /// Returns `true` if the model was loaded, `false` if it was not found.
    pub fn unload(&mut self, model_id: &str) -> bool {
        if let Some(backend) = self.loaded.remove(model_id) {
            self.vram_used_mb = self.vram_used_mb.saturating_sub(backend.vram_usage_mb());
            self.eviction.remove(model_id);
            tracing::debug!(model_id, "unloaded model from GPU pool");
            true
        } else {
            false
        }
    }

    /// Retrieve a reference to a loaded backend.
    pub fn get(&self, model_id: &str) -> Option<Arc<dyn ModelBackend>> {
        self.loaded.get(model_id).cloned()
    }

    /// Run inference on the named model.
    ///
    /// 1. Touches the model in the LRU tracker.
    /// 2. Allocates KV cache (if `params.kv_cache_mb > 0`).
    /// 3. Dispatches to the backend.
    /// 4. Frees the KV cache slot.
    ///
    /// # Errors
    ///
    /// - [`GpuError::ModelNotLoaded`] — the model is not in the pool.
    /// - [`GpuError::KvCacheExhausted`] — not enough KV cache headroom.
    /// - [`GpuError::InferenceFailed`] — backend returned an error.
    pub async fn infer(
        &mut self,
        model_id: &str,
        prompt: &str,
        params: InferenceParams,
    ) -> Result<String, GpuError> {
        let backend = self
            .loaded
            .get(model_id)
            .cloned()
            .ok_or_else(|| GpuError::ModelNotLoaded {
                model_id: model_id.to_owned(),
            })?;

        // Update access order.
        self.eviction.touch(model_id);

        // Reserve KV cache.
        let request_id = generate_request_id();
        if params.kv_cache_mb > 0 {
            self.kv_cache.allocate(&request_id, params.kv_cache_mb)?;
        }

        // Dispatch inference.
        let result = backend.generate(prompt, params).await;

        // Always release the KV cache slot.
        self.kv_cache.free(&request_id);

        result
    }

    // ── private helpers ───────────────────────────────────────────────────────

    /// Evict LRU models until `needed_mb` MiB fits in the weight budget and
    /// the model count is below `max_models`.
    fn make_room_for(&mut self, needed_mb: u64) -> Result<(), GpuError> {
        // Check weight budget (KV cache headroom is already subtracted).
        let weight_budget = self
            .config
            .vram_budget_mb
            .saturating_sub(self.config.kv_cache_mb);

        if needed_mb > weight_budget {
            return Err(GpuError::InsufficientVram {
                needed_mb,
                available_mb: weight_budget,
            });
        }

        // Evict until both constraints are satisfied.
        loop {
            let count_ok = self.loaded.len() < self.config.max_models;
            let vram_ok = self.vram_available_mb() >= needed_mb;

            if count_ok && vram_ok {
                break;
            }

            // Evict the LRU candidate.
            let candidate = self
                .eviction
                .evict_candidate()
                .map(str::to_owned)
                .ok_or(GpuError::InsufficientVram {
                    needed_mb,
                    available_mb: self.vram_available_mb(),
                })?;

            tracing::debug!(model_id = %candidate, "evicting LRU model from GPU pool");
            self.unload(&candidate);
        }

        Ok(())
    }
}

/// Generate a short unique request ID.
fn generate_request_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("req-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::GpuConfig, SimulatedModelBackend};

    fn small_config() -> GpuConfig {
        GpuConfig {
            vram_budget_mb: 10_000,
            max_models: 3,
            kv_cache_mb: 1_000,
            ..GpuConfig::default()
        }
    }

    fn make_model(id: &str, vram_mb: u64) -> Arc<dyn ModelBackend> {
        Arc::new(SimulatedModelBackend::new(id, vram_mb))
    }

    #[test]
    fn load_and_unload() {
        let mut pool = GpuModelPool::new(small_config());
        pool.load(make_model("a", 2_000)).unwrap();
        assert!(pool.is_loaded("a"));
        assert_eq!(pool.vram_used_mb(), 2_000);

        pool.unload("a");
        assert!(!pool.is_loaded("a"));
        assert_eq!(pool.vram_used_mb(), 0);
    }

    #[test]
    fn duplicate_load_returns_error() {
        let mut pool = GpuModelPool::new(small_config());
        pool.load(make_model("a", 2_000)).unwrap();
        let err = pool.load(make_model("a", 2_000)).unwrap_err();
        assert!(matches!(err, GpuError::AlreadyLoaded { .. }));
    }

    #[test]
    fn lru_eviction_on_max_models() {
        let mut pool = GpuModelPool::new(small_config()); // max_models=3
        pool.load(make_model("a", 1_000)).unwrap();
        pool.load(make_model("b", 1_000)).unwrap();
        pool.load(make_model("c", 1_000)).unwrap();

        // Loading "d" should evict "a" (LRU).
        pool.load(make_model("d", 1_000)).unwrap();
        assert!(!pool.is_loaded("a"), "a should have been evicted");
        assert!(pool.is_loaded("d"));
    }

    #[test]
    fn vram_overflow_returns_error() {
        let mut pool = GpuModelPool::new(small_config()); // budget=10_000, kv=1_000 → weight=9_000
        let err = pool.load(make_model("huge", 20_000)).unwrap_err();
        assert!(matches!(err, GpuError::InsufficientVram { .. }));
    }

    #[tokio::test]
    async fn infer_returns_output() {
        let mut pool = GpuModelPool::new(small_config());
        pool.load(make_model("my-model", 2_000)).unwrap();

        let out = pool
            .infer("my-model", "Hello!", Default::default())
            .await
            .unwrap();
        assert!(out.contains("my-model"));
    }

    #[tokio::test]
    async fn infer_model_not_loaded_returns_error() {
        let mut pool = GpuModelPool::new(small_config());
        let err = pool
            .infer("ghost", "Hello!", Default::default())
            .await
            .unwrap_err();
        assert!(matches!(err, GpuError::ModelNotLoaded { .. }));
    }
}
