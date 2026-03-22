//! Verdict engine — praxis-style keep/discard evaluation.
//!
//! The [`VerdictEngine`] applies a [`VerdictPolicy`] to a [`Measurement`] and
//! [`Hypothesis`] to decide whether to keep (commit) or discard (revert) the
//! mutation from a completed experiment.
//!
//! The built-in [`DefaultVerdictPolicy`] keeps any mutation that improved the
//! metric above a minimum threshold, scaled by the hypothesis confidence.

use crate::{
    hypothesis::Hypothesis,
    measurement::Measurement,
    AutoresearchError, Verdict,
};
use serde::{Deserialize, Serialize};

// ── VerdictInput ──────────────────────────────────────────────────────────────

/// All information available to the verdict engine for a single experiment.
#[derive(Debug, Clone)]
pub struct VerdictInput<'a> {
    /// The measurement outcome of the experiment.
    pub measurement: &'a Measurement,
    /// The hypothesis that motivated the experiment.
    pub hypothesis: &'a Hypothesis,
    /// The praxis guidance active for this run.
    pub praxis_guidance: &'a str,
    /// The current best metric value recorded in the ledger (baseline).
    pub current_best: f64,
}

// ── VerdictOutput ─────────────────────────────────────────────────────────────

/// The verdict and supporting reasoning returned by the engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerdictOutput {
    /// The decision: keep, discard, or error.
    pub verdict: Verdict,
    /// Human-readable justification.
    pub reason: String,
}

// ── VerdictPolicy trait ───────────────────────────────────────────────────────

/// Pluggable verdict policy.
pub trait VerdictPolicy: Send + Sync {
    /// Evaluate the experiment and return a verdict.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError`] only for truly unrecoverable situations
    /// (e.g. NaN metrics).  Ordinary failures (no improvement) should produce
    /// `Verdict::Discard`, not an error.
    fn evaluate(&self, input: &VerdictInput<'_>) -> Result<VerdictOutput, AutoresearchError>;
}

// ── DefaultVerdictPolicy ──────────────────────────────────────────────────────

/// Default praxis verdict: keep if the improvement exceeds a minimum threshold
/// that is adjusted by the hypothesis confidence.
///
/// The effective threshold is `min_improvement_threshold × (1 − confidence)`.
/// A high-confidence hypothesis therefore requires a smaller absolute
/// improvement to be accepted.
#[derive(Debug, Clone)]
pub struct DefaultVerdictPolicy {
    /// Minimum fractional improvement required to keep a mutation.
    ///
    /// Set to `0.0` to keep any improvement, however small.
    pub min_improvement_threshold: f64,
}

impl Default for DefaultVerdictPolicy {
    fn default() -> Self {
        Self {
            min_improvement_threshold: 1e-4,
        }
    }
}

impl VerdictPolicy for DefaultVerdictPolicy {
    fn evaluate(&self, input: &VerdictInput<'_>) -> Result<VerdictOutput, AutoresearchError> {
        let m = input.measurement;

        // Reject NaN/infinite metrics.
        if !m.after.is_finite() || !m.before.is_finite() {
            return Ok(VerdictOutput {
                verdict: Verdict::Error,
                reason: format!(
                    "non-finite metric value: before={}, after={}",
                    m.before, m.after
                ),
            });
        }

        let rel = m.relative_improvement();

        // Effective threshold scales down for high-confidence hypotheses.
        let effective_threshold =
            self.min_improvement_threshold * (1.0 - input.hypothesis.confidence.clamp(0.0, 1.0));

        if m.improved() && rel >= effective_threshold {
            Ok(VerdictOutput {
                verdict: Verdict::Keep,
                reason: format!(
                    "metric {name} improved from {before:.6} to {after:.6} (+{rel:.2}%)",
                    name = m.metric_name,
                    before = m.before,
                    after = m.after,
                    rel = rel * 100.0,
                ),
            })
        } else if m.improved() {
            // Improved but below the threshold — treat as a marginal keep.
            Ok(VerdictOutput {
                verdict: Verdict::Keep,
                reason: format!(
                    "metric {name} marginally improved from {before:.6} to {after:.6} (below threshold {effective_threshold:.2e})",
                    name = m.metric_name,
                    before = m.before,
                    after = m.after,
                ),
            })
        } else {
            Ok(VerdictOutput {
                verdict: Verdict::Discard,
                reason: format!(
                    "metric {name} did not improve: before={before:.6}, after={after:.6}",
                    name = m.metric_name,
                    before = m.before,
                    after = m.after,
                ),
            })
        }
    }
}

// ── VerdictEngine ─────────────────────────────────────────────────────────────

/// Orchestrates verdict evaluation using a pluggable [`VerdictPolicy`].
pub struct VerdictEngine {
    policy: Box<dyn VerdictPolicy>,
}

impl Default for VerdictEngine {
    fn default() -> Self {
        Self {
            policy: Box::new(DefaultVerdictPolicy::default()),
        }
    }
}

impl VerdictEngine {
    /// Create a `VerdictEngine` backed by the default policy.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a `VerdictEngine` with a custom policy.
    #[must_use]
    pub fn with_policy(policy: Box<dyn VerdictPolicy>) -> Self {
        Self { policy }
    }

    /// Evaluate an experiment.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by the underlying policy.
    pub fn evaluate(&self, input: &VerdictInput<'_>) -> Result<VerdictOutput, AutoresearchError> {
        self.policy.evaluate(input)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{hypothesis::Hypothesis, measurement::Measurement, mutation::MutationSet};

    fn hypothesis(confidence: f64) -> Hypothesis {
        Hypothesis {
            statement: "test hypothesis".into(),
            mutation: MutationSet { operators: vec![] },
            confidence,
        }
    }

    fn measurement(before: f64, after: f64, higher_is_better: bool) -> Measurement {
        Measurement {
            metric_name: "val_bpb".into(),
            before,
            after,
            higher_is_better,
        }
    }

    #[test]
    fn keep_on_improvement() {
        let engine = VerdictEngine::new();
        let m = measurement(1.0, 0.8, false); // lower is better
        let h = hypothesis(0.5);
        let input = VerdictInput {
            measurement: &m,
            hypothesis: &h,
            praxis_guidance: "reduce loss",
            current_best: 1.0,
        };
        let out = engine.evaluate(&input).unwrap();
        assert_eq!(out.verdict, Verdict::Keep);
    }

    #[test]
    fn discard_on_regression() {
        let engine = VerdictEngine::new();
        let m = measurement(0.8, 1.0, false); // lower is better; 1.0 > 0.8 = regression
        let h = hypothesis(0.5);
        let input = VerdictInput {
            measurement: &m,
            hypothesis: &h,
            praxis_guidance: "reduce loss",
            current_best: 0.8,
        };
        let out = engine.evaluate(&input).unwrap();
        assert_eq!(out.verdict, Verdict::Discard);
    }

    #[test]
    fn error_on_nan_metric() {
        let engine = VerdictEngine::new();
        let m = measurement(f64::NAN, 0.5, false);
        let h = hypothesis(0.5);
        let input = VerdictInput {
            measurement: &m,
            hypothesis: &h,
            praxis_guidance: "test",
            current_best: 0.5,
        };
        let out = engine.evaluate(&input).unwrap();
        assert_eq!(out.verdict, Verdict::Error);
    }

    #[test]
    fn high_confidence_keeps_smaller_improvement() {
        // High confidence lowers the effective threshold close to 0.
        let engine = VerdictEngine::new();
        let m = measurement(1.0, 1.0 - 1e-6, false); // tiny improvement, lower is better
        let h = hypothesis(0.9999); // near-certainty → effective threshold ~= 0
        let input = VerdictInput {
            measurement: &m,
            hypothesis: &h,
            praxis_guidance: "reduce loss",
            current_best: 1.0,
        };
        let out = engine.evaluate(&input).unwrap();
        assert_eq!(out.verdict, Verdict::Keep);
    }

    #[test]
    fn verdict_engine_with_custom_policy() {
        struct AlwaysDiscard;
        impl VerdictPolicy for AlwaysDiscard {
            fn evaluate(
                &self,
                _input: &VerdictInput<'_>,
            ) -> Result<VerdictOutput, AutoresearchError> {
                Ok(VerdictOutput {
                    verdict: Verdict::Discard,
                    reason: "always discard".into(),
                })
            }
        }

        let engine = VerdictEngine::with_policy(Box::new(AlwaysDiscard));
        let m = measurement(0.5, 0.9, true); // improvement
        let h = hypothesis(0.5);
        let input = VerdictInput {
            measurement: &m,
            hypothesis: &h,
            praxis_guidance: "test",
            current_best: 0.5,
        };
        let out = engine.evaluate(&input).unwrap();
        assert_eq!(out.verdict, Verdict::Discard);
    }
}
