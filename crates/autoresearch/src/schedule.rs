//! Scheduling constraints for the autoresearch loop.
//!
//! [`ResearchSchedule`] controls the rate of experiments, the total time
//! budget, and the convergence/diminishing-returns stop conditions.

use crate::AutoresearchError;
use serde::{Deserialize, Serialize};

// ── ResearchSchedule ──────────────────────────────────────────────────────────

/// Scheduling parameters that govern when to run experiments and when to stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSchedule {
    /// Maximum number of experiments to run per hour.
    ///
    /// The runner will sleep between experiments to honour this rate.
    /// Set to `0` to disable rate limiting (run as fast as possible).
    pub max_experiments_per_hour: u32,

    /// Hard cap on the total number of experiments (across all hours).
    ///
    /// The run stops when this many experiments have been completed.
    /// Set to `0` for no cap.
    pub max_experiments_total: u32,

    /// Stop after this many hours regardless of progress.
    ///
    /// Set to `0` for no time cap.
    pub max_hours: f64,

    /// Timeout per individual experiment (seconds).
    ///
    /// Experiments that exceed this duration are forcibly aborted and recorded
    /// as [`Verdict::Error`](crate::Verdict::Error).
    pub experiment_timeout_secs: f64,

    /// If the best metric does not improve by at least this fraction over the
    /// last `convergence_window` experiments, declare convergence and stop.
    ///
    /// Set to `0.0` to disable convergence detection.
    pub convergence_threshold: f64,

    /// Number of consecutive experiments to look back when checking convergence.
    pub convergence_window: u32,

    /// Emit a diminishing-returns alert when the improvement rate falls below
    /// this fraction of the initial improvement rate.
    ///
    /// Set to `0.0` to disable the alert.
    pub diminishing_returns_threshold: f64,
}

impl Default for ResearchSchedule {
    fn default() -> Self {
        Self {
            max_experiments_per_hour: 12,
            max_experiments_total: 100,
            max_hours: 8.0,
            experiment_timeout_secs: 300.0,
            convergence_threshold: 1e-4,
            convergence_window: 10,
            diminishing_returns_threshold: 0.1,
        }
    }
}

impl ResearchSchedule {
    /// Validate the schedule, returning an error on any invalid field.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::InvalidConfig`] on invalid values.
    pub fn validate(&self) -> Result<(), AutoresearchError> {
        if self.experiment_timeout_secs <= 0.0 {
            return Err(AutoresearchError::InvalidConfig(
                "experiment_timeout_secs must be positive".into(),
            ));
        }
        if self.convergence_threshold < 0.0 {
            return Err(AutoresearchError::InvalidConfig(
                "convergence_threshold must be non-negative".into(),
            ));
        }
        if self.convergence_window == 0 && self.convergence_threshold > 0.0 {
            return Err(AutoresearchError::InvalidConfig(
                "convergence_window must be at least 1 when convergence_threshold > 0".into(),
            ));
        }
        if self.max_hours < 0.0 {
            return Err(AutoresearchError::InvalidConfig(
                "max_hours must be non-negative".into(),
            ));
        }
        if self.diminishing_returns_threshold < 0.0 {
            return Err(AutoresearchError::InvalidConfig(
                "diminishing_returns_threshold must be non-negative".into(),
            ));
        }
        Ok(())
    }

    /// Compute the minimum delay between experiments (in seconds) to honour
    /// `max_experiments_per_hour`.
    ///
    /// Returns `0.0` when rate limiting is disabled (`max_experiments_per_hour == 0`).
    #[must_use]
    pub fn min_inter_experiment_delay_secs(&self) -> f64 {
        if self.max_experiments_per_hour == 0 {
            return 0.0;
        }
        3600.0 / f64::from(self.max_experiments_per_hour)
    }
}

// ── StopCondition ─────────────────────────────────────────────────────────────

/// The reason the autoresearch loop terminated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopCondition {
    /// The experiment budget was exhausted.
    ExperimentBudgetExhausted,
    /// The time budget was exhausted.
    TimeBudgetExhausted,
    /// The metric converged (improvement < `convergence_threshold`).
    Converged,
    /// Manual cancellation was requested.
    ManualStop,
}

impl std::fmt::Display for StopCondition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExperimentBudgetExhausted => write!(f, "experiment budget exhausted"),
            Self::TimeBudgetExhausted => write!(f, "time budget exhausted"),
            Self::Converged => write!(f, "converged"),
            Self::ManualStop => write!(f, "manual stop"),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_schedule_is_valid() {
        assert!(ResearchSchedule::default().validate().is_ok());
    }

    #[test]
    fn schedule_validates_zero_timeout() {
        let s = ResearchSchedule {
            experiment_timeout_secs: 0.0,
            ..Default::default()
        };
        assert!(matches!(s.validate(), Err(AutoresearchError::InvalidConfig(_))));
    }

    #[test]
    fn schedule_validates_negative_convergence_threshold() {
        let s = ResearchSchedule {
            convergence_threshold: -0.1,
            ..Default::default()
        };
        assert!(matches!(s.validate(), Err(AutoresearchError::InvalidConfig(_))));
    }

    #[test]
    fn schedule_validates_zero_window_with_nonzero_threshold() {
        let s = ResearchSchedule {
            convergence_threshold: 1e-4,
            convergence_window: 0,
            ..Default::default()
        };
        assert!(matches!(s.validate(), Err(AutoresearchError::InvalidConfig(_))));
    }

    #[test]
    fn schedule_min_delay_for_12_per_hour() {
        let s = ResearchSchedule::default();
        let delay = s.min_inter_experiment_delay_secs();
        assert!((delay - 300.0).abs() < 1e-9); // 3600/12 = 300 s
    }

    #[test]
    fn schedule_min_delay_disabled_when_zero() {
        let s = ResearchSchedule {
            max_experiments_per_hour: 0,
            ..Default::default()
        };
        assert_eq!(s.min_inter_experiment_delay_secs(), 0.0);
    }

    #[test]
    fn stop_condition_display() {
        assert_eq!(
            StopCondition::Converged.to_string(),
            "converged"
        );
        assert_eq!(
            StopCondition::TimeBudgetExhausted.to_string(),
            "time budget exhausted"
        );
    }
}
