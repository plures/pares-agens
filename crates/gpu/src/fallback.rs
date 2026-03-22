//! Fallback runner — GPU full → CPU runner → cloud API cascade.
//!
//! When the [`crate::pool::GpuModelPool`] cannot serve a request (pool full,
//! VRAM exhausted, model not loaded) it hands the request to a
//! [`FallbackRunner`] that tries each configured [`FallbackTarget`] in order.
//!
//! # Design
//!
//! The runner is intentionally backend-agnostic: each [`FallbackTarget`]
//! carries an opaque `endpoint` string so callers can plug in a real HTTP
//! client, a local `llama.cpp` process runner, etc.  The provided
//! [`FallbackRunner::run`] method calls the registered
//! [`InferenceBackend`] for each target in priority order.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::GpuError;

// ── FallbackTarget ────────────────────────────────────────────────────────────

/// A single stage in the fallback cascade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackTarget {
    /// Human-readable label (e.g. `"gpu"`, `"cpu"`, `"cloud"`).
    pub label: String,

    /// Kind of backend at this stage.
    pub kind: FallbackKind,

    /// Opaque endpoint passed to the backend (e.g. a URL or path).
    pub endpoint: String,
}

impl FallbackTarget {
    /// Convenience constructor.
    pub fn new(
        label: impl Into<String>,
        kind: FallbackKind,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            label: label.into(),
            kind,
            endpoint: endpoint.into(),
        }
    }
}

/// Category of backend in a [`FallbackTarget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackKind {
    /// GPU pool — should not normally appear in the fallback list but kept for
    /// completeness (e.g. secondary GPU).
    Gpu,
    /// Local CPU inference (e.g. `llama.cpp`).
    Cpu,
    /// Remote cloud API (e.g. OpenAI, Azure, etc.).
    Cloud,
}

// ── InferenceBackend ──────────────────────────────────────────────────────────

/// Request payload forwarded to each fallback backend.
#[derive(Debug, Clone)]
pub struct InferenceRequest {
    /// Model identifier.
    pub model_id: String,
    /// Prompt text.
    pub prompt: String,
}

/// Response from a backend.
#[derive(Debug, Clone)]
pub struct InferenceResponse {
    /// Which target served the request.
    pub served_by: String,
    /// Generated text.
    pub text: String,
}

/// Pluggable inference backend called by [`FallbackRunner`] for each target.
///
/// Implement this trait on a real HTTP client or local runner.
/// A [`SimulatedBackend`] is provided for tests.
#[async_trait]
pub trait InferenceBackend: Send + Sync {
    /// Attempt to run inference.
    ///
    /// Return `Ok(response)` on success or `Err` if this backend cannot serve
    /// the request (the runner will move to the next target).
    async fn run(
        &self,
        target: &FallbackTarget,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, GpuError>;
}

// ── SimulatedBackend ──────────────────────────────────────────────────────────

/// A simulated backend that succeeds or fails based on a pre-configured list
/// of labels.  Used in tests.
#[derive(Debug)]
pub struct SimulatedBackend {
    /// Labels of targets that will succeed.
    succeeding_labels: Vec<String>,
}

impl SimulatedBackend {
    /// Create a backend that only succeeds for the given target labels.
    pub fn new(succeeding_labels: &[&str]) -> Self {
        Self {
            succeeding_labels: succeeding_labels.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[async_trait]
impl InferenceBackend for SimulatedBackend {
    async fn run(
        &self,
        target: &FallbackTarget,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, GpuError> {
        if self.succeeding_labels.contains(&target.label) {
            Ok(InferenceResponse {
                served_by: target.label.clone(),
                text: format!("response from {} for {}", target.label, request.model_id),
            })
        } else {
            Err(GpuError::FallbackExhausted(request.model_id.clone()))
        }
    }
}

// ── FallbackRunner ────────────────────────────────────────────────────────────

/// Runs a cascade of [`FallbackTarget`]s until one succeeds.
///
/// # Example
///
/// ```no_run
/// use pares_agens_gpu::fallback::{
///     FallbackKind, FallbackRunner, FallbackTarget, InferenceRequest, SimulatedBackend,
/// };
///
/// # async fn example() {
/// let targets = vec![
///     FallbackTarget::new("gpu", FallbackKind::Gpu, "vram://local"),
///     FallbackTarget::new("cpu", FallbackKind::Cpu, "cpu://local"),
///     FallbackTarget::new("cloud", FallbackKind::Cloud, "https://api.openai.com"),
/// ];
/// // Only the CPU backend will succeed in this simulation.
/// let backend = SimulatedBackend::new(&["cpu"]);
/// let runner = FallbackRunner::new(targets, backend);
///
/// let request = InferenceRequest { model_id: "8b".into(), prompt: "Hello".into() };
/// let response = runner.run(&request).await.unwrap();
/// assert_eq!(response.served_by, "cpu");
/// # }
/// ```
pub struct FallbackRunner<B: InferenceBackend> {
    targets: Vec<FallbackTarget>,
    backend: B,
}

impl<B: InferenceBackend> FallbackRunner<B> {
    /// Create a new runner with the given targets (tried in order) and backend.
    pub fn new(targets: Vec<FallbackTarget>, backend: B) -> Self {
        Self { targets, backend }
    }

    /// Try each target in order, returning the first successful response.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::FallbackExhausted`] if all targets fail.
    pub async fn run(&self, request: &InferenceRequest) -> Result<InferenceResponse, GpuError> {
        for target in &self.targets {
            debug!(label = %target.label, model = %request.model_id, "trying fallback target");
            match self.backend.run(target, request).await {
                Ok(resp) => {
                    debug!(label = %target.label, "fallback target succeeded");
                    return Ok(resp);
                }
                Err(e) => {
                    warn!(label = %target.label, error = %e, "fallback target failed, trying next");
                }
            }
        }
        Err(GpuError::FallbackExhausted(request.model_id.clone()))
    }

    /// Returns the configured targets in order.
    pub fn targets(&self) -> &[FallbackTarget] {
        &self.targets
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu_cpu_cloud_targets() -> Vec<FallbackTarget> {
        vec![
            FallbackTarget::new("gpu", FallbackKind::Gpu, "vram://local"),
            FallbackTarget::new("cpu", FallbackKind::Cpu, "cpu://local"),
            FallbackTarget::new("cloud", FallbackKind::Cloud, "https://api.example.com"),
        ]
    }

    fn req(model: &str) -> InferenceRequest {
        InferenceRequest {
            model_id: model.into(),
            prompt: "Hello".into(),
        }
    }

    #[tokio::test]
    async fn first_succeeding_target_is_used() {
        let backend = SimulatedBackend::new(&["gpu"]);
        let runner = FallbackRunner::new(gpu_cpu_cloud_targets(), backend);
        let resp = runner.run(&req("8b")).await.unwrap();
        assert_eq!(resp.served_by, "gpu");
    }

    #[tokio::test]
    async fn falls_back_to_cpu_when_gpu_fails() {
        let backend = SimulatedBackend::new(&["cpu"]);
        let runner = FallbackRunner::new(gpu_cpu_cloud_targets(), backend);
        let resp = runner.run(&req("8b")).await.unwrap();
        assert_eq!(resp.served_by, "cpu");
    }

    #[tokio::test]
    async fn falls_back_to_cloud_when_gpu_and_cpu_fail() {
        let backend = SimulatedBackend::new(&["cloud"]);
        let runner = FallbackRunner::new(gpu_cpu_cloud_targets(), backend);
        let resp = runner.run(&req("8b")).await.unwrap();
        assert_eq!(resp.served_by, "cloud");
    }

    #[tokio::test]
    async fn all_targets_fail_returns_fallback_exhausted() {
        let backend = SimulatedBackend::new(&[]);
        let runner = FallbackRunner::new(gpu_cpu_cloud_targets(), backend);
        let err = runner.run(&req("8b")).await.unwrap_err();
        assert!(matches!(err, GpuError::FallbackExhausted(_)));
    }

    #[tokio::test]
    async fn empty_targets_returns_fallback_exhausted() {
        let backend = SimulatedBackend::new(&["gpu"]);
        let runner = FallbackRunner::new(vec![], backend);
        let err = runner.run(&req("8b")).await.unwrap_err();
        assert!(matches!(err, GpuError::FallbackExhausted(_)));
    }

    #[test]
    fn fallback_target_kind_roundtrips_json() {
        for kind in [FallbackKind::Gpu, FallbackKind::Cpu, FallbackKind::Cloud] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: FallbackKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }
}
