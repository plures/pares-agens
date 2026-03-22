//! Metric extraction and comparison.
//!
//! [`Measurement`] parses a scalar metric from the raw execution output
//! produced by the sandbox, and compares it against a baseline to determine
//! whether an improvement occurred.

use crate::AutoresearchError;
use serde::{Deserialize, Serialize};

// ── MetricExtractor trait ─────────────────────────────────────────────────────

/// Parse a named scalar metric from raw execution output.
pub trait MetricExtractor: Send + Sync {
    /// Extract the value of `metric_name` from `stdout`.
    ///
    /// # Errors
    ///
    /// Returns [`AutoresearchError::MeasurementError`] if the metric cannot be
    /// found or parsed.
    fn extract(&self, stdout: &str, metric_name: &str) -> Result<f64, AutoresearchError>;
}

// ── KeyValueExtractor ─────────────────────────────────────────────────────────

/// Extracts a metric from key-value pairs in the output.
///
/// Looks for lines of the form `<metric_name>: <value>` or
/// `<metric_name>=<value>` (case-insensitive key match).
#[derive(Debug, Default, Clone)]
pub struct KeyValueExtractor;

impl MetricExtractor for KeyValueExtractor {
    fn extract(&self, stdout: &str, metric_name: &str) -> Result<f64, AutoresearchError> {
        let key_lower = metric_name.to_lowercase();
        for line in stdout.lines() {
            // Try `: ` separator.
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().to_lowercase() == key_lower {
                    let val_str = v.split_whitespace().next().unwrap_or("").trim();
                    return val_str.parse::<f64>().map_err(|_| {
                        AutoresearchError::MeasurementError(format!(
                            "could not parse metric '{metric_name}' value {val_str:?}"
                        ))
                    });
                }
            }
            // Try `=` separator.
            if let Some((k, v)) = line.split_once('=') {
                if k.trim().to_lowercase() == key_lower {
                    let val_str = v.split_whitespace().next().unwrap_or("").trim();
                    return val_str.parse::<f64>().map_err(|_| {
                        AutoresearchError::MeasurementError(format!(
                            "could not parse metric '{metric_name}' value {val_str:?}"
                        ))
                    });
                }
            }
        }
        Err(AutoresearchError::MeasurementError(format!(
            "metric '{metric_name}' not found in output"
        )))
    }
}

// ── Measurement ───────────────────────────────────────────────────────────────

/// The result of measuring a single experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Measurement {
    /// Name of the metric measured.
    pub metric_name: String,
    /// Value before the experiment (baseline).
    pub before: f64,
    /// Value after the experiment.
    pub after: f64,
    /// Whether a higher value is better.
    pub higher_is_better: bool,
}

impl Measurement {
    /// Return the raw delta (`after − before`).
    #[must_use]
    pub fn delta(&self) -> f64 {
        self.after - self.before
    }

    /// Return `true` when the metric improved in the correct direction.
    #[must_use]
    pub fn improved(&self) -> bool {
        if self.higher_is_better {
            self.after > self.before
        } else {
            self.after < self.before
        }
    }

    /// Return the relative improvement as a fraction of the baseline
    /// (positive = improvement, negative = regression).
    ///
    /// Returns `0.0` when `before == 0.0` to avoid division by zero.
    #[must_use]
    pub fn relative_improvement(&self) -> f64 {
        if self.before == 0.0 {
            return 0.0;
        }
        let signed = if self.higher_is_better {
            self.after - self.before
        } else {
            self.before - self.after
        };
        signed / self.before.abs()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── KeyValueExtractor tests ───────────────────────────────────────────

    #[test]
    fn extracts_colon_separated_metric() {
        let extractor = KeyValueExtractor;
        let stdout = "epoch: 10\nval_bpb: 1.234\nloss: 0.5\n";
        let val = extractor.extract(stdout, "val_bpb").unwrap();
        assert!((val - 1.234).abs() < 1e-9);
    }

    #[test]
    fn extracts_equals_separated_metric() {
        let extractor = KeyValueExtractor;
        let stdout = "val_loss=0.312\naccuracy=0.987\n";
        let val = extractor.extract(stdout, "accuracy").unwrap();
        assert!((val - 0.987).abs() < 1e-9);
    }

    #[test]
    fn extractor_is_case_insensitive() {
        let extractor = KeyValueExtractor;
        let stdout = "Val_BPB: 2.0\n";
        let val = extractor.extract(stdout, "val_bpb").unwrap();
        assert!((val - 2.0).abs() < 1e-9);
    }

    #[test]
    fn extractor_returns_error_when_metric_absent() {
        let extractor = KeyValueExtractor;
        let stdout = "epoch: 10\n";
        let err = extractor.extract(stdout, "val_bpb").unwrap_err();
        assert!(matches!(err, AutoresearchError::MeasurementError(_)));
    }

    #[test]
    fn extractor_returns_error_when_value_not_numeric() {
        let extractor = KeyValueExtractor;
        let stdout = "val_bpb: not-a-number\n";
        let err = extractor.extract(stdout, "val_bpb").unwrap_err();
        assert!(matches!(err, AutoresearchError::MeasurementError(_)));
    }

    // ── Measurement tests ─────────────────────────────────────────────────

    #[test]
    fn measurement_improved_higher_is_better() {
        let m = Measurement {
            metric_name: "accuracy".into(),
            before: 0.8,
            after: 0.85,
            higher_is_better: true,
        };
        assert!(m.improved());
        assert!((m.delta() - 0.05).abs() < 1e-9);
    }

    #[test]
    fn measurement_not_improved_higher_is_better() {
        let m = Measurement {
            metric_name: "accuracy".into(),
            before: 0.85,
            after: 0.80,
            higher_is_better: true,
        };
        assert!(!m.improved());
    }

    #[test]
    fn measurement_improved_lower_is_better() {
        let m = Measurement {
            metric_name: "loss".into(),
            before: 1.0,
            after: 0.9,
            higher_is_better: false,
        };
        assert!(m.improved());
    }

    #[test]
    fn relative_improvement_higher_is_better() {
        let m = Measurement {
            metric_name: "recall".into(),
            before: 0.8,
            after: 1.0,
            higher_is_better: true,
        };
        let rel = m.relative_improvement();
        // (1.0 - 0.8) / 0.8 = 0.25
        assert!((rel - 0.25).abs() < 1e-9);
    }

    #[test]
    fn relative_improvement_lower_is_better() {
        let m = Measurement {
            metric_name: "loss".into(),
            before: 1.0,
            after: 0.8,
            higher_is_better: false,
        };
        let rel = m.relative_improvement();
        // (1.0 - 0.8) / 1.0 = 0.2
        assert!((rel - 0.2).abs() < 1e-9);
    }

    #[test]
    fn relative_improvement_zero_baseline() {
        let m = Measurement {
            metric_name: "metric".into(),
            before: 0.0,
            after: 1.0,
            higher_is_better: true,
        };
        assert_eq!(m.relative_improvement(), 0.0);
    }
}
