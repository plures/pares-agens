//! `pares-agens-ensemble` — BitNet MoE-style expert routing for Pares Agens.
//!
//! Runs a pool of specialised BitNet models as a mixture-of-experts (MoE)
//! ensemble, enabling ~120 B effective capacity on 20 GB GPU + any CPU.
//!
//! # Architecture
//!
//! ```text
//! Gate (cerebellum gating function)
//!     │
//!     ├─ GpuHot  pool  (4–5 active experts, ~40 B)
//!     │   └─ CUDA streams, instant activation
//!     │
//!     ├─ CpuWarm pool  (3–5 experts, ~24–40 B)
//!     │   └─ bitnet.cpp, 5–7 tok/s, background tasks
//!     │
//!     └─ ColdStorage   (remaining experts, ~40 B+)
//!         └─ On-disk, load-on-demand in <2 s
//! ```
//!
//! # Modules
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`expert`] | [`Expert`](expert::Expert) struct — domain-specialist model with metrics |
//! | [`pool`] | [`ExpertPool`](pool::ExpertPool) — hot / warm / cold tier management and expert graduation |
//! | [`gate`] | [`Gate`](gate::Gate) — cerebellum gating function, routing weights, self-tuning |
//! | [`scheduler`] | [`Scheduler`](scheduler::Scheduler) — CPU+GPU hybrid assignment |
//! | [`consensus`] | [`ConsensusEngine`](consensus::ConsensusEngine) — multi-expert query + output merging |
//! | [`benchmark`] | [`EnsembleBenchmark`](benchmark::EnsembleBenchmark) — ensemble vs single-model comparison |

pub mod benchmark;
pub mod consensus;
pub mod expert;
pub mod gate;
pub mod pool;
pub mod scheduler;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during ensemble operations.
#[derive(Debug, Error)]
pub enum EnsembleError {
    /// No expert is available for the requested domain.
    #[error("no expert available for domain: {0:?}")]
    NoExpertAvailable(ExpertDomain),

    /// An expert with the given identifier could not be found.
    #[error("expert not found: {0}")]
    ExpertNotFound(String),

    /// The pool configuration is invalid.
    #[error("invalid pool configuration: {0}")]
    InvalidPoolConfig(String),

    /// Consensus merging failed because fewer than 2 responses were collected.
    #[error("consensus requires at least 2 responses, got {0}")]
    InsufficientConsensusResponses(usize),

    /// An expert response was empty or could not be parsed.
    #[error("expert response error: {0}")]
    ExpertResponseError(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── ExpertDomain ──────────────────────────────────────────────────────────────

/// The specialisation domain of a BitNet expert model.
///
/// Each variant corresponds to one fine-tuned expert trained on domain-specific
/// data.  [`ExpertDomain::General`] is the catch-all fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpertDomain {
    /// Source-code understanding and generation.
    Code,
    /// Mathematical reasoning and symbolic computation.
    Math,
    /// Long-form prose, creative writing, and editing.
    Writing,
    /// Multi-step logical and causal reasoning.
    Reasoning,
    /// Factual recall and question-answering.
    Factual,
    /// Summarisation and information extraction.
    Summarisation,
    /// Dialogue and conversational tasks.
    Dialogue,
    /// Catch-all for queries that do not fit a specialist domain.
    General,
}

impl std::fmt::Display for ExpertDomain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ExpertDomain::Code => "code",
            ExpertDomain::Math => "math",
            ExpertDomain::Writing => "writing",
            ExpertDomain::Reasoning => "reasoning",
            ExpertDomain::Factual => "factual",
            ExpertDomain::Summarisation => "summarisation",
            ExpertDomain::Dialogue => "dialogue",
            ExpertDomain::General => "general",
        };
        f.write_str(s)
    }
}

// ── ComputeTier ───────────────────────────────────────────────────────────────

/// The compute tier an expert currently occupies.
///
/// Tier assignment drives latency expectations and resource budgets:
///
/// | Tier | Backend | Latency | Capacity |
/// |------|---------|---------|----------|
/// | [`GpuHot`](ComputeTier::GpuHot) | CUDA streams | instant | ~40 B |
/// | [`CpuWarm`](ComputeTier::CpuWarm) | bitnet.cpp | 5–7 tok/s | ~24–40 B |
/// | [`ColdStorage`](ComputeTier::ColdStorage) | disk | <2 s load | ~40 B+ |
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeTier {
    /// Actively loaded on GPU; lowest latency.
    GpuHot,
    /// Loaded on CPU via bitnet.cpp; medium latency.
    CpuWarm,
    /// Stored on disk; loaded on demand.
    ColdStorage,
}

impl std::fmt::Display for ComputeTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ComputeTier::GpuHot => "gpu_hot",
            ComputeTier::CpuWarm => "cpu_warm",
            ComputeTier::ColdStorage => "cold_storage",
        };
        f.write_str(s)
    }
}

// ── ExpertMetrics ─────────────────────────────────────────────────────────────

/// Performance metrics tracked for a single expert.
///
/// The [`gate::Gate`] uses these metrics to compute routing weights and to
/// decide when an expert should be promoted to a hotter tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertMetrics {
    /// Total number of queries this expert has handled.
    pub queries_handled: u64,

    /// Running accuracy estimate (0.0 – 1.0).  Updated via exponential moving
    /// average with the alpha in [`EnsembleConfig::ema_alpha`].
    pub accuracy: f32,

    /// Exponential moving average of per-query latency in milliseconds.
    pub avg_latency_ms: f64,
}

impl ExpertMetrics {
    /// Return a zero-initialised metrics record.
    #[must_use]
    pub fn new() -> Self {
        Self {
            queries_handled: 0,
            accuracy: 0.5, // start at 50 % so new experts are not penalised
            avg_latency_ms: 0.0,
        }
    }

    /// Update accuracy using an exponential moving average.
    ///
    /// `alpha` controls the smoothing: values close to 1 weight recent
    /// observations heavily; values close to 0 are slow to adapt.
    /// Must be in `(0.0, 1.0]`.
    pub fn update_accuracy(&mut self, outcome: f32, alpha: f32) {
        self.queries_handled += 1;
        self.accuracy = alpha * outcome + (1.0 - alpha) * self.accuracy;
    }

    /// Update the latency EMA with a new observation (milliseconds).
    pub fn update_latency(&mut self, latency_ms: f64, alpha: f32) {
        self.avg_latency_ms =
            f64::from(alpha) * latency_ms + (1.0 - f64::from(alpha)) * self.avg_latency_ms;
    }
}

impl Default for ExpertMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ── EnsembleConfig ────────────────────────────────────────────────────────────

/// Top-level configuration for the BitNet ensemble.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsembleConfig {
    /// Maximum number of experts that may reside in the GPU-hot pool.
    pub gpu_hot_capacity: usize,

    /// Maximum number of experts that may reside in the CPU-warm pool.
    pub cpu_warm_capacity: usize,

    /// Smoothing factor for the accuracy and latency exponential moving
    /// averages tracked in [`ExpertMetrics`].  Must be in `(0.0, 1.0]`.
    pub ema_alpha: f32,

    /// Number of experts to consult in consensus mode.  Must be ≥ 2.
    pub consensus_k: usize,

    /// Minimum query count before an expert is eligible for tier promotion.
    pub graduation_min_queries: u64,

    /// Minimum accuracy an expert must sustain to be promoted to a hotter tier.
    pub graduation_min_accuracy: f32,
}

impl EnsembleConfig {
    /// Return a configuration suitable for consumer hardware with a single
    /// 20 GB GPU and a reasonably powerful CPU.
    #[must_use]
    pub fn consumer_hardware() -> Self {
        Self {
            gpu_hot_capacity: 5,
            cpu_warm_capacity: 5,
            ema_alpha: 0.1,
            consensus_k: 2,
            graduation_min_queries: 50,
            graduation_min_accuracy: 0.70,
        }
    }

    /// Validate all fields.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::InvalidPoolConfig`] when any value is out of range.
    pub fn validate(&self) -> Result<(), EnsembleError> {
        if self.gpu_hot_capacity == 0 {
            return Err(EnsembleError::InvalidPoolConfig(
                "gpu_hot_capacity must be at least 1".into(),
            ));
        }
        if self.cpu_warm_capacity == 0 {
            return Err(EnsembleError::InvalidPoolConfig(
                "cpu_warm_capacity must be at least 1".into(),
            ));
        }
        if self.ema_alpha <= 0.0 || self.ema_alpha > 1.0 {
            return Err(EnsembleError::InvalidPoolConfig(format!(
                "ema_alpha must be in (0.0, 1.0], got {}",
                self.ema_alpha
            )));
        }
        if self.consensus_k < 2 {
            return Err(EnsembleError::InvalidPoolConfig(format!(
                "consensus_k must be at least 2, got {}",
                self.consensus_k
            )));
        }
        if self.graduation_min_accuracy < 0.0 || self.graduation_min_accuracy > 1.0 {
            return Err(EnsembleError::InvalidPoolConfig(format!(
                "graduation_min_accuracy must be in [0.0, 1.0], got {}",
                self.graduation_min_accuracy
            )));
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── ExpertDomain ──────────────────────────────────────────────────────

    #[test]
    fn expert_domain_display() {
        assert_eq!(ExpertDomain::Code.to_string(), "code");
        assert_eq!(ExpertDomain::General.to_string(), "general");
    }

    #[test]
    fn expert_domain_roundtrips_json() {
        let d = ExpertDomain::Math;
        let json = serde_json::to_string(&d).unwrap();
        let back: ExpertDomain = serde_json::from_str(&json).unwrap();
        assert_eq!(d, back);
    }

    // ── ComputeTier ───────────────────────────────────────────────────────

    #[test]
    fn compute_tier_display() {
        assert_eq!(ComputeTier::GpuHot.to_string(), "gpu_hot");
        assert_eq!(ComputeTier::CpuWarm.to_string(), "cpu_warm");
        assert_eq!(ComputeTier::ColdStorage.to_string(), "cold_storage");
    }

    #[test]
    fn compute_tier_ordering() {
        assert!(ComputeTier::GpuHot < ComputeTier::CpuWarm);
        assert!(ComputeTier::CpuWarm < ComputeTier::ColdStorage);
    }

    // ── ExpertMetrics ─────────────────────────────────────────────────────

    #[test]
    fn metrics_initialises_at_fifty_percent_accuracy() {
        let m = ExpertMetrics::new();
        assert!((m.accuracy - 0.5).abs() < 1e-6);
        assert_eq!(m.queries_handled, 0);
    }

    #[test]
    fn metrics_update_accuracy_increments_query_count() {
        let mut m = ExpertMetrics::new();
        m.update_accuracy(1.0, 0.1);
        assert_eq!(m.queries_handled, 1);
    }

    #[test]
    fn metrics_ema_accuracy_moves_toward_new_value() {
        let mut m = ExpertMetrics::new();
        // After many perfect outcomes the accuracy should approach 1.0.
        for _ in 0..200 {
            m.update_accuracy(1.0, 0.1);
        }
        assert!(m.accuracy > 0.99);
    }

    #[test]
    fn metrics_ema_latency_initialises_to_first_observation() {
        let mut m = ExpertMetrics::new();
        m.update_latency(42.0, 1.0); // alpha=1 means no averaging
        assert!((m.avg_latency_ms - 42.0).abs() < 1e-9);
    }

    // ── EnsembleConfig ────────────────────────────────────────────────────

    #[test]
    fn consumer_hardware_config_is_valid() {
        assert!(EnsembleConfig::consumer_hardware().validate().is_ok());
    }

    #[test]
    fn config_rejects_zero_gpu_capacity() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.gpu_hot_capacity = 0;
        assert!(matches!(
            cfg.validate(),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }

    #[test]
    fn config_rejects_ema_alpha_above_one() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.ema_alpha = 1.1;
        assert!(matches!(
            cfg.validate(),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }

    #[test]
    fn config_rejects_zero_ema_alpha() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.ema_alpha = 0.0;
        assert!(matches!(
            cfg.validate(),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }

    #[test]
    fn config_rejects_consensus_k_below_two() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.consensus_k = 1;
        assert!(matches!(
            cfg.validate(),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }

    #[test]
    fn config_rejects_accuracy_above_one() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.graduation_min_accuracy = 1.5;
        assert!(matches!(
            cfg.validate(),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }
}
