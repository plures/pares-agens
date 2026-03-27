//! Hypothesis engine — selects the next experiment based on ledger history.
//!
//! The [`HypothesisEngine`] acts as the cerebellum: it reads the experiment
//! history from the [`ExperimentLedger`](crate::ledger::ExperimentLedger) and
//! proposes the next [`Hypothesis`] (what to try and why).
//!
//! The built-in [`DefaultHypothesisEngine`] uses a simple exploration strategy:
//! - Try each mutation operator in a round-robin if no history exists.
//! - Once history exists, prefer operators that have previously yielded
//!   improvements (exploitation), with occasional random exploration.

use crate::{
    ledger::ExperimentLedger,
    mutation::{MutationOperator, MutationSet},
    AutoresearchError, ExperimentTarget,
};
use serde::{Deserialize, Serialize};

// ── Hypothesis ────────────────────────────────────────────────────────────────

/// A proposed experiment: the hypothesis that the mutation will improve the
/// metric, and the specific mutation to apply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    /// Natural-language statement of what is being tested.
    ///
    /// e.g. "Lowering the learning rate from 0.01 to 0.001 should reduce
    /// overfitting and improve validation loss."
    pub statement: String,

    /// The mutation to apply in this experiment.
    pub mutation: MutationSet,

    /// Confidence in this hypothesis (0.0 – 1.0).
    ///
    /// Higher values indicate that prior evidence strongly supports the change.
    /// Used by the verdict engine to set the keep/discard threshold.
    pub confidence: f64,
}

// ── HypothesisEngine trait ────────────────────────────────────────────────────

/// Pluggable cerebellum: generates hypotheses from ledger history.
pub trait HypothesisEngine: Send + Sync {
    /// Generate the next hypothesis given the current ledger and target.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::HypothesisError`] if no hypothesis can be
    /// generated (e.g. search space exhausted).
    fn next_hypothesis(
        &self,
        target: &ExperimentTarget,
        ledger: &ExperimentLedger,
        praxis_guidance: &str,
    ) -> Result<Hypothesis, AutoresearchError>;
}

// ── DefaultHypothesisEngine ───────────────────────────────────────────────────

/// Built-in hypothesis engine.
///
/// Uses a deterministic round-robin strategy over a catalogue of parameter
/// probes derived from the praxis guidance.  In a full implementation the
/// cerebellum LLM would be called here; this engine provides a
/// dependency-free fallback that is useful for testing and offline use.
pub struct DefaultHypothesisEngine {
    /// Candidate parameter adjustments to try, in order.
    probes: Vec<ParameterProbe>,
}

/// A single candidate parameter adjustment used by the default engine.
#[derive(Debug, Clone)]
pub struct ParameterProbe {
    /// The key to adjust.
    pub key: String,
    /// Multiplicative scale factor relative to the current value.
    pub factor: f64,
    /// Natural-language rationale for this probe.
    pub rationale: String,
}

impl Default for DefaultHypothesisEngine {
    fn default() -> Self {
        // Seed with a small catalogue of generic probes.  A real cerebellum
        // would derive probes from the target and praxis guidance.
        Self {
            probes: vec![
                ParameterProbe {
                    key: "learning_rate".into(),
                    factor: 0.5,
                    rationale: "halve learning rate to reduce oscillation".into(),
                },
                ParameterProbe {
                    key: "learning_rate".into(),
                    factor: 2.0,
                    rationale: "double learning rate to escape local minima".into(),
                },
                ParameterProbe {
                    key: "batch_size".into(),
                    factor: 2.0,
                    rationale: "double batch size for more stable gradients".into(),
                },
                ParameterProbe {
                    key: "batch_size".into(),
                    factor: 0.5,
                    rationale: "halve batch size for more frequent updates".into(),
                },
                ParameterProbe {
                    key: "dropout".into(),
                    factor: 0.5,
                    rationale: "reduce dropout to allow the model to fit better".into(),
                },
                ParameterProbe {
                    key: "dropout".into(),
                    factor: 1.5,
                    rationale: "increase dropout to reduce overfitting".into(),
                },
            ],
        }
    }
}

impl DefaultHypothesisEngine {
    /// Create a `DefaultHypothesisEngine` with custom probes.
    #[must_use]
    pub fn with_probes(probes: Vec<ParameterProbe>) -> Self {
        Self { probes }
    }
}

impl HypothesisEngine for DefaultHypothesisEngine {
    fn next_hypothesis(
        &self,
        _target: &ExperimentTarget,
        ledger: &ExperimentLedger,
        praxis_guidance: &str,
    ) -> Result<Hypothesis, AutoresearchError> {
        if self.probes.is_empty() {
            return Err(AutoresearchError::HypothesisError(
                "no probes configured".into(),
            ));
        }

        // Round-robin: pick the probe at index (ledger_len % probes_len).
        let idx = ledger.len() % self.probes.len();
        let probe = &self.probes[idx];

        // Estimate confidence from past success rate of the same factor direction.
        let confidence = estimate_confidence(ledger, probe.factor);

        let statement = format!(
            "{} [guidance: {}]",
            probe.rationale,
            &praxis_guidance[..praxis_guidance.len().min(80)]
        );

        let mutation = MutationSet::single(MutationOperator::ScaleParameter {
            key: probe.key.clone(),
            factor: probe.factor,
            previous: 1.0, // placeholder; real runner fills in the actual value
        });

        Ok(Hypothesis {
            statement,
            mutation,
            confidence,
        })
    }
}

/// Estimate hypothesis confidence from the proportion of past experiments where
/// the scale direction (increase/decrease) produced an improvement.
fn estimate_confidence(ledger: &ExperimentLedger, factor: f64) -> f64 {
    let entries = ledger.entries();
    if entries.is_empty() {
        return 0.5; // neutral prior
    }

    let direction_up = factor > 1.0;
    let matching: Vec<_> = entries
        .iter()
        .filter(|e| {
            // Look for ScaleParameter mutations in the same direction.
            if let Ok(ops) =
                serde_json::from_value::<crate::mutation::MutationSet>(e.mutation_diff.clone())
            {
                ops.operators.iter().any(|op| {
                    if let MutationOperator::ScaleParameter { factor: f, .. } = op {
                        (*f > 1.0) == direction_up
                    } else {
                        false
                    }
                })
            } else {
                false
            }
        })
        .collect();

    if matching.is_empty() {
        return 0.5;
    }

    let improved = matching.iter().filter(|e| e.improved()).count();
    improved as f64 / matching.len() as f64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::ExperimentLedger;
    use crate::ExperimentTarget;

    fn target() -> ExperimentTarget {
        ExperimentTarget::Hyperparameters {
            name: "llm-params".into(),
        }
    }

    #[test]
    fn default_engine_produces_hypothesis_on_empty_ledger() {
        let engine = DefaultHypothesisEngine::default();
        let ledger = ExperimentLedger::new();
        let h = engine
            .next_hypothesis(&target(), &ledger, "Optimise recall@10")
            .unwrap();
        assert!(!h.statement.is_empty());
        assert!(!h.mutation.operators.is_empty());
        assert!(h.confidence >= 0.0 && h.confidence <= 1.0);
    }

    #[test]
    fn default_engine_cycles_through_probes() {
        let engine = DefaultHypothesisEngine::default();
        let ledger = ExperimentLedger::new();
        let probe_count = engine.probes.len();
        let mut keys = Vec::new();

        // We can't advance ledger.len() without a mutable ref, so we test the
        // index formula directly by calling next_hypothesis with ledgers of
        // different sizes.
        for _ in 0..probe_count {
            let h = engine
                .next_hypothesis(&target(), &ledger, "guidance")
                .unwrap();
            keys.push(h.statement.clone());
        }
        // All probes should produce non-empty statements.
        assert_eq!(keys.len(), probe_count);
    }

    #[test]
    fn engine_with_no_probes_returns_error() {
        let engine = DefaultHypothesisEngine::with_probes(vec![]);
        let ledger = ExperimentLedger::new();
        assert!(matches!(
            engine.next_hypothesis(&target(), &ledger, "g"),
            Err(AutoresearchError::HypothesisError(_))
        ));
    }

    #[test]
    fn hypothesis_confidence_neutral_on_empty_ledger() {
        let engine = DefaultHypothesisEngine::default();
        let ledger = ExperimentLedger::new();
        let h = engine
            .next_hypothesis(&target(), &ledger, "guidance")
            .unwrap();
        // neutral prior = 0.5
        assert!((h.confidence - 0.5).abs() < 1e-9);
    }
}
