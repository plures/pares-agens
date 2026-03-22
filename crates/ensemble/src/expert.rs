//! Domain-specialist expert model.
//!
//! An [`Expert`] represents a single fine-tuned BitNet model occupying one of
//! the three compute tiers (GPU-hot, CPU-warm, cold-storage).  It carries its
//! own [`ExpertMetrics`] so that the [`crate::gate::Gate`] can make
//! data-driven routing decisions.

use serde::{Deserialize, Serialize};

use crate::{ComputeTier, EnsembleError, ExpertDomain, ExpertMetrics};

// ── Expert ────────────────────────────────────────────────────────────────────

/// A single BitNet expert model.
///
/// # Example
/// ```
/// use pares_agens_ensemble::{ExpertDomain, ComputeTier};
/// use pares_agens_ensemble::expert::Expert;
///
/// let expert = Expert::new("code-expert-1", ExpertDomain::Code, "models/code_8b.gguf", 8.0);
/// assert_eq!(expert.domain, ExpertDomain::Code);
/// assert_eq!(expert.tier, ComputeTier::ColdStorage);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expert {
    /// Unique identifier (e.g. `"code-expert-1"`).
    pub id: String,

    /// Domain this expert was fine-tuned for.
    pub domain: ExpertDomain,

    /// Path to the model weights on disk (GGUF / BitNet format).
    pub model_path: String,

    /// Current compute tier.
    pub tier: ComputeTier,

    /// Approximate parameter count in billions (e.g. `8.0` for an 8 B model).
    pub parameters_b: f32,

    /// Accumulated performance metrics.
    pub metrics: ExpertMetrics,
}

impl Expert {
    /// Create a new expert starting in cold storage.
    ///
    /// New experts are placed in [`ComputeTier::ColdStorage`] until they
    /// accumulate enough queries to be promoted by the
    /// [`crate::pool::ExpertPool`].
    #[must_use]
    pub fn new(id: &str, domain: ExpertDomain, model_path: &str, parameters_b: f32) -> Self {
        Self {
            id: id.to_string(),
            domain,
            model_path: model_path.to_string(),
            tier: ComputeTier::ColdStorage,
            parameters_b,
            metrics: ExpertMetrics::new(),
        }
    }

    /// Validate that the expert's fields are well-formed.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::ExpertNotFound`] when `id` or `model_path` is
    /// empty, or [`EnsembleError::InvalidPoolConfig`] when `parameters_b` is
    /// not positive.
    pub fn validate(&self) -> Result<(), EnsembleError> {
        if self.id.trim().is_empty() {
            return Err(EnsembleError::ExpertNotFound(
                "expert id must not be empty".into(),
            ));
        }
        if self.model_path.trim().is_empty() {
            return Err(EnsembleError::InvalidPoolConfig(
                "model_path must not be empty".into(),
            ));
        }
        if self.parameters_b <= 0.0 {
            return Err(EnsembleError::InvalidPoolConfig(format!(
                "parameters_b must be positive, got {}",
                self.parameters_b
            )));
        }
        Ok(())
    }

    /// Record the outcome of a query and update latency, forwarding to the
    /// embedded [`ExpertMetrics`].
    ///
    /// `outcome` should be `1.0` for a correct/good response and `0.0` for an
    /// incorrect/poor one.  Intermediate values express partial credit.
    pub fn record_outcome(&mut self, outcome: f32, latency_ms: f64, ema_alpha: f32) {
        self.metrics.update_accuracy(outcome, ema_alpha);
        self.metrics.update_latency(latency_ms, ema_alpha);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_expert() -> Expert {
        Expert::new("e1", ExpertDomain::Code, "models/code.gguf", 8.0)
    }

    #[test]
    fn new_expert_starts_in_cold_storage() {
        let e = make_expert();
        assert_eq!(e.tier, ComputeTier::ColdStorage);
    }

    #[test]
    fn new_expert_has_zero_queries() {
        let e = make_expert();
        assert_eq!(e.metrics.queries_handled, 0);
    }

    #[test]
    fn validate_accepts_valid_expert() {
        assert!(make_expert().validate().is_ok());
    }

    #[test]
    fn validate_rejects_empty_id() {
        let mut e = make_expert();
        e.id = String::new();
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_rejects_empty_model_path() {
        let mut e = make_expert();
        e.model_path = String::new();
        assert!(e.validate().is_err());
    }

    #[test]
    fn validate_rejects_non_positive_parameters() {
        let mut e = make_expert();
        e.parameters_b = 0.0;
        assert!(e.validate().is_err());
    }

    #[test]
    fn record_outcome_increments_query_count() {
        let mut e = make_expert();
        e.record_outcome(1.0, 50.0, 0.1);
        assert_eq!(e.metrics.queries_handled, 1);
    }

    #[test]
    fn expert_roundtrips_json() {
        let e = make_expert();
        let json = serde_json::to_string(&e).unwrap();
        let back: Expert = serde_json::from_str(&json).unwrap();
        assert_eq!(e.id, back.id);
        assert_eq!(e.domain, back.domain);
        assert_eq!(e.tier, back.tier);
    }
}
