//! Experiment ledger — append-only log of every research run.
//!
//! [`ExperimentLedger`] stores all [`LedgerEntry`](crate::LedgerEntry) records
//! for a research run and provides query helpers used by the hypothesis engine,
//! verdict engine, and report generator.

use crate::{LedgerEntry, Verdict};

// ── ExperimentLedger ──────────────────────────────────────────────────────────

/// Append-only log of experiments for a single research run.
///
/// The ledger is the single source of truth for the research history.  It is
/// intentionally in-memory in this implementation — callers that need
/// persistence should serialise/deserialise via `serde_json`.
#[derive(Debug, Default)]
pub struct ExperimentLedger {
    entries: Vec<LedgerEntry>,
}

impl ExperimentLedger {
    /// Create a new empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an entry to the ledger.
    pub fn append(&mut self, entry: LedgerEntry) {
        self.entries.push(entry);
    }

    /// Return all entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Return the total number of recorded experiments.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when no experiments have been recorded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the entry with the best metric value seen so far.
    ///
    /// Only considers entries where `metric_after` is present (not sandbox
    /// errors).  Returns `None` if the ledger is empty or all entries errored.
    #[must_use]
    pub fn best_entry(&self) -> Option<&LedgerEntry> {
        self.entries
            .iter()
            .filter(|e| e.metric_after.is_some())
            .max_by(|a, b| {
                // All entries share the same `higher_is_better` flag; use the
                // first to decide comparison direction.
                let after_a = a.metric_after.unwrap_or(f64::NEG_INFINITY);
                let after_b = b.metric_after.unwrap_or(f64::NEG_INFINITY);
                if a.higher_is_better {
                    after_a
                        .partial_cmp(&after_b)
                        .unwrap_or(std::cmp::Ordering::Equal)
                } else {
                    after_b
                        .partial_cmp(&after_a)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }
            })
    }

    /// Return only the entries that were kept (verdict == Keep).
    #[must_use]
    pub fn kept_entries(&self) -> Vec<&LedgerEntry> {
        self.entries
            .iter()
            .filter(|e| e.verdict == Verdict::Keep)
            .collect()
    }

    /// Return the last `n` entries (or fewer if the ledger is smaller).
    #[must_use]
    pub fn last_n(&self, n: usize) -> &[LedgerEntry] {
        let len = self.entries.len();
        if n >= len {
            &self.entries
        } else {
            &self.entries[len - n..]
        }
    }

    /// Return the best metric value seen across all non-error experiments.
    ///
    /// Returns `None` if no successful experiments exist.
    #[must_use]
    pub fn best_metric(&self) -> Option<f64> {
        self.best_entry().and_then(|e| e.metric_after)
    }

    /// Compute the improvement rate over the last `window` experiments.
    ///
    /// The improvement rate is the fraction of entries in the window where the
    /// metric improved (`entry.improved() == true`).
    ///
    /// Returns `0.0` when `window == 0` or the ledger is empty.
    #[must_use]
    pub fn improvement_rate(&self, window: usize) -> f64 {
        if window == 0 || self.entries.is_empty() {
            return 0.0;
        }
        let recent = self.last_n(window);
        let improved = recent.iter().filter(|e| e.improved()).count();
        improved as f64 / recent.len() as f64
    }

    /// Return `true` if the best metric has not improved by more than
    /// `threshold` over the last `window` entries.
    ///
    /// Used by the runner to detect convergence and stop early.
    #[must_use]
    pub fn has_converged(&self, window: usize, threshold: f64) -> bool {
        if window == 0 || threshold <= 0.0 {
            return false;
        }
        let recent = self.last_n(window);
        if recent.len() < window {
            // Not enough data yet.
            return false;
        }
        // Compare the metric_after of the first and last entries in the window.
        let first = recent.first().and_then(|e| e.metric_after);
        let last = recent.last().and_then(|e| e.metric_after);
        match (first, last) {
            (Some(f), Some(l)) => {
                let delta = (l - f).abs();
                delta < threshold
            }
            _ => false,
        }
    }

    /// Serialise the ledger to a JSON array.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if serialisation fails.
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(&self.entries)
    }

    /// Restore a ledger from a JSON array produced by [`to_json`](Self::to_json).
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] if deserialisation fails.
    pub fn from_json(json: &serde_json::Value) -> Result<Self, serde_json::Error> {
        let entries: Vec<LedgerEntry> = serde_json::from_value(json.clone())?;
        Ok(Self { entries })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Verdict;
    use chrono::Utc;

    fn make_entry(n: u32, before: f64, after: Option<f64>, verdict: Verdict) -> LedgerEntry {
        LedgerEntry {
            id: format!("e{n}"),
            run_id: "run-1".into(),
            experiment_number: n,
            hypothesis: "test hypothesis".into(),
            mutation_description: "test mutation".into(),
            mutation_diff: serde_json::Value::Null,
            metric_before: before,
            metric_after: after,
            higher_is_better: true,
            verdict,
            verdict_reason: String::new(),
            duration_secs: 1.0,
            started_at: Utc::now(),
            completed_at: Utc::now(),
        }
    }

    #[test]
    fn ledger_starts_empty() {
        let ledger = ExperimentLedger::new();
        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
    }

    #[test]
    fn append_increases_len() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, Some(0.6), Verdict::Keep));
        assert_eq!(ledger.len(), 1);
        assert!(!ledger.is_empty());
    }

    #[test]
    fn best_entry_picks_highest_metric_after() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, Some(0.6), Verdict::Keep));
        ledger.append(make_entry(2, 0.6, Some(0.8), Verdict::Keep));
        ledger.append(make_entry(3, 0.8, Some(0.7), Verdict::Discard));
        assert_eq!(ledger.best_entry().unwrap().id, "e2");
    }

    #[test]
    fn best_entry_ignores_errors() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, None, Verdict::Error));
        ledger.append(make_entry(2, 0.5, Some(0.6), Verdict::Keep));
        assert_eq!(ledger.best_entry().unwrap().id, "e2");
    }

    #[test]
    fn best_entry_none_when_all_errors() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, None, Verdict::Error));
        assert!(ledger.best_entry().is_none());
    }

    #[test]
    fn kept_entries_filter() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, Some(0.6), Verdict::Keep));
        ledger.append(make_entry(2, 0.6, Some(0.55), Verdict::Discard));
        ledger.append(make_entry(3, 0.6, None, Verdict::Error));
        assert_eq!(ledger.kept_entries().len(), 1);
        assert_eq!(ledger.kept_entries()[0].id, "e1");
    }

    #[test]
    fn last_n_returns_tail() {
        let mut ledger = ExperimentLedger::new();
        for i in 1..=5 {
            ledger.append(make_entry(i, 0.0, Some(0.0), Verdict::Discard));
        }
        let tail = ledger.last_n(3);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].experiment_number, 3);
    }

    #[test]
    fn last_n_clamps_to_ledger_size() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.0, Some(0.0), Verdict::Discard));
        assert_eq!(ledger.last_n(10).len(), 1);
    }

    #[test]
    fn improvement_rate_all_improved() {
        let mut ledger = ExperimentLedger::new();
        for i in 1..=5u32 {
            let before = f64::from(i) * 0.1;
            let after = before + 0.05;
            ledger.append(make_entry(i, before, Some(after), Verdict::Keep));
        }
        let rate = ledger.improvement_rate(5);
        assert!((rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn converged_returns_true_when_no_progress() {
        let mut ledger = ExperimentLedger::new();
        for i in 1..=10u32 {
            ledger.append(make_entry(i, 0.8, Some(0.8), Verdict::Discard));
        }
        assert!(ledger.has_converged(10, 1e-4));
    }

    #[test]
    fn converged_returns_false_with_insufficient_data() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.8, Some(0.8), Verdict::Discard));
        assert!(!ledger.has_converged(10, 1e-4));
    }

    #[test]
    fn json_roundtrip() {
        let mut ledger = ExperimentLedger::new();
        ledger.append(make_entry(1, 0.5, Some(0.7), Verdict::Keep));
        let json = ledger.to_json().unwrap();
        let back = ExperimentLedger::from_json(&json).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back.entries()[0].id, "e1");
    }
}
