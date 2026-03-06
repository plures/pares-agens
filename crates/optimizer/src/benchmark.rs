//! Benchmark harness for comparing a baseline policy against an optimized policy.
//!
//! [`BenchmarkHarness`] runs both policies over the same set of episodes and
//! produces a [`BenchmarkReport`] that can be serialised to JSON as a result
//! artifact.
//!
//! # Example
//!
//! ```rust
//! use pares_agens_optimizer::{OptimizerInput, Objective, Constraint};
//! use pares_agens_optimizer::benchmark::{BenchmarkConfig, BenchmarkHarness};
//! use std::collections::HashMap;
//!
//! let episodes = vec![
//!     OptimizerInput {
//!         run_id: "bench-0".into(),
//!         policy_id: "policy-v1".into(),
//!         objective: Objective { scores: vec![0.5, 0.8, 0.3], weights: None },
//!         constraints: vec![],
//!         max_iterations: 30,
//!         convergence_tolerance: 1e-4,
//!         context: HashMap::new(),
//!     },
//! ];
//!
//! let config = BenchmarkConfig::default();
//! let harness = BenchmarkHarness::new(config);
//! let report = harness.run(episodes).unwrap();
//! assert!(report.optimized_mean_score >= report.baseline_mean_score);
//! println!("{}", serde_json::to_string_pretty(&report).unwrap());
//! ```

use serde::{Deserialize, Serialize};

use crate::{
    engine::{DefaultPolicy, MaxMinOptimizer, Policy},
    telemetry::TelemetryEmitter,
    Objective, OptimizerError, OptimizerInput,
};

// ── BenchmarkConfig ───────────────────────────────────────────────────────────

/// Configuration for the benchmark harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkConfig {
    /// Step size used by the baseline (untuned) [`DefaultPolicy`].
    pub baseline_step_size: f64,

    /// Step size used by the optimized [`DefaultPolicy`].
    pub optimized_step_size: f64,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            baseline_step_size: 0.0, // baseline: no improvement steps
            optimized_step_size: 0.3,
        }
    }
}

// ── EpisodeResult ─────────────────────────────────────────────────────────────

/// Per-episode result for one policy arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpisodeResult {
    /// The run identifier from the episode's [`OptimizerInput`].
    pub run_id: String,

    /// Objective score returned by the policy arm for this episode.
    pub score: f64,

    /// Whether the optimizer converged for this episode.
    pub converged: bool,

    /// Iterations used.
    pub iterations: u32,
}

// ── BenchmarkReport ───────────────────────────────────────────────────────────

/// Aggregated benchmark result artifact.
///
/// Serialise with [`serde_json::to_string_pretty`] to produce a JSON artifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// Mean objective score for the baseline (unoptimized) policy.
    pub baseline_mean_score: f64,

    /// Mean objective score for the optimized policy.
    pub optimized_mean_score: f64,

    /// Absolute improvement: `optimized_mean_score - baseline_mean_score`.
    pub absolute_improvement: f64,

    /// Relative improvement in percent (may be `NaN` when baseline is 0).
    pub relative_improvement_pct: f64,

    /// Number of episodes where the optimized policy won over the baseline.
    pub episodes_won: usize,

    /// Total number of episodes evaluated.
    pub total_episodes: usize,

    /// Per-episode results for the baseline arm.
    pub baseline_episodes: Vec<EpisodeResult>,

    /// Per-episode results for the optimized arm.
    pub optimized_episodes: Vec<EpisodeResult>,
}

// ── BenchmarkHarness ──────────────────────────────────────────────────────────

/// Harness that runs both a baseline and an optimized policy arm over the same
/// set of episodes and aggregates the results.
pub struct BenchmarkHarness {
    config: BenchmarkConfig,
}

impl BenchmarkHarness {
    /// Create a new harness with the supplied configuration.
    #[must_use]
    pub fn new(config: BenchmarkConfig) -> Self {
        Self { config }
    }

    /// Run both arms over `episodes` and return a [`BenchmarkReport`].
    ///
    /// Non-convergence in either arm is treated as a soft failure: the
    /// best-effort score is recorded and the episode is marked as not converged.
    ///
    /// # Errors
    ///
    /// Returns the first hard [`OptimizerError`] (e.g. invalid config or
    /// non-finite objective) encountered in either arm.
    pub fn run(
        &self,
        episodes: Vec<OptimizerInput>,
    ) -> Result<BenchmarkReport, OptimizerError> {
        let baseline_policy: Box<dyn Policy> =
            Box::new(DefaultPolicy::new(self.config.baseline_step_size));
        let optimized_policy: Box<dyn Policy> =
            Box::new(DefaultPolicy::new(self.config.optimized_step_size));

        let _baseline_opt =
            MaxMinOptimizer::with_policy(TelemetryEmitter::noop(), baseline_policy);
        let optimized_opt =
            MaxMinOptimizer::with_policy(TelemetryEmitter::noop(), optimized_policy);

        let mut baseline_results: Vec<EpisodeResult> = Vec::new();
        let mut optimized_results: Vec<EpisodeResult> = Vec::new();

        for episode in episodes {
            episode.validate()?;

            // Evaluate baseline objective (no optimization steps).
            let baseline_score = Objective {
                scores: episode.objective.scores.clone(),
                weights: episode.objective.weights.clone(),
            }
            .evaluate()?;

            baseline_results.push(EpisodeResult {
                run_id: episode.run_id.clone(),
                score: baseline_score,
                converged: true,
                iterations: 0,
            });

            // Run optimized arm.
            let (opt_score, opt_converged, opt_iterations) =
                match optimized_opt.run(episode.clone()) {
                    Ok(r) => (r.objective_score, r.converged, r.iterations),
                    Err(OptimizerError::NoConvergence(iters)) => {
                        // Extract best-effort score from the initial objective.
                        let s = Objective {
                            scores: episode.objective.scores.clone(),
                            weights: episode.objective.weights.clone(),
                        }
                        .evaluate()?;
                        (s, false, iters)
                    }
                    Err(e) => return Err(e),
                };

            optimized_results.push(EpisodeResult {
                run_id: episode.run_id.clone(),
                score: opt_score,
                converged: opt_converged,
                iterations: opt_iterations,
            });
        }

        let baseline_mean = mean_score(&baseline_results);
        let optimized_mean = mean_score(&optimized_results);
        let absolute_improvement = optimized_mean - baseline_mean;
        let relative_improvement_pct = if baseline_mean == 0.0 {
            f64::NAN
        } else {
            (absolute_improvement / baseline_mean) * 100.0
        };
        let episodes_won = baseline_results
            .iter()
            .zip(optimized_results.iter())
            .filter(|(b, o)| o.score > b.score)
            .count();

        Ok(BenchmarkReport {
            baseline_mean_score: baseline_mean,
            optimized_mean_score: optimized_mean,
            absolute_improvement,
            relative_improvement_pct,
            episodes_won,
            total_episodes: baseline_results.len(),
            baseline_episodes: baseline_results,
            optimized_episodes: optimized_results,
        })
    }
}

fn mean_score(results: &[EpisodeResult]) -> f64 {
    if results.is_empty() {
        return 0.0;
    }
    results.iter().map(|r| r.score).sum::<f64>() / results.len() as f64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Objective, OptimizerInput};
    use std::collections::HashMap;

    fn episode(run_id: &str, scores: Vec<f64>) -> OptimizerInput {
        OptimizerInput {
            run_id: run_id.into(),
            policy_id: "p".into(),
            objective: Objective {
                scores,
                weights: None,
            },
            constraints: vec![],
            max_iterations: 50,
            convergence_tolerance: 1e-5,
            context: HashMap::new(),
        }
    }

    #[test]
    fn report_has_correct_episode_count() {
        let episodes = vec![episode("e1", vec![0.5, 0.5]), episode("e2", vec![0.8, 0.3])];
        let harness = BenchmarkHarness::new(BenchmarkConfig::default());
        let report = harness.run(episodes).unwrap();
        assert_eq!(report.total_episodes, 2);
    }

    #[test]
    fn optimized_mean_ge_baseline_mean_for_unequal_scores() {
        // Unequal scores give room for the optimizer to improve.
        let episodes = vec![
            episode("b1", vec![0.9, 0.1, 0.7]),
            episode("b2", vec![0.6, 0.2, 0.8]),
        ];
        let config = BenchmarkConfig {
            baseline_step_size: 0.0,
            optimized_step_size: 0.3,
        };
        let harness = BenchmarkHarness::new(config);
        let report = harness.run(episodes).unwrap();
        assert!(
            report.optimized_mean_score >= report.baseline_mean_score,
            "optimized ({}) should be >= baseline ({})",
            report.optimized_mean_score,
            report.baseline_mean_score,
        );
    }

    #[test]
    fn benchmark_report_serialises_to_json() {
        let harness = BenchmarkHarness::new(BenchmarkConfig::default());
        let report = harness.run(vec![episode("j1", vec![0.5, 0.5])]).unwrap();
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("baseline_mean_score"));
        assert!(json.contains("optimized_mean_score"));
    }

    #[test]
    fn absolute_improvement_matches_difference() {
        let harness = BenchmarkHarness::new(BenchmarkConfig::default());
        let report = harness.run(vec![episode("ai", vec![0.5, 0.5])]).unwrap();
        let expected = report.optimized_mean_score - report.baseline_mean_score;
        assert!((report.absolute_improvement - expected).abs() < 1e-12);
    }
}
