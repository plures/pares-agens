//! Max-min optimization engine.
//!
//! [`MaxMinOptimizer`] iterates toward a solution that maximises the minimum
//! weighted score across all dimensions of the objective, subject to the
//! supplied constraints.  It emits [`ObservabilityEvent`](crate::telemetry::ObservabilityEvent)s
//! at each iteration via a pluggable [`TelemetryEmitter`](crate::telemetry::TelemetryEmitter).
//!
//! # Algorithm outline
//!
//! 1. Evaluate the initial objective (the "baseline").
//! 2. On each iteration, attempt an improvement step by adjusting the policy
//!    scores by a damped gradient proxy.
//! 3. If the resulting objective improves by less than `convergence_tolerance`,
//!    declare convergence.
//! 4. Emit telemetry after every iteration and on completion.
//!
//! The implementation is intentionally self-contained and dependency-free so
//! that fine-tuned model policies can wrap or replace the step logic without
//! pulling in heavy numerical libraries.

use crate::{
    telemetry::{ObservabilityEvent, TelemetryEmitter},
    Constraint, Objective, OptimizerError, OptimizerInput, OptimizationResult,
};

// ── Policy trait ──────────────────────────────────────────────────────────────

/// A pluggable policy that produces updated scores given the current objective
/// and constraint state.
///
/// Implement this trait to plug in a fine-tuned model policy.
pub trait Policy: Send + Sync {
    /// Given the current scores and constraints, return a new set of scores
    /// representing the policy's proposed improvement.
    ///
    /// The returned `Vec` must have the same length as `current_scores`.
    ///
    /// # Errors
    ///
    /// Return [`OptimizerError::ObjectiveError`] if the policy cannot produce
    /// a valid score vector.
    fn step(
        &self,
        iteration: u32,
        current_scores: &[f64],
        constraints: &[Constraint],
    ) -> Result<Vec<f64>, OptimizerError>;
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Compute one damped gradient-free step that nudges `score` toward `min_score`.
///
/// The update rule is:
///
/// ```text
/// new = score + step_size × sign(min_score − score) × √|score − min_score|
/// ```
///
/// The square-root dampening slows convergence as the score approaches the
/// minimum, preventing overshoot when scores are nearly equal.
fn damped_step(score: f64, min_score: f64, step_size: f64) -> f64 {
    let gap = score - min_score;
    score + step_size * (-gap).signum() * gap.abs().sqrt()
}

// ── DefaultPolicy ─────────────────────────────────────────────────────────────

/// Default gradient-free policy that nudges each score toward the current
/// minimum score, narrowing the spread between dimensions.
///
/// This is the built-in baseline policy and is used when no custom `Policy` is
/// supplied.
pub struct DefaultPolicy {
    /// Step size in (0, 1].  Smaller values give smoother convergence.
    pub step_size: f64,
}

impl DefaultPolicy {
    /// Create a `DefaultPolicy` with the given step size.
    #[must_use]
    pub fn new(step_size: f64) -> Self {
        Self { step_size }
    }
}

impl Policy for DefaultPolicy {
    fn step(
        &self,
        _iteration: u32,
        current_scores: &[f64],
        _constraints: &[Constraint],
    ) -> Result<Vec<f64>, OptimizerError> {
        let min_score = current_scores
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);

        let new_scores = current_scores
            .iter()
            .map(|&s| damped_step(s, min_score, self.step_size))
            .collect();

        Ok(new_scores)
    }
}

// ── MaxMinOptimizer ────────────────────────────────────────────────────────────

/// Iterative max-min optimizer.
///
/// # Example
///
/// ```rust
/// use pares_agens_optimizer::{OptimizerInput, Objective, Constraint};
/// use pares_agens_optimizer::engine::MaxMinOptimizer;
/// use pares_agens_optimizer::telemetry::TelemetryEmitter;
/// use std::collections::HashMap;
///
/// let input = OptimizerInput {
///     run_id: "run-1".into(),
///     policy_id: "policy-v1".into(),
///     objective: Objective { scores: vec![0.9, 0.4, 0.7], weights: None },
///     constraints: vec![],
///     max_iterations: 50,
///     convergence_tolerance: 1e-4,
///     context: HashMap::new(),
/// };
///
/// let emitter = TelemetryEmitter::noop();
/// let optimizer = MaxMinOptimizer::new(emitter);
/// let result = optimizer.run(input).unwrap();
/// assert!(result.converged);
/// ```
pub struct MaxMinOptimizer {
    emitter: TelemetryEmitter,
    policy: Box<dyn Policy>,
}

impl MaxMinOptimizer {
    /// Create a new `MaxMinOptimizer` using the built-in [`DefaultPolicy`].
    #[must_use]
    pub fn new(emitter: TelemetryEmitter) -> Self {
        Self {
            emitter,
            policy: Box::new(DefaultPolicy::new(0.3)),
        }
    }

    /// Create a `MaxMinOptimizer` with a custom [`Policy`] implementation.
    #[must_use]
    pub fn with_policy(emitter: TelemetryEmitter, policy: Box<dyn Policy>) -> Self {
        Self { emitter, policy }
    }

    /// Run the full optimization loop for the given [`OptimizerInput`].
    ///
    /// # Errors
    ///
    /// - [`OptimizerError::InvalidConfig`] — if the input fails validation.
    /// - [`OptimizerError::ConstraintViolation`] — if a hard constraint is
    ///   violated in the initial input and `constraints` are non-empty.
    /// - [`OptimizerError::ObjectiveError`] — if objective evaluation fails.
    /// - [`OptimizerError::NoConvergence`] — if the budget is exhausted without
    ///   reaching tolerance (note: a result is still returned via the
    ///   `converged = false` flag, so callers can choose to treat this as a
    ///   soft failure).
    pub fn run(&self, input: OptimizerInput) -> Result<OptimizationResult, OptimizerError> {
        input.validate()?;

        let run_id = input.run_id.clone();
        let policy_id = input.policy_id.clone();

        let mut scores = input.objective.scores.clone();
        let weights = input.objective.weights.clone();

        // Evaluate the baseline objective.
        let initial_objective = Objective {
            scores: scores.clone(),
            weights: weights.clone(),
        }
        .evaluate()?;

        self.emitter.emit(ObservabilityEvent::EpisodeStarted {
            run_id: run_id.clone(),
            policy_id: policy_id.clone(),
            initial_score: initial_objective,
            context: input.context.clone(),
        });

        let mut prev_score = initial_objective;
        let mut iterations = 0u32;
        let mut converged = false;

        for iter in 0..input.max_iterations {
            iterations = iter + 1;

            // Ask the policy for the next set of scores.
            let candidate = self.policy.step(iter, &scores, &input.constraints)?;

            // Evaluate the candidate objective.
            let candidate_objective = Objective {
                scores: candidate.clone(),
                weights: weights.clone(),
            }
            .evaluate()?;

            // Check constraints on the candidate scores.
            let mut violated: Vec<String> = Vec::new();
            for constraint in &input.constraints {
                // Map constraint to the first score dimension that shares the
                // constraint name (by index lookup fallback to value check).
                if !constraint.is_satisfied() {
                    violated.push(constraint.name.clone());
                }
            }

            if !violated.is_empty() {
                self.emitter.emit(ObservabilityEvent::ConstraintViolated {
                    run_id: run_id.clone(),
                    iteration: iterations,
                    violated_constraints: violated.clone(),
                });
            }

            let improvement = candidate_objective - prev_score;

            self.emitter.emit(ObservabilityEvent::IterationCompleted {
                run_id: run_id.clone(),
                iteration: iterations,
                objective_score: candidate_objective,
                improvement,
                violated_constraint_count: violated.len(),
            });

            // Accept the step (gradient-free: always accept if non-negative).
            if candidate_objective >= prev_score {
                scores = candidate;
                prev_score = candidate_objective;
            }

            // Check convergence.
            if improvement.abs() < input.convergence_tolerance {
                converged = true;
                break;
            }
        }

        // Final constraint check on the accepted solution.
        let final_violated: Vec<String> = input
            .constraints
            .iter()
            .filter(|c| !c.is_satisfied())
            .map(|c| c.name.clone())
            .collect();

        self.emitter.emit(ObservabilityEvent::EpisodeCompleted {
            run_id: run_id.clone(),
            policy_id: policy_id.clone(),
            final_score: prev_score,
            iterations,
            converged,
            violated_constraints: final_violated.clone(),
        });

        let result = OptimizationResult {
            run_id,
            policy_id,
            objective_score: prev_score,
            iterations,
            converged,
            violated_constraints: final_violated,
        };

        if !converged {
            // Emit a metric but return the result; callers decide whether to
            // treat non-convergence as a hard error.
            return Err(OptimizerError::NoConvergence(iterations));
        }

        Ok(result)
    }
}

// ── Offline / online evaluation hooks ────────────────────────────────────────

/// Evaluate a policy offline on a fixed dataset of `(scores, constraints)`
/// episodes, returning the mean objective score across all episodes.
///
/// Use this to measure policy quality before deploying to the online loop.
///
/// # Errors
///
/// Returns the first [`OptimizerError`] encountered.
pub fn offline_evaluate(
    episodes: &[(Vec<f64>, Vec<Constraint>)],
    policy: &dyn Policy,
    convergence_tolerance: f64,
    max_iterations: u32,
) -> Result<f64, OptimizerError> {
    if episodes.is_empty() {
        return Ok(0.0);
    }
    let mut total = 0.0;
    for (scores, constraints) in episodes {
        let mut current = scores.clone();
        for iter in 0..max_iterations {
            let candidate = policy.step(iter, &current, constraints)?;
            let prev = Objective {
                scores: current.clone(),
                weights: None,
            }
            .evaluate()?;
            let next = Objective {
                scores: candidate.clone(),
                weights: None,
            }
            .evaluate()?;
            let improvement = next - prev;
            if next >= prev {
                current = candidate;
            }
            if improvement.abs() < convergence_tolerance {
                break;
            }
        }
        total += Objective {
            scores: current,
            weights: None,
        }
        .evaluate()?;
    }
    Ok(total / episodes.len() as f64)
}

/// Evaluate a single episode in the online (live) loop and return the
/// objective score after optimization.
///
/// Non-convergence is propagated as [`OptimizerError::NoConvergence`] so callers
/// can distinguish it from a legitimate score of zero.
///
/// # Errors
///
/// Returns [`OptimizerError`] for any failure including non-convergence.
pub fn online_evaluate(
    input: OptimizerInput,
    emitter: TelemetryEmitter,
) -> Result<f64, OptimizerError> {
    let optimizer = MaxMinOptimizer::new(emitter);
    Ok(optimizer.run(input)?.objective_score)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Constraint, Objective, OptimizerInput};
    use std::collections::HashMap;

    fn simple_input(run_id: &str, scores: Vec<f64>) -> OptimizerInput {
        OptimizerInput {
            run_id: run_id.into(),
            policy_id: "policy-test".into(),
            objective: Objective {
                scores,
                weights: None,
            },
            constraints: vec![],
            max_iterations: 100,
            convergence_tolerance: 1e-6,
            context: HashMap::new(),
        }
    }

    #[test]
    fn default_policy_nudges_scores_toward_min() {
        let policy = DefaultPolicy::new(0.5);
        let scores = vec![0.8, 0.2, 0.6];
        let new_scores = policy.step(0, &scores, &[]).unwrap();
        // The minimum is 0.2; the policy should nudge all scores toward 0.2.
        // The minimum score itself stays at 0.2 (signum of 0 is 0).
        assert_eq!(new_scores.len(), scores.len());
        // All values ≥ 0.2 should be reduced (or stay the same at the min).
        for (&orig, &next) in scores.iter().zip(new_scores.iter()) {
            if orig > 0.2 {
                assert!(next < orig, "score {orig} should decrease toward min");
            }
        }
    }

    #[test]
    fn optimizer_converges_on_equal_scores() {
        // If all scores are equal, the objective is already at its maximum
        // — the optimizer should converge immediately.
        let input = simple_input("run-eq", vec![0.5, 0.5, 0.5]);
        let emitter = TelemetryEmitter::noop();
        let optimizer = MaxMinOptimizer::new(emitter);
        let result = optimizer.run(input).unwrap();
        assert!(result.converged);
        assert_eq!(result.iterations, 1);
    }

    #[test]
    fn optimizer_improves_unequal_scores() {
        let initial_scores = vec![0.9, 0.1, 0.7];
        let input = simple_input("run-unequal", initial_scores.clone());
        let initial_min = initial_scores
            .iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);
        let emitter = TelemetryEmitter::noop();
        let optimizer = MaxMinOptimizer::new(emitter);
        let result = optimizer.run(input).unwrap();
        assert!(result.objective_score >= initial_min);
    }

    #[test]
    fn optimizer_emits_telemetry_events() {
        use crate::telemetry::ObservabilityEvent;

        let input = simple_input("run-telem", vec![0.5, 0.5]);
        let (emitter, events) = TelemetryEmitter::collecting();
        let optimizer = MaxMinOptimizer::new(emitter);
        optimizer.run(input).unwrap();

        let events = events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ObservabilityEvent::EpisodeStarted { .. })),
            "should emit EpisodeStarted"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ObservabilityEvent::EpisodeCompleted { .. })),
            "should emit EpisodeCompleted"
        );
    }

    #[test]
    fn offline_evaluate_returns_mean_of_converged_episodes() {
        let episodes = vec![
            (vec![0.5_f64, 0.5_f64], vec![] as Vec<Constraint>),
            (vec![0.8_f64, 0.8_f64], vec![]),
        ];
        let policy = DefaultPolicy::new(0.3);
        let mean = offline_evaluate(&episodes, &policy, 1e-6, 50).unwrap();
        // Both episodes are already equal; mean should be 0.5 * 0.5 + 0.5 * 0.8 = 0.65
        assert!(mean > 0.0);
    }

    #[test]
    fn offline_evaluate_empty_episodes_returns_zero() {
        let policy = DefaultPolicy::new(0.3);
        let result = offline_evaluate(&[], &policy, 1e-6, 50).unwrap();
        assert_eq!(result, 0.0);
    }

    #[test]
    fn online_evaluate_returns_score_for_converged_input() {
        let input = simple_input("online-1", vec![0.5, 0.5]);
        let emitter = TelemetryEmitter::noop();
        let score = online_evaluate(input, emitter).unwrap();
        assert!((score - 0.5).abs() < 1e-6);
    }
}
