//! Research report — summary of all experiments, best findings, and
//! recommended changes.
//!
//! [`ResearchReport`] is generated at the end of (or during) a research run.
//! It aggregates the [`ExperimentLedger`] into human-readable statistics and
//! actionable recommendations.

use crate::{ledger::ExperimentLedger, schedule::StopCondition, LedgerEntry, Verdict};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── ExperimentSummary ─────────────────────────────────────────────────────────

/// A condensed view of a single ledger entry for inclusion in the report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentSummary {
    /// Sequential experiment number (1-based).
    pub number: u32,
    /// The hypothesis statement.
    pub hypothesis: String,
    /// The mutation description.
    pub mutation: String,
    /// Metric before.
    pub metric_before: f64,
    /// Metric after (`None` for sandbox errors).
    pub metric_after: Option<f64>,
    /// Verdict.
    pub verdict: Verdict,
    /// Verdict reason.
    pub reason: String,
    /// Experiment wall-clock duration (seconds).
    pub duration_secs: f64,
}

impl From<&LedgerEntry> for ExperimentSummary {
    fn from(e: &LedgerEntry) -> Self {
        Self {
            number: e.experiment_number,
            hypothesis: e.hypothesis.clone(),
            mutation: e.mutation_description.clone(),
            metric_before: e.metric_before,
            metric_after: e.metric_after,
            verdict: e.verdict,
            reason: e.verdict_reason.clone(),
            duration_secs: e.duration_secs,
        }
    }
}

// ── ReportParams ──────────────────────────────────────────────────────────────

/// Parameters required to build a [`ResearchReport`] from a ledger.
#[derive(Debug, Clone)]
pub struct ReportParams {
    /// Research run identifier.
    pub run_id: String,
    /// `ExperimentTarget::label()` string.
    pub target_label: String,
    /// Metric name being optimised.
    pub metric: String,
    /// Whether a higher metric value is better.
    pub higher_is_better: bool,
    /// Metric value before any mutations.
    pub baseline_metric: f64,
    /// When the run started.
    pub started_at: DateTime<Utc>,
    /// Why the run terminated.
    pub stop_condition: StopCondition,
}

// ── ResearchReport ────────────────────────────────────────────────────────────

/// Full summary of a completed (or in-progress) autoresearch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchReport {
    /// Research run identifier.
    pub run_id: String,

    /// The optimisation target label.
    pub target_label: String,

    /// Name of the metric being optimised.
    pub metric: String,

    /// Whether higher is better.
    pub higher_is_better: bool,

    /// Timestamp when the run started.
    pub started_at: DateTime<Utc>,

    /// Timestamp when the report was generated.
    pub generated_at: DateTime<Utc>,

    /// Why the run stopped.
    pub stop_condition: StopCondition,

    /// Total experiments executed.
    pub total_experiments: usize,

    /// Number of experiments that were kept.
    pub kept_count: usize,

    /// Number of experiments that were discarded.
    pub discarded_count: usize,

    /// Number of experiments that errored.
    pub error_count: usize,

    /// Metric value at the start of the run (before any mutations).
    pub baseline_metric: f64,

    /// Best metric value achieved.
    pub best_metric: f64,

    /// Overall improvement: `best_metric − baseline_metric`
    /// (positive = improvement for `higher_is_better == true`).
    pub total_improvement: f64,

    /// Description of the mutation that produced the best result.
    pub best_mutation: String,

    /// All experiments in chronological order (condensed view).
    pub experiments: Vec<ExperimentSummary>,

    /// Human-readable recommended next steps.
    pub recommendations: Vec<String>,
}

impl ResearchReport {
    /// Build a report from a completed ledger.
    ///
    /// # Parameters
    ///
    /// - `params` — run metadata (see [`ReportParams`]).
    /// - `ledger` — the full experiment ledger.
    #[must_use]
    pub fn from_ledger(params: ReportParams, ledger: &ExperimentLedger) -> Self {
        let ReportParams {
            run_id,
            target_label,
            metric,
            higher_is_better,
            baseline_metric,
            started_at,
            stop_condition,
        } = params;
        let total_experiments = ledger.len();
        let kept_count = ledger
            .entries()
            .iter()
            .filter(|e| e.verdict == Verdict::Keep)
            .count();
        let error_count = ledger
            .entries()
            .iter()
            .filter(|e| e.verdict == Verdict::Error)
            .count();
        let discarded_count = total_experiments - kept_count - error_count;

        let best_metric = ledger.best_metric().unwrap_or(baseline_metric);
        let total_improvement = if higher_is_better {
            best_metric - baseline_metric
        } else {
            baseline_metric - best_metric
        };

        let best_mutation = ledger
            .best_entry()
            .map(|e| e.mutation_description.clone())
            .unwrap_or_default();

        let experiments = ledger
            .entries()
            .iter()
            .map(ExperimentSummary::from)
            .collect();

        let recommendations = build_recommendations(ledger, higher_is_better, &best_mutation);

        Self {
            run_id,
            target_label,
            metric,
            higher_is_better,
            started_at,
            generated_at: Utc::now(),
            stop_condition,
            total_experiments,
            kept_count,
            discarded_count,
            error_count,
            baseline_metric,
            best_metric,
            total_improvement,
            best_mutation,
            experiments,
            recommendations,
        }
    }

    /// Return `true` when the run produced any net improvement over the baseline.
    #[must_use]
    pub fn improved(&self) -> bool {
        self.total_improvement > 0.0
    }

    /// Serialise the report to pretty-printed JSON.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialisation fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Generate human-readable recommendations based on ledger statistics.
fn build_recommendations(
    ledger: &ExperimentLedger,
    higher_is_better: bool,
    best_mutation: &str,
) -> Vec<String> {
    let mut recs = Vec::new();

    if ledger.is_empty() {
        recs.push("No experiments were run. Check the target and sandbox configuration.".into());
        return recs;
    }

    let rate = ledger.improvement_rate(ledger.len());
    if rate > 0.5 {
        recs.push(format!(
            "High improvement rate ({:.0}%). Consider running more experiments to continue optimisation.",
            rate * 100.0
        ));
    } else if rate < 0.1 {
        recs.push(
            "Low improvement rate (<10%). The search space may be exhausted or the metric ceiling reached.".into(),
        );
    }

    if !best_mutation.is_empty() {
        recs.push(format!(
            "Best result achieved by: {best_mutation}. Apply this mutation permanently to the target."
        ));
    }

    let error_count = ledger
        .entries()
        .iter()
        .filter(|e| e.verdict == Verdict::Error)
        .count();
    if error_count > 0 {
        recs.push(format!(
            "{error_count} experiment(s) errored. Inspect sandbox logs to improve robustness."
        ));
    }

    if !higher_is_better {
        recs.push(
            "This run minimises the metric. Verify that the metric floor has not been reached."
                .into(),
        );
    }

    recs
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ledger::ExperimentLedger, Verdict};
    use chrono::Utc;

    fn make_entry(n: u32, before: f64, after: Option<f64>, verdict: Verdict) -> LedgerEntry {
        LedgerEntry {
            id: format!("e{n}"),
            run_id: "run-1".into(),
            experiment_number: n,
            hypothesis: "test".into(),
            mutation_description: format!("mutation-{n}"),
            mutation_diff: serde_json::Value::Null,
            metric_before: before,
            metric_after: after,
            higher_is_better: true,
            verdict,
            verdict_reason: "ok".into(),
            duration_secs: 1.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        }
    }

    fn build_ledger() -> ExperimentLedger {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, Some(0.6), Verdict::Keep));
        ledger.append(make_entry(2, 0.6, Some(0.55), Verdict::Discard));
        ledger.append(make_entry(3, 0.6, None, Verdict::Error));
        ledger
    }

    fn default_params(stop_condition: StopCondition) -> ReportParams {
        ReportParams {
            run_id: "run-1".into(),
            target_label: "procedure:test".into(),
            metric: "recall".into(),
            higher_is_better: true,
            baseline_metric: 0.5,
            started_at: Utc::now(),
            stop_condition,
        }
    }

    #[test]
    fn report_counts_are_correct() {
        let ledger = build_ledger();
        let report = ResearchReport::from_ledger(
            default_params(StopCondition::ExperimentBudgetExhausted),
            &ledger,
        );
        assert_eq!(report.total_experiments, 3);
        assert_eq!(report.kept_count, 1);
        assert_eq!(report.discarded_count, 1);
        assert_eq!(report.error_count, 1);
    }

    #[test]
    fn report_best_metric() {
        let ledger = build_ledger();
        let report = ResearchReport::from_ledger(default_params(StopCondition::Converged), &ledger);
        assert!((report.best_metric - 0.6).abs() < 1e-9);
        assert!(report.improved());
    }

    #[test]
    fn report_total_improvement_higher_is_better() {
        let ledger = build_ledger();
        let report = ResearchReport::from_ledger(default_params(StopCondition::Converged), &ledger);
        // best = 0.6, baseline = 0.5, improvement = 0.1
        assert!((report.total_improvement - 0.1).abs() < 1e-9);
    }

    #[test]
    fn report_has_recommendations() {
        let ledger = build_ledger();
        let report = ResearchReport::from_ledger(default_params(StopCondition::Converged), &ledger);
        assert!(!report.recommendations.is_empty());
    }

    #[test]
    fn report_serialises_to_json() {
        let ledger = build_ledger();
        let report = ResearchReport::from_ledger(default_params(StopCondition::Converged), &ledger);
        let json = report.to_json().unwrap();
        assert!(json.contains("\"run_id\""));
        assert!(json.contains("\"experiments\""));
    }

    #[test]
    fn empty_ledger_recommendations() {
        let ledger = ExperimentLedger::new();
        let recs = super::build_recommendations(&ledger, true, "");
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("No experiments"));
    }
}
