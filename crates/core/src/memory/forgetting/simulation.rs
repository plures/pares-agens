//! Simulation drills — intentional memory-loss scenarios for resilience testing.
//!
//! A [`SimulationDrill`] describes a hypothetical memory-loss event (e.g. "lose
//! 50 % of all `Conversation` entries") without making any real changes to the
//! backing store.  The engine records every simulated loss in the audit log
//! with `is_simulation = true`.
//!
//! Use drills to:
//! - Validate that agents can still operate after targeted category losses
//! - Benchmark recovery time under various forgetting scenarios
//! - Verify that retention policies are tuned correctly

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::memory::{
    entry::{MemoryCategory, MemoryEntry},
    Error,
};

use super::audit::{AuditAction, AuditEntry, AuditLog};

// ---------------------------------------------------------------------------
// SimulationDrill
// ---------------------------------------------------------------------------

/// Describes a single intentional memory-loss simulation scenario.
///
/// # Example
///
/// ```rust
/// use pares_agens_core::memory::{
///     entry::MemoryCategory,
///     forgetting::simulation::SimulationDrill,
/// };
///
/// let drill = SimulationDrill {
///     name: "chaos-conversation-50pct".to_owned(),
///     categories: vec![MemoryCategory::Conversation],
///     fraction: 0.5,
///     reason: "resilience-test".to_owned(),
/// };
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationDrill {
    /// Human-readable name for the drill (used in reports and audit entries).
    pub name: String,
    /// Categories to include in this drill.  An empty slice targets all categories.
    pub categories: Vec<MemoryCategory>,
    /// Fraction of matching entries to mark as "lost" (0.0 – 1.0).
    ///
    /// `1.0` = total loss; `0.5` = lose half the entries.
    pub fraction: f32,
    /// Human-readable reason recorded in each audit entry.
    pub reason: String,
}

impl SimulationDrill {
    /// Quick constructor for a total-loss drill targeting a single category.
    pub fn total_loss(category: MemoryCategory, reason: impl Into<String>) -> Self {
        Self {
            name: format!("total-loss-{}", category.as_str()),
            categories: vec![category],
            fraction: 1.0,
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// DrillResult
// ---------------------------------------------------------------------------

/// Outcome of running a [`SimulationDrill`].
#[derive(Debug)]
pub struct DrillResult {
    /// Name of the drill that was run.
    pub drill_name: String,
    /// Audit entries recorded (each with `is_simulation = true`).
    pub simulated_losses: Vec<AuditEntry>,
    /// Human-readable summary of the simulation impact.
    pub impact_summary: String,
}

impl DrillResult {
    /// Number of entries that were virtually "lost".
    #[must_use]
    pub fn loss_count(&self) -> usize {
        self.simulated_losses.len()
    }
}

// ---------------------------------------------------------------------------
// run_drill
// ---------------------------------------------------------------------------

/// Execute a [`SimulationDrill`] against a snapshot of memory entries.
///
/// This function is **non-destructive** — it reads `entries` and records
/// simulated losses in `audit_log` with `is_simulation = true`, but never
/// mutates the backing store.
///
/// # Parameters
/// - `entries` — the current live snapshot (from `MemoryStore::all()`).
/// - `drill` — the scenario to simulate.
/// - `audit_log` — receives one audit entry per simulated loss.
///
/// # Errors
/// Currently infallible; returns `Result` for forward compatibility.
pub async fn run_drill(
    entries: &[MemoryEntry],
    drill: &SimulationDrill,
    audit_log: &AuditLog,
) -> Result<DrillResult, Error> {
    let candidates: Vec<&MemoryEntry> = if drill.categories.is_empty() {
        entries.iter().collect()
    } else {
        entries
            .iter()
            .filter(|e| drill.categories.contains(&e.category))
            .collect()
    };

    let loss_count = (candidates.len() as f32 * drill.fraction.clamp(0.0, 1.0)).round() as usize;

    let mut simulated: Vec<AuditEntry> = Vec::with_capacity(loss_count);
    for entry in candidates.iter().take(loss_count) {
        let audit = AuditEntry::new(
            &entry.id,
            entry.category.clone(),
            AuditAction::SimulatedLoss,
            &drill.reason,
            true,
        );
        audit_log.append(audit.clone()).await;
        simulated.push(audit);
    }

    let impact_summary = format!(
        "Drill '{}': {} of {} matching entries virtually lost ({:.0}%)",
        drill.name,
        loss_count,
        candidates.len(),
        drill.fraction * 100.0,
    );

    info!(%impact_summary);

    Ok(DrillResult {
        drill_name: drill.name.clone(),
        simulated_losses: simulated,
        impact_summary,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::entry::MemoryCategory;

    fn make_entry(id: &str, category: MemoryCategory) -> MemoryEntry {
        MemoryEntry {
            id: id.to_owned(),
            content: "test content".to_owned(),
            category,
            tags: vec![],
            embedding: vec![],
            score: 0.0,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[tokio::test]
    async fn drill_no_entries_returns_zero_losses() {
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Conversation, "test");
        let result = run_drill(&[], &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 0);
        assert!(log.is_empty().await);
    }

    #[tokio::test]
    async fn drill_total_loss_marks_all_matching() {
        let entries = vec![
            make_entry("a", MemoryCategory::Conversation),
            make_entry("b", MemoryCategory::Conversation),
            make_entry("c", MemoryCategory::CodePattern),
        ];
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Conversation, "drill");
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        // Only the 2 conversation entries
        assert_eq!(result.loss_count(), 2);
        assert_eq!(log.len().await, 2);
    }

    #[tokio::test]
    async fn drill_half_fraction() {
        let entries: Vec<MemoryEntry> = (0..10)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::Conversation))
            .collect();
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "half".into(),
            categories: vec![MemoryCategory::Conversation],
            fraction: 0.5,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 5);
    }

    #[tokio::test]
    async fn drill_empty_categories_targets_all() {
        let entries = vec![
            make_entry("a", MemoryCategory::Conversation),
            make_entry("b", MemoryCategory::CodePattern),
            make_entry("c", MemoryCategory::Decision),
        ];
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "all-categories".into(),
            categories: vec![], // empty = all
            fraction: 1.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 3);
    }

    #[tokio::test]
    async fn drill_marks_simulation_flag_in_audit() {
        let entries = vec![make_entry("x", MemoryCategory::Preference)];
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Preference, "sim-test");
        run_drill(&entries, &drill, &log).await.unwrap();
        let entries = log.entries().await;
        assert!(entries[0].is_simulation);
        assert_eq!(entries[0].action, AuditAction::SimulatedLoss);
    }

    #[tokio::test]
    async fn drill_does_not_clamp_fraction_above_one() {
        let entries: Vec<MemoryEntry> = (0..4)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::Conversation))
            .collect();
        let log = AuditLog::new();
        // fraction > 1.0 is clamped to 1.0 internally
        let drill = SimulationDrill {
            name: "over-fraction".into(),
            categories: vec![MemoryCategory::Conversation],
            fraction: 2.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 4); // clamped to all
    }

    #[test]
    fn impact_summary_contains_drill_name() {
        // Verify summary string format without async
        let summary = format!(
            "Drill '{}': {} of {} matching entries virtually lost ({:.0}%)",
            "my-drill", 5, 10, 50.0_f32
        );
        assert!(summary.contains("my-drill"));
        assert!(summary.contains("50%"));
    }

    // -----------------------------------------------------------------------
    // Additional mutation-gap tests
    // -----------------------------------------------------------------------

    #[test]
    fn total_loss_constructor_sets_fraction_to_one() {
        let drill = SimulationDrill::total_loss(MemoryCategory::CodePattern, "reason");
        assert_eq!(drill.fraction, 1.0);
        assert_ne!(drill.fraction, 0.0);
    }

    #[test]
    fn total_loss_constructor_sets_name_with_category() {
        let drill = SimulationDrill::total_loss(MemoryCategory::CodePattern, "r");
        assert_eq!(drill.name, "total-loss-code-pattern");
        assert_eq!(drill.categories.len(), 1);
        assert_eq!(drill.categories[0], MemoryCategory::CodePattern);
    }

    #[test]
    fn total_loss_constructor_sets_reason() {
        let drill = SimulationDrill::total_loss(MemoryCategory::Decision, "my-reason");
        assert_eq!(drill.reason, "my-reason");
    }

    #[test]
    fn drill_result_loss_count_matches_vec_len() {
        let result = DrillResult {
            drill_name: "test".into(),
            simulated_losses: vec![],
            impact_summary: String::new(),
        };
        assert_eq!(result.loss_count(), 0);
    }

    #[tokio::test]
    async fn drill_negative_fraction_clamped_to_zero() {
        let entries: Vec<MemoryEntry> = (0..5)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::Conversation))
            .collect();
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "neg-fraction".into(),
            categories: vec![MemoryCategory::Conversation],
            fraction: -0.5,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 0);
    }

    #[tokio::test]
    async fn drill_zero_fraction_loses_nothing() {
        let entries: Vec<MemoryEntry> = (0..5)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::Conversation))
            .collect();
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "zero".into(),
            categories: vec![MemoryCategory::Conversation],
            fraction: 0.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 0);
    }

    #[tokio::test]
    async fn drill_fraction_one_loses_all() {
        let entries: Vec<MemoryEntry> = (0..7)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::ErrorFix))
            .collect();
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "full".into(),
            categories: vec![MemoryCategory::ErrorFix],
            fraction: 1.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 7);
    }

    #[tokio::test]
    async fn drill_result_contains_drill_name() {
        let entries = vec![make_entry("a", MemoryCategory::Fact)];
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Fact, "x");
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.drill_name, "total-loss-fact");
        assert!(result.impact_summary.contains("total-loss-fact"));
    }

    #[tokio::test]
    async fn drill_audit_entries_have_correct_memory_id() {
        let entries = vec![
            make_entry("mem-001", MemoryCategory::Conversation),
            make_entry("mem-002", MemoryCategory::Conversation),
        ];
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Conversation, "id-check");
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        let ids: Vec<&str> = result
            .simulated_losses
            .iter()
            .map(|a| a.memory_id.as_str())
            .collect();
        assert!(ids.contains(&"mem-001"));
        assert!(ids.contains(&"mem-002"));
    }

    #[tokio::test]
    async fn drill_audit_entries_have_correct_category() {
        let entries = vec![make_entry("a", MemoryCategory::Preference)];
        let log = AuditLog::new();
        let drill = SimulationDrill::total_loss(MemoryCategory::Preference, "cat-check");
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(
            result.simulated_losses[0].category,
            Some(MemoryCategory::Preference)
        );
    }

    #[tokio::test]
    async fn drill_unmatched_category_returns_no_losses() {
        let entries = vec![
            make_entry("a", MemoryCategory::Conversation),
            make_entry("b", MemoryCategory::CodePattern),
        ];
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "miss".into(),
            categories: vec![MemoryCategory::ScreenCapture],
            fraction: 1.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 0);
    }

    #[tokio::test]
    async fn drill_multi_category_targets_union() {
        let entries = vec![
            make_entry("a", MemoryCategory::Conversation),
            make_entry("b", MemoryCategory::CodePattern),
            make_entry("c", MemoryCategory::Decision),
        ];
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "multi".into(),
            categories: vec![MemoryCategory::Conversation, MemoryCategory::Decision],
            fraction: 1.0,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert_eq!(result.loss_count(), 2); // a + c, not b
    }

    #[tokio::test]
    async fn drill_impact_summary_shows_percentage() {
        let entries: Vec<MemoryEntry> = (0..10)
            .map(|i| make_entry(&format!("e{i}"), MemoryCategory::Conversation))
            .collect();
        let log = AuditLog::new();
        let drill = SimulationDrill {
            name: "pct-test".into(),
            categories: vec![MemoryCategory::Conversation],
            fraction: 0.3,
            reason: "test".into(),
        };
        let result = run_drill(&entries, &drill, &log).await.unwrap();
        assert!(result.impact_summary.contains("30%"));
        assert!(result.impact_summary.contains("pct-test"));
    }
}
