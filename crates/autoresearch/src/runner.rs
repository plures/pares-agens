//! Research runner — orchestrates the full autonomous experiment loop.
//!
//! [`ResearchRunner`] ties together every module:
//!
//! ```text
//! for each experiment:
//!   1. HypothesisEngine::next_hypothesis() → Hypothesis
//!   2. ExecutionSandbox::execute()         → SandboxOutput
//!   3. MetricExtractor::extract()          → f64
//!   4. VerdictEngine::evaluate()           → Verdict
//!   5. ExperimentLedger::append()          → LedgerEntry
//!   6. Check stop conditions
//! ResearchReport::from_ledger()            → ResearchReport
//! ```
//!
//! The runner is intentionally synchronous to remain dependency-free.
//! A real async wrapper would call `runner.run_experiment()` from a tokio task
//! with the inter-experiment delay enforced by `tokio::time::sleep`.

use chrono::Utc;
use uuid::Uuid;

use crate::{
    hypothesis::{DefaultHypothesisEngine, HypothesisEngine},
    ledger::ExperimentLedger,
    measurement::{KeyValueExtractor, MetricExtractor},
    report::ResearchReport,
    sandbox::{DryRunSandbox, ExecutionSandbox},
    schedule::StopCondition,
    verdict::{VerdictEngine, VerdictInput},
    AutoresearchConfig, AutoresearchError, LedgerEntry, Verdict,
};

// ── RunnerState ───────────────────────────────────────────────────────────────

/// Internal state maintained by the runner across experiments.
pub struct RunnerState {
    pub(crate) ledger: ExperimentLedger,
    pub(crate) baseline_metric: f64,
    pub(crate) experiment_count: u32,
    pub(crate) start_time: chrono::DateTime<Utc>,
}

// ── ResearchRunner ────────────────────────────────────────────────────────────

/// Orchestrates the autoresearch loop.
///
/// # Example
///
/// ```rust
/// use pares_agens_autoresearch::{
///     AutoresearchConfig, ExperimentTarget,
///     runner::ResearchRunner,
///     schedule::ResearchSchedule,
/// };
///
/// let config = AutoresearchConfig {
///     id: "test-run".into(),
///     target: ExperimentTarget::Hyperparameters { name: "llm-params".into() },
///     metric: "val_bpb".into(),
///     higher_is_better: false,
///     schedule: ResearchSchedule {
///         max_experiments_total: 3,
///         max_experiments_per_hour: 0,
///         ..Default::default()
///     },
///     praxis_guidance: "Reduce validation BPB below 1.0".into(),
/// };
///
/// let runner = ResearchRunner::new(config);
/// // In a real run the sandbox and metric extractor would be injected;
/// // the defaults (DryRunSandbox + KeyValueExtractor) are used here.
/// let report = runner.run().unwrap();
/// assert_eq!(report.run_id, "test-run");
/// ```
pub struct ResearchRunner {
    config: AutoresearchConfig,
    hypothesis_engine: Box<dyn HypothesisEngine>,
    sandbox: Box<dyn ExecutionSandbox>,
    metric_extractor: Box<dyn MetricExtractor>,
    verdict_engine: VerdictEngine,
}

impl ResearchRunner {
    /// Create a `ResearchRunner` with all default components.
    ///
    /// - Hypothesis engine: [`DefaultHypothesisEngine`]
    /// - Sandbox: [`DryRunSandbox`]
    /// - Metric extractor: [`KeyValueExtractor`]
    /// - Verdict engine: [`VerdictEngine`] with default policy
    #[must_use]
    pub fn new(config: AutoresearchConfig) -> Self {
        Self {
            config,
            hypothesis_engine: Box::new(DefaultHypothesisEngine::default()),
            sandbox: Box::new(DryRunSandbox::default()),
            metric_extractor: Box::new(KeyValueExtractor),
            verdict_engine: VerdictEngine::new(),
        }
    }

    /// Builder: replace the hypothesis engine.
    #[must_use]
    pub fn with_hypothesis_engine(mut self, engine: Box<dyn HypothesisEngine>) -> Self {
        self.hypothesis_engine = engine;
        self
    }

    /// Builder: replace the execution sandbox.
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: Box<dyn ExecutionSandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }

    /// Builder: replace the metric extractor.
    #[must_use]
    pub fn with_metric_extractor(mut self, extractor: Box<dyn MetricExtractor>) -> Self {
        self.metric_extractor = extractor;
        self
    }

    /// Builder: replace the verdict engine.
    #[must_use]
    pub fn with_verdict_engine(mut self, engine: VerdictEngine) -> Self {
        self.verdict_engine = engine;
        self
    }

    /// Run the full autoresearch loop synchronously, returning a [`ResearchReport`].
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::InvalidConfig`] if the configuration fails
    /// validation.  Individual experiment errors (sandbox failures, measurement
    /// failures) are recorded in the ledger as `Verdict::Error` entries and do
    /// not abort the loop.
    pub fn run(&self) -> Result<ResearchReport, AutoresearchError> {
        self.config.validate()?;

        let start_time = Utc::now();
        let target_label = self.config.target.label();

        // Measure the baseline metric (using dry-run output for the initial
        // measurement; a real implementation would run the unmodified target).
        let baseline_output = self
            .sandbox
            .execute(&target_label, &serde_json::Value::Null, self.config.schedule.experiment_timeout_secs)
            .unwrap_or_else(|_| crate::sandbox::SandboxOutput {
                stdout: format!("{}: 0.0", self.config.metric),
                exit_code: Some(0),
                timed_out: false,
                elapsed_secs: 0.0,
            });

        let baseline_metric = self
            .metric_extractor
            .extract(&baseline_output.stdout, &self.config.metric)
            .unwrap_or(0.0);

        let mut state = RunnerState {
            ledger: ExperimentLedger::new(),
            baseline_metric,
            experiment_count: 0,
            start_time,
        };

        let stop_condition = self.experiment_loop(&mut state);

        let report = ResearchReport::from_ledger(
            self.config.id.clone(),
            target_label,
            self.config.metric.clone(),
            self.config.higher_is_better,
            state.baseline_metric,
            state.start_time,
            stop_condition,
            &state.ledger,
        );

        Ok(report)
    }

    /// Run a single experiment and append the result to the ledger.
    ///
    /// Returns the [`Verdict`] so the caller can react immediately (e.g. log).
    ///
    /// This method is public so that async wrappers can drive the loop
    /// externally while injecting inter-experiment delays.
    ///
    /// # Errors
    ///
    /// Returns `Err` only for unrecoverable failures (e.g. the hypothesis
    /// engine is exhausted).  Sandbox/measurement failures produce
    /// `Verdict::Error` entries in the ledger.
    pub fn run_experiment(&self, state: &mut RunnerState) -> Result<Verdict, AutoresearchError> {
        state.experiment_count += 1;
        let exp_num = state.experiment_count;

        let started_at = Utc::now();

        // 1. Hypothesise.
        let hypothesis = self.hypothesis_engine.next_hypothesis(
            &self.config.target,
            &state.ledger,
            &self.config.praxis_guidance,
        )?;

        // 2. Serialise the mutation diff for the ledger.
        let mutation_diff = hypothesis.mutation.to_diff().unwrap_or(serde_json::Value::Null);
        let mutation_description = hypothesis.mutation.description();

        // 3. Execute in sandbox.
        let sandbox_result = self.sandbox.execute(
            &self.config.target.label(),
            &mutation_diff,
            self.config.schedule.experiment_timeout_secs,
        );

        let completed_at = Utc::now();
        let elapsed = (completed_at - started_at).num_milliseconds() as f64 / 1000.0;

        // Handle sandbox errors.
        let output = match sandbox_result {
            Err(e) => {
                let entry = LedgerEntry {
                    id: Uuid::new_v4().to_string(),
                    run_id: self.config.id.clone(),
                    experiment_number: exp_num,
                    hypothesis: hypothesis.statement.clone(),
                    mutation_description,
                    mutation_diff,
                    metric_before: state.ledger.best_metric().unwrap_or(state.baseline_metric),
                    metric_after: None,
                    higher_is_better: self.config.higher_is_better,
                    verdict: Verdict::Error,
                    verdict_reason: format!("sandbox error: {e}"),
                    duration_secs: elapsed,
                    started_at,
                    completed_at,
                };
                state.ledger.append(entry);
                return Ok(Verdict::Error);
            }
            Ok(o) => o,
        };

        // Handle timeouts.
        if output.timed_out {
            let entry = LedgerEntry {
                id: Uuid::new_v4().to_string(),
                run_id: self.config.id.clone(),
                experiment_number: exp_num,
                hypothesis: hypothesis.statement.clone(),
                mutation_description,
                mutation_diff,
                metric_before: state.ledger.best_metric().unwrap_or(state.baseline_metric),
                metric_after: None,
                higher_is_better: self.config.higher_is_better,
                verdict: Verdict::Error,
                verdict_reason: format!(
                    "experiment timed out after {:.1}s",
                    self.config.schedule.experiment_timeout_secs
                ),
                duration_secs: elapsed,
                started_at,
                completed_at,
            };
            state.ledger.append(entry);
            return Ok(Verdict::Error);
        }

        // 4. Extract metric.
        let metric_before = state.ledger.best_metric().unwrap_or(state.baseline_metric);

        let metric_after = match self
            .metric_extractor
            .extract(&output.stdout, &self.config.metric)
        {
            Ok(v) => v,
            Err(e) => {
                let entry = LedgerEntry {
                    id: Uuid::new_v4().to_string(),
                    run_id: self.config.id.clone(),
                    experiment_number: exp_num,
                    hypothesis: hypothesis.statement.clone(),
                    mutation_description,
                    mutation_diff,
                    metric_before,
                    metric_after: None,
                    higher_is_better: self.config.higher_is_better,
                    verdict: Verdict::Error,
                    verdict_reason: format!("measurement error: {e}"),
                    duration_secs: elapsed,
                    started_at,
                    completed_at,
                };
                state.ledger.append(entry);
                return Ok(Verdict::Error);
            }
        };

        // 5. Evaluate verdict.
        let measurement = crate::measurement::Measurement {
            metric_name: self.config.metric.clone(),
            before: metric_before,
            after: metric_after,
            higher_is_better: self.config.higher_is_better,
        };

        let verdict_input = VerdictInput {
            measurement: &measurement,
            hypothesis: &hypothesis,
            praxis_guidance: &self.config.praxis_guidance,
            current_best: state.ledger.best_metric().unwrap_or(state.baseline_metric),
        };

        let verdict_output = match self.verdict_engine.evaluate(&verdict_input) {
            Ok(v) => v,
            Err(e) => {
                // Verdict engine failure — record as error.
                let entry = LedgerEntry {
                    id: Uuid::new_v4().to_string(),
                    run_id: self.config.id.clone(),
                    experiment_number: exp_num,
                    hypothesis: hypothesis.statement.clone(),
                    mutation_description,
                    mutation_diff,
                    metric_before,
                    metric_after: Some(metric_after),
                    higher_is_better: self.config.higher_is_better,
                    verdict: Verdict::Error,
                    verdict_reason: format!("verdict engine error: {e}"),
                    duration_secs: elapsed,
                    started_at,
                    completed_at,
                };
                state.ledger.append(entry);
                return Ok(Verdict::Error);
            }
        };

        // 6. Append to ledger.
        let verdict = verdict_output.verdict;
        let entry = LedgerEntry {
            id: Uuid::new_v4().to_string(),
            run_id: self.config.id.clone(),
            experiment_number: exp_num,
            hypothesis: hypothesis.statement,
            mutation_description,
            mutation_diff,
            metric_before,
            metric_after: Some(metric_after),
            higher_is_better: self.config.higher_is_better,
            verdict,
            verdict_reason: verdict_output.reason,
            duration_secs: elapsed,
            started_at,
            completed_at,
        };
        state.ledger.append(entry);

        Ok(verdict)
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Drive the experiment loop until a stop condition is reached.
    fn experiment_loop(&self, state: &mut RunnerState) -> StopCondition {
        let sched = &self.config.schedule;

        loop {
            // Check total experiment budget.
            if sched.max_experiments_total > 0
                && state.experiment_count >= sched.max_experiments_total
            {
                return StopCondition::ExperimentBudgetExhausted;
            }

            // Check time budget.
            if sched.max_hours > 0.0 {
                let elapsed_hours =
                    (Utc::now() - state.start_time).num_seconds() as f64 / 3600.0;
                if elapsed_hours >= sched.max_hours {
                    return StopCondition::TimeBudgetExhausted;
                }
            }

            // Check convergence.
            if sched.convergence_threshold > 0.0 && sched.convergence_window > 0 {
                let window = sched.convergence_window as usize;
                if state
                    .ledger
                    .has_converged(window, sched.convergence_threshold)
                {
                    return StopCondition::Converged;
                }
            }

            // Run the next experiment (errors are recorded in the ledger).
            if let Err(e) = self.run_experiment(state) {
                // Unrecoverable error (hypothesis exhausted, etc.) — stop.
                tracing::warn!("autoresearch stopping due to unrecoverable error: {e}");
                return StopCondition::ManualStop;
            }
        }
    }

    /// Return a reference to the configuration.
    #[must_use]
    pub fn config(&self) -> &AutoresearchConfig {
        &self.config
    }
}


// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        schedule::ResearchSchedule,
        ExperimentTarget,
    };

    fn test_config(max_experiments: u32) -> AutoresearchConfig {
        AutoresearchConfig {
            id: "test-run".into(),
            target: ExperimentTarget::Hyperparameters {
                name: "llm-params".into(),
            },
            metric: "val_bpb".into(),
            higher_is_better: false,
            schedule: ResearchSchedule {
                max_experiments_total: max_experiments,
                max_experiments_per_hour: 0, // no rate limiting in tests
                max_hours: 0.0,
                experiment_timeout_secs: 300.0,
                convergence_threshold: 0.0, // no convergence check by default
                convergence_window: 5,
                diminishing_returns_threshold: 0.0,
            },
            praxis_guidance: "Reduce val_bpb below 1.0".into(),
        }
    }

    #[test]
    fn runner_runs_exactly_n_experiments() {
        let config = test_config(5);
        let runner = ResearchRunner::new(config);
        let report = runner.run().unwrap();
        assert_eq!(report.total_experiments, 5);
        assert_eq!(
            report.stop_condition,
            crate::schedule::StopCondition::ExperimentBudgetExhausted
        );
    }

    #[test]
    fn runner_report_has_correct_run_id() {
        let config = test_config(2);
        let runner = ResearchRunner::new(config);
        let report = runner.run().unwrap();
        assert_eq!(report.run_id, "test-run");
    }

    #[test]
    fn runner_rejects_invalid_config() {
        let mut config = test_config(5);
        config.id = "".into();
        let runner = ResearchRunner::new(config);
        assert!(matches!(
            runner.run(),
            Err(AutoresearchError::InvalidConfig(_))
        ));
    }

    #[test]
    fn runner_with_custom_sandbox() {
        use crate::sandbox::FailingSandbox;

        let config = test_config(3);
        let runner = ResearchRunner::new(config)
            .with_sandbox(Box::new(FailingSandbox {
                error_message: "disk full".into(),
            }));
        let report = runner.run().unwrap();
        // All experiments should error.
        assert_eq!(report.error_count, 3);
        assert_eq!(report.kept_count, 0);
    }

    #[test]
    fn runner_with_timeout_sandbox_records_errors() {
        use crate::sandbox::TimeoutSandbox;

        let config = test_config(2);
        let runner = ResearchRunner::new(config)
            .with_sandbox(Box::new(TimeoutSandbox { elapsed_secs: 400.0 }));
        let report = runner.run().unwrap();
        assert_eq!(report.error_count, 2);
    }

    #[test]
    fn runner_convergence_stops_loop() {
        use crate::sandbox::DryRunSandbox;

        // The DryRunSandbox always returns "val_bpb: 1.234", so the metric
        // never changes → the loop converges quickly.
        let config = AutoresearchConfig {
            id: "conv-run".into(),
            target: ExperimentTarget::Hyperparameters { name: "p".into() },
            metric: "val_bpb".into(),
            higher_is_better: false,
            schedule: ResearchSchedule {
                max_experiments_total: 100,
                max_experiments_per_hour: 0,
                max_hours: 0.0,
                experiment_timeout_secs: 300.0,
                convergence_threshold: 1e-4,
                convergence_window: 3,
                diminishing_returns_threshold: 0.0,
            },
            praxis_guidance: "Reduce val_bpb".into(),
        };

        let runner =
            ResearchRunner::new(config).with_sandbox(Box::new(DryRunSandbox::default()));
        let report = runner.run().unwrap();
        assert_eq!(report.stop_condition, StopCondition::Converged);
        // Should have converged well before the 100-experiment cap.
        assert!(report.total_experiments < 100);
    }

    #[test]
    fn runner_experiment_entries_are_in_ledger() {
        let config = test_config(4);
        let runner = ResearchRunner::new(config);
        let report = runner.run().unwrap();
        assert_eq!(report.experiments.len(), report.total_experiments);
        for (i, exp) in report.experiments.iter().enumerate() {
            assert_eq!(exp.number as usize, i + 1);
        }
    }
}
