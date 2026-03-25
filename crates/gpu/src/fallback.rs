//! Fallback cascade — GPU → CPU → cloud API.
//!
//! [`FallbackRunner`] wraps a primary (GPU) backend and one or more fallback
//! tiers.  If the primary fails it retries each tier in order, reporting which
//! tier ultimately served the request via [`FallbackTier`].

use std::sync::Arc;

use async_trait::async_trait;

use crate::{backend::ModelBackend, error::GpuError, pool::InferenceParams};

// ── FallbackTier ──────────────────────────────────────────────────────────────

/// The compute tier that served a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackTier {
    /// Served from the GPU pool (fast path).
    Gpu,
    /// Served from a local CPU runner (no GPU required).
    Cpu,
    /// Served from a remote cloud API (last resort).
    Cloud,
}

impl std::fmt::Display for FallbackTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FallbackTier::Gpu => write!(f, "GPU"),
            FallbackTier::Cpu => write!(f, "CPU"),
            FallbackTier::Cloud => write!(f, "Cloud"),
        }
    }
}

// ── CloudBackend ──────────────────────────────────────────────────────────────

/// A backend that delegates to a remote cloud API.
///
/// This is the last tier in the fallback chain.  The default implementation
/// is a no-op stub that always returns [`GpuError::CudaUnavailable`].
/// Production code should replace this with a real HTTP client.
#[async_trait]
pub trait CloudBackend: Send + Sync {
    /// Run a generation request via the cloud API.
    async fn generate(&self, prompt: &str, params: InferenceParams) -> Result<String, GpuError>;
}

/// A stub [`CloudBackend`] that always fails, used when no real cloud client
/// is configured.
pub struct NoCloudBackend;

#[async_trait]
impl CloudBackend for NoCloudBackend {
    async fn generate(&self, _prompt: &str, _params: InferenceParams) -> Result<String, GpuError> {
        Err(GpuError::AllFallbacksFailed {
            reason: "no cloud backend configured".to_owned(),
        })
    }
}

// ── FallbackRunner ────────────────────────────────────────────────────────────

/// Cascades through GPU → CPU → cloud API tiers.
///
/// Each tier is tried in order; the first successful result is returned along
/// with the [`FallbackTier`] that served it.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use pares_agens_gpu::fallback::{FallbackRunner, FallbackTier, NoCloudBackend};
/// use pares_agens_gpu::{SimulatedModelBackend, InferenceParams};
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), pares_agens_gpu::GpuError> {
/// let gpu_backend = Arc::new(SimulatedModelBackend::new("code-8b", 2_000));
/// let cpu_backend = Arc::new(SimulatedModelBackend::new("code-8b-cpu", 0));
/// let cloud = Arc::new(NoCloudBackend);
///
/// let runner = FallbackRunner::new(gpu_backend, cpu_backend, cloud);
///
/// let (output, tier) = runner.run("Hello!", InferenceParams::default()).await?;
/// assert_eq!(tier, FallbackTier::Gpu);
/// println!("served by {tier}: {output}");
/// # Ok(())
/// # }
/// ```
pub struct FallbackRunner {
    gpu: Arc<dyn ModelBackend>,
    cpu: Arc<dyn ModelBackend>,
    cloud: Arc<dyn CloudBackend>,
}

impl FallbackRunner {
    /// Build a runner with explicit GPU, CPU, and cloud tiers.
    pub fn new(
        gpu: Arc<dyn ModelBackend>,
        cpu: Arc<dyn ModelBackend>,
        cloud: Arc<dyn CloudBackend>,
    ) -> Self {
        Self { gpu, cpu, cloud }
    }

    /// Build a runner without a cloud tier (uses [`NoCloudBackend`]).
    pub fn without_cloud(gpu: Arc<dyn ModelBackend>, cpu: Arc<dyn ModelBackend>) -> Self {
        Self::new(gpu, cpu, Arc::new(NoCloudBackend))
    }

    /// Run inference, cascading through tiers on failure.
    ///
    /// Returns `(output, tier)` where `tier` is the tier that succeeded.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::AllFallbacksFailed`] only if every tier fails.
    pub async fn run(
        &self,
        prompt: &str,
        params: InferenceParams,
    ) -> Result<(String, FallbackTier), GpuError> {
        // Tier 1: GPU
        match self.gpu.generate(prompt, params.clone()).await {
            Ok(output) => {
                tracing::debug!("inference served by GPU tier");
                return Ok((output, FallbackTier::Gpu));
            }
            Err(e) => {
                tracing::warn!(error = %e, "GPU tier failed, falling back to CPU");
            }
        }

        // Tier 2: CPU
        match self.cpu.generate(prompt, params.clone()).await {
            Ok(output) => {
                tracing::info!("inference served by CPU tier (GPU unavailable)");
                return Ok((output, FallbackTier::Cpu));
            }
            Err(e) => {
                tracing::warn!(error = %e, "CPU tier failed, falling back to cloud");
            }
        }

        // Tier 3: Cloud
        match self.cloud.generate(prompt, params).await {
            Ok(output) => {
                tracing::warn!("inference served by cloud tier (GPU and CPU unavailable)");
                Ok((output, FallbackTier::Cloud))
            }
            Err(e) => Err(GpuError::AllFallbacksFailed {
                reason: format!("GPU, CPU, and cloud all failed: {e}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SimulatedModelBackend;

    fn sim(id: &str) -> Arc<dyn ModelBackend> {
        Arc::new(SimulatedModelBackend::new(id, 0))
    }

    #[tokio::test]
    async fn gpu_succeeds_first() {
        let runner = FallbackRunner::without_cloud(sim("gpu-model"), sim("cpu-model"));
        let (out, tier) = runner.run("hi", InferenceParams::default()).await.unwrap();
        assert_eq!(tier, FallbackTier::Gpu);
        assert!(out.contains("gpu-model"));
    }

    #[tokio::test]
    async fn falls_back_to_cpu_when_gpu_fails() {
        /// A backend that always fails.
        struct AlwaysFails;
        #[async_trait::async_trait]
        impl ModelBackend for AlwaysFails {
            fn model_id(&self) -> &str {
                "always-fails"
            }
            fn vram_usage_mb(&self) -> u64 {
                0
            }
            async fn generate(
                &self,
                _prompt: &str,
                _params: InferenceParams,
            ) -> Result<String, GpuError> {
                Err(GpuError::CudaUnavailable)
            }
        }

        let runner = FallbackRunner::without_cloud(Arc::new(AlwaysFails), sim("cpu-model"));
        let (out, tier) = runner.run("hi", InferenceParams::default()).await.unwrap();
        assert_eq!(tier, FallbackTier::Cpu);
        assert!(out.contains("cpu-model"));
    }

    #[tokio::test]
    async fn all_tiers_fail_returns_error() {
        struct AlwaysFails;
        #[async_trait::async_trait]
        impl ModelBackend for AlwaysFails {
            fn model_id(&self) -> &str {
                "fail"
            }
            fn vram_usage_mb(&self) -> u64 {
                0
            }
            async fn generate(
                &self,
                _prompt: &str,
                _params: InferenceParams,
            ) -> Result<String, GpuError> {
                Err(GpuError::CudaUnavailable)
            }
        }

        let runner =
            FallbackRunner::without_cloud(Arc::new(AlwaysFails), Arc::new(AlwaysFails));
        let err = runner
            .run("hi", InferenceParams::default())
            .await
            .unwrap_err();
        assert!(matches!(err, GpuError::AllFallbacksFailed { .. }));
    }
}
