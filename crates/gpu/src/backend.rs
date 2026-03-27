//! [`ModelBackend`] trait and [`SimulatedModelBackend`] stub.

use async_trait::async_trait;

use crate::{error::GpuError, pool::InferenceParams};

// ── ModelBackend ──────────────────────────────────────────────────────────────

/// A GPU-resident model that can execute inference requests.
///
/// Implementors are responsible for:
/// - Reporting their VRAM footprint via [`vram_usage_mb`].
/// - Running inference on one or more CUDA streams (or equivalent) via [`generate`].
///
/// # Thread safety
///
/// Implementors **must** be `Send + Sync` because the pool shares them across
/// async tasks.  Each `generate` call should run on its own CUDA stream so
/// multiple requests can be in-flight concurrently.
#[async_trait]
pub trait ModelBackend: Send + Sync {
    /// A short, stable identifier for this model (e.g. `"code-8b"`).
    fn model_id(&self) -> &str;

    /// VRAM consumed by this model's weights (not including KV cache), in MiB.
    fn vram_usage_mb(&self) -> u64;

    /// Run a generation request and return the full output text.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::InferenceFailed`] on any backend error.
    async fn generate(&self, prompt: &str, params: InferenceParams) -> Result<String, GpuError>;
}

// ── SimulatedModelBackend ─────────────────────────────────────────────────────

/// A no-GPU stub that implements [`ModelBackend`] for tests and CI.
///
/// `generate` immediately returns a deterministic echo response without
/// touching any real GPU resources.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::{SimulatedModelBackend, ModelBackend};
///
/// let backend = SimulatedModelBackend::new("test-model", 2_000);
/// assert_eq!(backend.model_id(), "test-model");
/// assert_eq!(backend.vram_usage_mb(), 2_000);
/// ```
pub struct SimulatedModelBackend {
    model_id: String,
    vram_mb: u64,
}

impl SimulatedModelBackend {
    /// Create a new simulated backend with the given model ID and VRAM footprint.
    pub fn new(model_id: impl Into<String>, vram_mb: u64) -> Self {
        Self {
            model_id: model_id.into(),
            vram_mb,
        }
    }
}

impl std::fmt::Debug for SimulatedModelBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimulatedModelBackend")
            .field("model_id", &self.model_id)
            .field("vram_mb", &self.vram_mb)
            .finish()
    }
}

#[async_trait]
impl ModelBackend for SimulatedModelBackend {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn vram_usage_mb(&self) -> u64 {
        self.vram_mb
    }

    async fn generate(&self, prompt: &str, _params: InferenceParams) -> Result<String, GpuError> {
        // Simulate a small async delay (yield so the executor can interleave).
        tokio::task::yield_now().await;
        Ok(format!("[simulated:{} echo] {}", self.model_id, prompt))
    }
}
