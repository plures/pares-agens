//! Benchmarking — compare ensemble vs single-model performance.
//!
//! [`EnsembleBenchmark`] runs a set of evaluation queries through both the
//! ensemble routing path and a single baseline model, then produces a
//! [`BenchmarkReport`] with side-by-side metrics.
//!
//! Because actual inference is delegated to the runtime, this module works
//! with a pluggable [`EvalScorer`] trait so callers can inject arbitrary
//! scoring logic without coupling to a particular model backend.

use serde::{Deserialize, Serialize};

use crate::EnsembleError;

// ── EvalQuery ─────────────────────────────────────────────────────────────────

/// A single evaluation query with a known reference answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalQuery {
    /// Unique identifier for this query.
    pub id: String,
    /// The input prompt.
    pub prompt: String,
    /// Optional reference answer used by the scorer.
    pub reference: Option<String>,
}

impl EvalQuery {
    /// Create a new evaluation query.
    #[must_use]
    pub fn new(id: &str, prompt: &str, reference: Option<&str>) -> Self {
        Self {
            id: id.to_string(),
            prompt: prompt.to_string(),
            reference: reference.map(str::to_string),
        }
    }
}

// ── ModelScore ────────────────────────────────────────────────────────────────

/// Per-query score pair for the ensemble and the baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelScore {
    /// Query identifier.
    pub query_id: String,
    /// Score produced by the ensemble (0.0 – 1.0).
    pub ensemble_score: f32,
    /// Score produced by the single baseline model (0.0 – 1.0).
    pub baseline_score: f32,
    /// Latency of the ensemble response in milliseconds.
    pub ensemble_latency_ms: f64,
    /// Latency of the baseline response in milliseconds.
    pub baseline_latency_ms: f64,
}

// ── BenchmarkReport ───────────────────────────────────────────────────────────

/// Aggregate benchmark report comparing ensemble vs baseline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// Number of evaluation queries run.
    pub num_queries: usize,
    /// Mean accuracy of the ensemble across all queries.
    pub ensemble_mean_accuracy: f32,
    /// Mean accuracy of the baseline model across all queries.
    pub baseline_mean_accuracy: f32,
    /// Mean latency of the ensemble in milliseconds.
    pub ensemble_mean_latency_ms: f64,
    /// Mean latency of the baseline in milliseconds.
    pub baseline_mean_latency_ms: f64,
    /// Per-query breakdown.
    pub scores: Vec<ModelScore>,
}

impl BenchmarkReport {
    /// Return `true` when the ensemble outperforms the baseline on accuracy.
    #[must_use]
    pub fn ensemble_wins_accuracy(&self) -> bool {
        self.ensemble_mean_accuracy > self.baseline_mean_accuracy
    }

    /// Return `true` when the ensemble has lower latency than the baseline.
    #[must_use]
    pub fn ensemble_wins_latency(&self) -> bool {
        self.ensemble_mean_latency_ms < self.baseline_mean_latency_ms
    }
}

// ── EvalScorer trait ──────────────────────────────────────────────────────────

/// Pluggable scoring function.
///
/// Implement this trait to inject custom evaluation logic (e.g. exact-match,
/// ROUGE, LLM-as-judge) into the benchmark harness.
pub trait EvalScorer: Send + Sync {
    /// Score a single response against the reference.
    ///
    /// Returns a value in `[0.0, 1.0]`.  If no reference is available,
    /// implementors should return a plausible default (e.g. 0.5).
    fn score(&self, response: &str, query: &EvalQuery) -> f32;

    /// Return the simulated latency in milliseconds for the given response.
    ///
    /// Implementors may return a fixed value, pull from real timing data, or
    /// derive latency from response length.
    fn latency_ms(&self, response: &str) -> f64;
}

// ── EnsembleBenchmark ─────────────────────────────────────────────────────────

/// Compares ensemble routing against a single baseline model on a set of
/// evaluation queries.
///
/// # Example
/// ```
/// use pares_agens_ensemble::benchmark::{
///     EnsembleBenchmark, EvalQuery, EvalScorer,
/// };
///
/// struct AlwaysCorrect;
/// impl EvalScorer for AlwaysCorrect {
///     fn score(&self, _: &str, _: &EvalQuery) -> f32 { 1.0 }
///     fn latency_ms(&self, _: &str) -> f64 { 10.0 }
/// }
///
/// let harness = EnsembleBenchmark::new();
/// let queries = vec![EvalQuery::new("q1", "What is 2+2?", Some("4"))];
/// let report = harness.run(
///     queries,
///     |q| "4".to_string(),      // ensemble inference stub
///     |q| "4".to_string(),      // baseline inference stub
///     &AlwaysCorrect,
/// ).unwrap();
/// assert_eq!(report.num_queries, 1);
/// ```
#[derive(Debug, Default)]
pub struct EnsembleBenchmark;

impl EnsembleBenchmark {
    /// Create a new benchmark harness.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Run the benchmark.
    ///
    /// `ensemble_fn` and `baseline_fn` are called once per query to produce
    /// responses.  The `scorer` evaluates each response.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::ExpertResponseError`] if `queries` is empty.
    pub fn run<EF, BF>(
        &self,
        queries: Vec<EvalQuery>,
        mut ensemble_fn: EF,
        mut baseline_fn: BF,
        scorer: &dyn EvalScorer,
    ) -> Result<BenchmarkReport, EnsembleError>
    where
        EF: FnMut(&EvalQuery) -> String,
        BF: FnMut(&EvalQuery) -> String,
    {
        if queries.is_empty() {
            return Err(EnsembleError::ExpertResponseError(
                "benchmark requires at least one query".into(),
            ));
        }

        let mut scores = Vec::with_capacity(queries.len());

        for query in &queries {
            let ensemble_resp = ensemble_fn(query);
            let baseline_resp = baseline_fn(query);

            scores.push(ModelScore {
                query_id: query.id.clone(),
                ensemble_score: scorer.score(&ensemble_resp, query),
                baseline_score: scorer.score(&baseline_resp, query),
                ensemble_latency_ms: scorer.latency_ms(&ensemble_resp),
                baseline_latency_ms: scorer.latency_ms(&baseline_resp),
            });
        }

        let n = scores.len() as f64;
        let ensemble_mean_accuracy =
            scores.iter().map(|s| s.ensemble_score).sum::<f32>() / scores.len() as f32;
        let baseline_mean_accuracy =
            scores.iter().map(|s| s.baseline_score).sum::<f32>() / scores.len() as f32;
        let ensemble_mean_latency_ms =
            scores.iter().map(|s| s.ensemble_latency_ms).sum::<f64>() / n;
        let baseline_mean_latency_ms =
            scores.iter().map(|s| s.baseline_latency_ms).sum::<f64>() / n;

        Ok(BenchmarkReport {
            num_queries: scores.len(),
            ensemble_mean_accuracy,
            baseline_mean_accuracy,
            ensemble_mean_latency_ms,
            baseline_mean_latency_ms,
            scores,
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeScorer {
        ensemble_correct: bool,
        baseline_correct: bool,
        latency: f64,
    }

    impl EvalScorer for FakeScorer {
        fn score(&self, response: &str, _query: &EvalQuery) -> f32 {
            if response == "ensemble" {
                if self.ensemble_correct { 1.0 } else { 0.0 }
            } else if self.baseline_correct {
                1.0
            } else {
                0.0
            }
        }

        fn latency_ms(&self, _: &str) -> f64 {
            self.latency
        }
    }

    fn queries(n: usize) -> Vec<EvalQuery> {
        (0..n)
            .map(|i| EvalQuery::new(&format!("q{i}"), "prompt", Some("answer")))
            .collect()
    }

    #[test]
    fn run_rejects_empty_query_list() {
        let h = EnsembleBenchmark::new();
        let scorer = FakeScorer {
            ensemble_correct: true,
            baseline_correct: false,
            latency: 10.0,
        };
        assert!(h
            .run(vec![], |_| "ensemble".into(), |_| "baseline".into(), &scorer)
            .is_err());
    }

    #[test]
    fn run_returns_correct_num_queries() {
        let h = EnsembleBenchmark::new();
        let scorer = FakeScorer {
            ensemble_correct: true,
            baseline_correct: true,
            latency: 20.0,
        };
        let report = h
            .run(queries(3), |_| "ensemble".into(), |_| "baseline".into(), &scorer)
            .unwrap();
        assert_eq!(report.num_queries, 3);
    }

    #[test]
    fn ensemble_wins_accuracy_when_ensemble_correct_and_baseline_wrong() {
        let h = EnsembleBenchmark::new();
        let scorer = FakeScorer {
            ensemble_correct: true,
            baseline_correct: false,
            latency: 10.0,
        };
        let report = h
            .run(queries(5), |_| "ensemble".into(), |_| "other".into(), &scorer)
            .unwrap();
        assert!(report.ensemble_wins_accuracy());
    }

    #[test]
    fn ensemble_wins_latency_when_faster() {
        let h = EnsembleBenchmark::new();
        struct VariableLatency;
        impl EvalScorer for VariableLatency {
            fn score(&self, _: &str, _: &EvalQuery) -> f32 { 0.5 }
            fn latency_ms(&self, response: &str) -> f64 {
                if response == "fast" { 5.0 } else { 50.0 }
            }
        }
        let report = h
            .run(queries(2), |_| "fast".into(), |_| "slow".into(), &VariableLatency)
            .unwrap();
        assert!(report.ensemble_wins_latency());
    }

    #[test]
    fn eval_query_roundtrips_json() {
        let q = EvalQuery::new("q1", "Hello?", Some("Hi!"));
        let json = serde_json::to_string(&q).unwrap();
        let back: EvalQuery = serde_json::from_str(&json).unwrap();
        assert_eq!(q.id, back.id);
        assert_eq!(q.reference, back.reference);
    }
}
