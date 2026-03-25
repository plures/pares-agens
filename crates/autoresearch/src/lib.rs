//! `pares-agens-autoresearch` — Autonomous experiment loop for Pares Agens.
//!
//! Implements Karpathy's *autoresearch* concept natively using praxis guidance
//! and PluresDB procedures (no Python).  The cerebellum autonomously runs
//! experiments in a closed loop:
//!
//! ```text
//! hypothesize → mutate → execute → measure → keep/discard → repeat
//! ```
//!
//! # Architecture
//!
//! ```text
//! AutoresearchConfig
//!       │
//!       ▼
//! ResearchRunner ──► HypothesisEngine ──► MutationOperator
//!       │                                        │
//!       ▼                                        ▼
//! ExperimentLedger ◄── VerdictEngine ◄── Measurement
//!       │
//!       ▼
//! ResearchReport
//! ```
//!
//! # Modules
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`ledger`] | [`ExperimentLedger`](ledger::ExperimentLedger) — append-only log of every run |
//! | [`hypothesis`] | [`HypothesisEngine`](hypothesis::HypothesisEngine) — selects the next experiment |
//! | [`mutation`] | [`MutationOperator`](mutation::MutationOperator) — code / config / param changes |
//! | [`sandbox`] | [`ExecutionSandbox`](sandbox::ExecutionSandbox) — isolated run with timeout |
//! | [`measurement`] | [`Measurement`](measurement::Measurement) — extract metric, compare baseline |
//! | [`verdict`] | [`VerdictEngine`](verdict::VerdictEngine) — keep or discard a result |
//! | [`schedule`] | [`ResearchSchedule`](schedule::ResearchSchedule) — rate limits and stop conditions |
//! | [`report`] | [`ResearchReport`](report::ResearchReport) — summary of all experiments |
//! | [`runner`] | [`ResearchRunner`](runner::ResearchRunner) — orchestrates the full loop |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_autoresearch::{
//!     AutoresearchConfig, ExperimentTarget,
//!     runner::ResearchRunner,
//!     schedule::ResearchSchedule,
//! };
//!
//! let config = AutoresearchConfig {
//!     id: "my-experiment".into(),
//!     target: ExperimentTarget::Procedure { name: "search-pipeline".into() },
//!     metric: "recall@10".into(),
//!     higher_is_better: true,
//!     schedule: ResearchSchedule::default(),
//!     praxis_guidance: "Optimise recall without degrading latency above 200ms".into(),
//! };
//!
//! let runner = ResearchRunner::new(config);
//! // runner.run().await  — drives the autonomous loop
//! ```

#![warn(missing_docs)]

pub mod hypothesis;
pub mod ledger;
pub mod measurement;
pub mod mutation;
pub mod report;
pub mod runner;
pub mod sandbox;
pub mod schedule;
pub mod verdict;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use schedule::ResearchSchedule;

// ── Error type ────────────────────────────────────────────────────────────────

/// All errors that can surface from the autoresearch loop.
#[derive(Debug, Error)]
pub enum AutoresearchError {
    /// The supplied configuration is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// The execution sandbox failed or timed out.
    #[error("sandbox error: {0}")]
    SandboxError(String),

    /// Metric extraction from the execution output failed.
    #[error("measurement error: {0}")]
    MeasurementError(String),

    /// A ledger write/read operation failed.
    #[error("ledger error: {0}")]
    LedgerError(String),

    /// The hypothesis engine could not produce a next experiment.
    #[error("hypothesis error: {0}")]
    HypothesisError(String),

    /// JSON (de)serialisation failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── ExperimentTarget ──────────────────────────────────────────────────────────

/// The thing being optimised in a research run.
///
/// Targets are intentionally open-ended — any measurable process can be
/// the subject of autonomous experimentation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExperimentTarget {
    /// Optimise a PluresDB procedure (modify step order, parameters, etc.).
    Procedure {
        /// Name of the procedure in PluresDB.
        name: String,
    },
    /// Optimise a configuration file or key-value config block.
    Config {
        /// File path or config block identifier.
        path: String,
    },
    /// Optimise a source-code file (e.g., build flags, prompt text).
    SourceFile {
        /// File path relative to the workspace root.
        path: String,
    },
    /// Optimise hyperparameters stored as a JSON object.
    Hyperparameters {
        /// Symbolic name for the hyperparameter set.
        name: String,
    },
    /// Optimise an arbitrary shell/cargo command invocation.
    Command {
        /// The command template to run (may contain `{{param}}` slots).
        template: String,
    },
}

impl ExperimentTarget {
    /// Return a human-readable label for the target.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Procedure { name } => format!("procedure:{name}"),
            Self::Config { path } => format!("config:{path}"),
            Self::SourceFile { path } => format!("file:{path}"),
            Self::Hyperparameters { name } => format!("hyperparams:{name}"),
            Self::Command { template } => format!("command:{}", &template[..template.len().min(40)]),
        }
    }
}

// ── Verdict ───────────────────────────────────────────────────────────────────

/// The outcome of evaluating a single experiment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The experiment improved the metric — keep the mutation.
    Keep,
    /// The experiment did not improve (or degraded) the metric — revert.
    Discard,
    /// The experiment could not be evaluated (sandbox error, timeout, etc.).
    Error,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => write!(f, "KEEP"),
            Self::Discard => write!(f, "DISCARD"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

// ── LedgerEntry ───────────────────────────────────────────────────────────────

/// A single experiment record written to the [`ledger::ExperimentLedger`].
///
/// Every field is immutable after creation — the ledger is append-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Unique experiment identifier (UUID v4).
    pub id: String,
    /// Parent research run identifier.
    pub run_id: String,
    /// Sequential experiment number within this run (1-based).
    pub experiment_number: u32,
    /// The hypothesis guiding this experiment.
    pub hypothesis: String,
    /// Human-readable description of the mutation applied.
    pub mutation_description: String,
    /// The serialised mutation diff (JSON).
    pub mutation_diff: serde_json::Value,
    /// Metric value before the mutation (baseline).
    pub metric_before: f64,
    /// Metric value after the mutation (`None` if the sandbox errored).
    pub metric_after: Option<f64>,
    /// Whether a higher metric value is better.
    pub higher_is_better: bool,
    /// Praxis verdict: keep or discard.
    pub verdict: Verdict,
    /// Reason provided by the verdict engine.
    pub verdict_reason: String,
    /// Wall-clock duration of the experiment (seconds).
    pub duration_secs: f64,
    /// ISO-8601 timestamp when the experiment started.
    pub started_at: DateTime<Utc>,
    /// ISO-8601 timestamp when the experiment completed.
    pub completed_at: DateTime<Utc>,
}

impl LedgerEntry {
    /// Return the metric delta (`metric_after − metric_before`).
    ///
    /// Returns `None` if `metric_after` is unavailable (sandbox error).
    #[must_use]
    pub fn delta(&self) -> Option<f64> {
        self.metric_after.map(|after| after - self.metric_before)
    }

    /// Return `true` when the metric improved (respecting `higher_is_better`).
    #[must_use]
    pub fn improved(&self) -> bool {
        match self.delta() {
            Some(d) if self.higher_is_better => d > 0.0,
            Some(d) => d < 0.0,
            None => false,
        }
    }
}

// ── AutoresearchConfig ────────────────────────────────────────────────────────

/// Top-level configuration for an autoresearch run.
///
/// This is the Pares Agens equivalent of Karpathy's `program.md` — it tells
/// the cerebellum *what* to optimise, *how* to measure success, and *when* to
/// stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoresearchConfig {
    /// Unique research run identifier (human-readable slug).
    pub id: String,

    /// The target being optimised.
    pub target: ExperimentTarget,

    /// Name of the scalar metric to optimise (e.g. `"val_bpb"`, `"recall@10"`).
    pub metric: String,

    /// Direction of improvement: `true` = higher is better (accuracy, recall),
    /// `false` = lower is better (loss, latency).
    pub higher_is_better: bool,

    /// Scheduling constraints (rate, budget, convergence).
    pub schedule: ResearchSchedule,

    /// Praxis guidance — natural-language instructions for the cerebellum.
    /// Equivalent to Karpathy's `program.md`.  The hypothesis engine uses this
    /// to constrain the search space and bias hypothesis generation.
    pub praxis_guidance: String,
}

impl AutoresearchConfig {
    /// Validate the configuration, returning an error on any invalid field.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::InvalidConfig`] on invalid configuration.
    pub fn validate(&self) -> Result<(), AutoresearchError> {
        if self.id.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "id must not be empty".into(),
            ));
        }
        if self.metric.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "metric must not be empty".into(),
            ));
        }
        if self.praxis_guidance.trim().is_empty() {
            return Err(AutoresearchError::InvalidConfig(
                "praxis_guidance must not be empty".into(),
            ));
        }
        self.schedule.validate()?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiment_target_label_is_descriptive() {
        assert_eq!(
            ExperimentTarget::Procedure {
                name: "search-pipeline".into()
            }
            .label(),
            "procedure:search-pipeline"
        );
        assert_eq!(
            ExperimentTarget::Config {
                path: "config/model.toml".into()
            }
            .label(),
            "config:config/model.toml"
        );
        assert_eq!(
            ExperimentTarget::SourceFile {
                path: "src/main.rs".into()
            }
            .label(),
            "file:src/main.rs"
        );
        assert_eq!(
            ExperimentTarget::Hyperparameters {
                name: "llm-params".into()
            }
            .label(),
            "hyperparams:llm-params"
        );
    }

    #[test]
    fn verdict_display() {
        assert_eq!(Verdict::Keep.to_string(), "KEEP");
        assert_eq!(Verdict::Discard.to_string(), "DISCARD");
        assert_eq!(Verdict::Error.to_string(), "ERROR");
    }

    #[test]
    fn ledger_entry_delta_and_improved_higher_is_better() {
        let entry = LedgerEntry {
            id: "x".into(),
            run_id: "r".into(),
            experiment_number: 1,
            hypothesis: "h".into(),
            mutation_description: "m".into(),
            mutation_diff: serde_json::Value::Null,
            metric_before: 0.5,
            metric_after: Some(0.7),
            higher_is_better: true,
            verdict: Verdict::Keep,
            verdict_reason: String::new(),
            duration_secs: 10.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };
        assert!((entry.delta().unwrap() - 0.2).abs() < 1e-9);
        assert!(entry.improved());
    }

    #[test]
    fn ledger_entry_improved_lower_is_better() {
        let entry = LedgerEntry {
            id: "x".into(),
            run_id: "r".into(),
            experiment_number: 1,
            hypothesis: "h".into(),
            mutation_description: "m".into(),
            mutation_diff: serde_json::Value::Null,
            metric_before: 0.8,
            metric_after: Some(0.6),
            higher_is_better: false,
            verdict: Verdict::Keep,
            verdict_reason: String::new(),
            duration_secs: 5.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };
        assert!(entry.improved());
    }

    #[test]
    fn ledger_entry_no_improvement_returns_false() {
        let entry = LedgerEntry {
            id: "x".into(),
            run_id: "r".into(),
            experiment_number: 1,
            hypothesis: "h".into(),
            mutation_description: "m".into(),
            mutation_diff: serde_json::Value::Null,
            metric_before: 0.8,
            metric_after: Some(0.7),
            higher_is_better: true,
            verdict: Verdict::Discard,
            verdict_reason: String::new(),
            duration_secs: 5.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };
        assert!(!entry.improved());
    }

    #[test]
    fn ledger_entry_sandbox_error_not_improved() {
        let entry = LedgerEntry {
            id: "x".into(),
            run_id: "r".into(),
            experiment_number: 1,
            hypothesis: "h".into(),
            mutation_description: "m".into(),
            mutation_diff: serde_json::Value::Null,
            metric_before: 0.5,
            metric_after: None,
            higher_is_better: true,
            verdict: Verdict::Error,
            verdict_reason: "sandbox timeout".into(),
            duration_secs: 300.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        };
        assert!(!entry.improved());
        assert!(entry.delta().is_none());
    }

    #[test]
    fn config_validate_rejects_empty_id() {
        let cfg = AutoresearchConfig {
            id: "  ".into(),
            target: ExperimentTarget::Procedure { name: "p".into() },
            metric: "recall".into(),
            higher_is_better: true,
            schedule: ResearchSchedule::default(),
            praxis_guidance: "do your best".into(),
        };
        assert!(matches!(
            cfg.validate(),
            Err(AutoresearchError::InvalidConfig(_))
        ));
    }

    #[test]
    fn config_validate_rejects_empty_metric() {
        let cfg = AutoresearchConfig {
            id: "run-1".into(),
            target: ExperimentTarget::Procedure { name: "p".into() },
            metric: "".into(),
            higher_is_better: true,
            schedule: ResearchSchedule::default(),
            praxis_guidance: "do your best".into(),
        };
        assert!(matches!(
            cfg.validate(),
            Err(AutoresearchError::InvalidConfig(_))
        ));
    }

    #[test]
    fn config_validate_ok_for_valid_config() {
        let cfg = AutoresearchConfig {
            id: "run-1".into(),
            target: ExperimentTarget::Procedure { name: "p".into() },
            metric: "recall@10".into(),
            higher_is_better: true,
            schedule: ResearchSchedule::default(),
            praxis_guidance: "Optimise recall".into(),
        };
        assert!(cfg.validate().is_ok());
    }
}
