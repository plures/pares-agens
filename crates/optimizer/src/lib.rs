//! `pares-agens-optimizer` — Native max-min optimization engine for Pares Agens.
//!
//! Provides a max-min style optimization lane that fine-tuned model policies can
//! plug into.  The runtime optimization logic lives here; orchestration and
//! control-plane decisions remain in `praxis-business`.
//!
//! # Architecture
//!
//! ```text
//! OptimizerInput  ──►  MaxMinOptimizer  ──►  OptimizationResult
//!                            │
//!                     TelemetryEmitter
//!                    (score / violations / convergence)
//! ```
//!
//! # Modules
//!
//! - [`engine`]    — [`MaxMinOptimizer`](engine::MaxMinOptimizer) that drives the
//!   iterative max-min search.
//! - [`telemetry`] — [`TelemetryEmitter`](telemetry::TelemetryEmitter) and
//!   [`ObservabilityEvent`](telemetry::ObservabilityEvent) for structured observability.
//! - [`benchmark`] — [`BenchmarkHarness`](benchmark::BenchmarkHarness) for comparing
//!   baseline policy vs optimized policy.

#![warn(missing_docs)]

pub mod benchmark;
pub mod engine;
pub mod telemetry;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors that can occur during optimization.
#[derive(Debug, Error)]
pub enum OptimizerError {
    /// A configuration value is out of the acceptable range.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// A required constraint is violated and optimization cannot continue.
    #[error("constraint violation: {0}")]
    ConstraintViolation(String),

    /// The optimizer did not converge within the allowed iteration budget.
    #[error("did not converge after {0} iterations")]
    NoConvergence(u32),

    /// The objective function returned a non-finite value.
    #[error("objective evaluation failed: {0}")]
    ObjectiveError(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Constraint ────────────────────────────────────────────────────────────────

/// A named scalar constraint that the optimizer must respect.
///
/// The optimizer treats a constraint as satisfied when `value >= lower_bound`
/// (i.e. the constraint is expressed in "greater-or-equal" form).  Set
/// `lower_bound` to `f64::NEG_INFINITY` to express an unconstrained dimension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    /// Human-readable name (e.g. `"latency_ms"`).
    pub name: String,

    /// Current evaluated value of this constraint.
    pub value: f64,

    /// Minimum acceptable value.  Constraint is violated when `value < lower_bound`.
    pub lower_bound: f64,
}

impl Constraint {
    /// Return `true` when this constraint is currently satisfied.
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.value >= self.lower_bound
    }
}

// ── Objective ─────────────────────────────────────────────────────────────────

/// The objective function specification.
///
/// In max-min optimization the goal is to **maximise the minimum** reward across
/// all agents / policies.  `weights` allows you to express per-dimension
/// relative importance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Objective {
    /// Per-dimension scores (one per policy or agent).  All values should be
    /// finite real numbers.
    pub scores: Vec<f64>,

    /// Optional per-dimension weights for the weighted min computation.
    /// When `None`, uniform weights of `1.0` are assumed.
    pub weights: Option<Vec<f64>>,
}

impl Objective {
    /// Compute the (weighted) minimum score — the quantity the optimizer maximises.
    ///
    /// # Errors
    ///
    /// Returns [`OptimizerError::ObjectiveError`] when `scores` is empty,
    /// weights length mismatches scores, or a non-finite score is encountered.
    pub fn evaluate(&self) -> Result<f64, OptimizerError> {
        if self.scores.is_empty() {
            return Err(OptimizerError::ObjectiveError(
                "scores must be non-empty".into(),
            ));
        }
        let weights = match &self.weights {
            Some(w) => {
                if w.len() != self.scores.len() {
                    return Err(OptimizerError::ObjectiveError(format!(
                        "weights length {} != scores length {}",
                        w.len(),
                        self.scores.len()
                    )));
                }
                w.clone()
            }
            None => vec![1.0_f64; self.scores.len()],
        };

        let mut min_val = f64::INFINITY;
        for (&score, &weight) in self.scores.iter().zip(weights.iter()) {
            if !score.is_finite() {
                return Err(OptimizerError::ObjectiveError(format!(
                    "non-finite score: {score}"
                )));
            }
            if weight <= 0.0 {
                return Err(OptimizerError::ObjectiveError(format!(
                    "weight must be positive, got {weight}"
                )));
            }
            let weighted = score * weight;
            if weighted < min_val {
                min_val = weighted;
            }
        }
        Ok(min_val)
    }
}

// ── OptimizerInput ────────────────────────────────────────────────────────────

/// Structured input to the optimizer for a single optimization episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizerInput {
    /// Unique identifier for this optimization run (e.g. UUID string).
    pub run_id: String,

    /// Policy identifier or fine-tuned model name.
    pub policy_id: String,

    /// Objective function specification for this episode.
    pub objective: Objective,

    /// Constraints that must be respected.
    pub constraints: Vec<Constraint>,

    /// Maximum number of optimiser iterations allowed.
    pub max_iterations: u32,

    /// Convergence tolerance: the optimizer stops when the improvement in the
    /// objective between iterations falls below this value.
    pub convergence_tolerance: f64,

    /// Arbitrary key-value context forwarded to telemetry (e.g. model version,
    /// environment tag).
    pub context: std::collections::HashMap<String, String>,
}

impl OptimizerInput {
    /// Validate the input, returning an error if any field is out of range.
    ///
    /// # Errors
    ///
    /// Returns [`OptimizerError::InvalidConfig`] on invalid configuration.
    pub fn validate(&self) -> Result<(), OptimizerError> {
        if self.run_id.trim().is_empty() {
            return Err(OptimizerError::InvalidConfig(
                "run_id must not be empty".into(),
            ));
        }
        if self.policy_id.trim().is_empty() {
            return Err(OptimizerError::InvalidConfig(
                "policy_id must not be empty".into(),
            ));
        }
        if self.max_iterations == 0 {
            return Err(OptimizerError::InvalidConfig(
                "max_iterations must be at least 1".into(),
            ));
        }
        if self.convergence_tolerance < 0.0 {
            return Err(OptimizerError::InvalidConfig(format!(
                "convergence_tolerance must be non-negative, got {}",
                self.convergence_tolerance
            )));
        }
        Ok(())
    }
}

// ── OptimizationResult ────────────────────────────────────────────────────────

/// The output contract returned after a completed optimization run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OptimizationResult {
    /// The `run_id` from the corresponding [`OptimizerInput`].
    pub run_id: String,

    /// The `policy_id` from the corresponding [`OptimizerInput`].
    pub policy_id: String,

    /// Final objective value (the maximised minimum score).
    pub objective_score: f64,

    /// Number of iterations executed before convergence or budget exhaustion.
    pub iterations: u32,

    /// Whether the optimizer converged within tolerance before hitting the
    /// iteration budget.
    pub converged: bool,

    /// Any constraints that were violated in the final solution.
    pub violated_constraints: Vec<String>,
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constraint tests ──────────────────────────────────────────────────

    #[test]
    fn constraint_satisfied_when_value_ge_lower_bound() {
        let c = Constraint {
            name: "latency".into(),
            value: 5.0,
            lower_bound: 3.0,
        };
        assert!(c.is_satisfied());
    }

    #[test]
    fn constraint_violated_when_value_lt_lower_bound() {
        let c = Constraint {
            name: "throughput".into(),
            value: 1.0,
            lower_bound: 2.0,
        };
        assert!(!c.is_satisfied());
    }

    #[test]
    fn constraint_satisfied_at_exact_boundary() {
        let c = Constraint {
            name: "boundary".into(),
            value: 1.0,
            lower_bound: 1.0,
        };
        assert!(c.is_satisfied());
    }

    // ── Objective tests ───────────────────────────────────────────────────

    #[test]
    fn objective_returns_min_score_uniform_weights() {
        let obj = Objective {
            scores: vec![0.9, 0.4, 0.7],
            weights: None,
        };
        let result = obj.evaluate().unwrap();
        assert!((result - 0.4).abs() < 1e-9);
    }

    #[test]
    fn objective_returns_weighted_min() {
        let obj = Objective {
            scores: vec![1.0, 2.0],
            weights: Some(vec![2.0, 1.0]),
        };
        // weighted: [2.0, 2.0] → min = 2.0
        let result = obj.evaluate().unwrap();
        assert!((result - 2.0).abs() < 1e-9);
    }

    #[test]
    fn objective_errors_on_empty_scores() {
        let obj = Objective {
            scores: vec![],
            weights: None,
        };
        assert!(matches!(
            obj.evaluate(),
            Err(OptimizerError::ObjectiveError(_))
        ));
    }

    #[test]
    fn objective_errors_on_weight_length_mismatch() {
        let obj = Objective {
            scores: vec![1.0, 2.0],
            weights: Some(vec![1.0]),
        };
        assert!(matches!(
            obj.evaluate(),
            Err(OptimizerError::ObjectiveError(_))
        ));
    }

    #[test]
    fn objective_errors_on_non_finite_score() {
        let obj = Objective {
            scores: vec![1.0, f64::NAN],
            weights: None,
        };
        assert!(matches!(
            obj.evaluate(),
            Err(OptimizerError::ObjectiveError(_))
        ));
    }

    // ── OptimizerInput validation tests ──────────────────────────────────

    #[test]
    fn input_validate_rejects_empty_run_id() {
        let input = OptimizerInput {
            run_id: "".into(),
            policy_id: "p".into(),
            objective: Objective {
                scores: vec![1.0],
                weights: None,
            },
            constraints: vec![],
            max_iterations: 10,
            convergence_tolerance: 1e-4,
            context: std::collections::HashMap::new(),
        };
        assert!(matches!(
            input.validate(),
            Err(OptimizerError::InvalidConfig(_))
        ));
    }

    #[test]
    fn input_validate_rejects_zero_iterations() {
        let input = OptimizerInput {
            run_id: "r".into(),
            policy_id: "p".into(),
            objective: Objective {
                scores: vec![1.0],
                weights: None,
            },
            constraints: vec![],
            max_iterations: 0,
            convergence_tolerance: 1e-4,
            context: std::collections::HashMap::new(),
        };
        assert!(matches!(
            input.validate(),
            Err(OptimizerError::InvalidConfig(_))
        ));
    }

    #[test]
    fn input_validate_rejects_negative_tolerance() {
        let input = OptimizerInput {
            run_id: "r".into(),
            policy_id: "p".into(),
            objective: Objective {
                scores: vec![1.0],
                weights: None,
            },
            constraints: vec![],
            max_iterations: 10,
            convergence_tolerance: -0.1,
            context: std::collections::HashMap::new(),
        };
        assert!(matches!(
            input.validate(),
            Err(OptimizerError::InvalidConfig(_))
        ));
    }
}
