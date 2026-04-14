//! Task-decomposition size constraint (ADR-0013).
//!
//! Enforces the task size limits proven by the 2026-04-10 sub-agent tests:
//!
//! | Limit | Value | Constraint ID |
//! |-------|-------|---------------|
//! | Task description word count | ≤ 200 | C-0009 |
//! | Expected text output | ≤ 2 000 chars | C-0010 |
//!
//! # Two evaluation paths
//!
//! 1. **Direct check** — [`TaskSizeConstraint::check`] counts words in a raw
//!    description string and returns a [`TaskSizeViolation`] when a limit is
//!    exceeded.  The violation can be converted to an [`Event`] via
//!    [`TaskSizeViolation::into_event`].
//!
//! 2. **Store-based check** — [`TaskSizeConstraint::register`] inserts C-0009
//!    and C-0010 into a [`PraxisStore`].  The orchestration layer must then
//!    set the following metadata fields on the [`AgentContext`] before calling
//!    `on_action` / `evaluate`:
//!
//!    | Field | JSON type | Description |
//!    |-------|-----------|-------------|
//!    | `task_description_word_count` | number | Word count of the task description |
//!    | `expected_output_type` | string | e.g. `"text"` |
//!    | `expected_output_chars` | number | Estimated output character count |

use crate::event::Event;
use pares_agens_praxis::db::{
    schema::{Condition, Constraint, Severity},
    store::PraxisStore,
};
use serde_json::json;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of words allowed in a sub-agent task description (ADR-0013).
pub const MAX_DESCRIPTION_WORD_COUNT: usize = 200;

/// Maximum estimated character count for expected text output (ADR-0013).
pub const MAX_OUTPUT_CHARS: usize = 2000;

// ---------------------------------------------------------------------------
// TaskSizeViolation
// ---------------------------------------------------------------------------

/// Produced by [`TaskSizeConstraint::check`] when a task exceeds the size
/// limits defined in ADR-0013.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskSizeViolation {
    /// Word count of the task description.
    pub word_count: usize,
    /// Estimated character count of expected text output, if the output type
    /// is `"text"` and it exceeded the limit.
    pub output_chars: Option<usize>,
    /// Suggested number of sub-tasks to decompose the original task into.
    pub suggested_splits: usize,
}

impl TaskSizeViolation {
    /// Convert this violation into a `task.decomposition.required` event
    /// suitable for emission on the agent event bus.
    pub fn into_event(self) -> Event {
        Event::TaskDecompositionRequired {
            word_count: self.word_count,
            output_chars: self.output_chars,
            suggested_splits: self.suggested_splits,
        }
    }
}

// ---------------------------------------------------------------------------
// TaskSizeConstraint
// ---------------------------------------------------------------------------

/// Enforces task decomposition rules from ADR-0013.
///
/// # Example — direct check
///
/// ```rust
/// use pares_agens_core::praxis::constraints::{TaskSizeConstraint, MAX_DESCRIPTION_WORD_COUNT};
///
/// let description = "word ".repeat(201);
/// let violation = TaskSizeConstraint::check(&description, None, None);
/// assert!(violation.is_some());
/// let ev = violation.unwrap().into_event();
/// assert_eq!(ev.kind(), "task_decomposition_required");
/// ```
///
/// # Example — store registration
///
/// ```rust
/// use pares_agens_praxis::db::store::PraxisStore;
/// use pares_agens_core::praxis::constraints::TaskSizeConstraint;
///
/// let mut store = PraxisStore::new();
/// TaskSizeConstraint::register(&mut store);
/// assert!(store.get_constraint("C-0009").is_some());
/// assert!(store.get_constraint("C-0010").is_some());
/// ```
pub struct TaskSizeConstraint;

impl TaskSizeConstraint {
    /// Directly check a task against the ADR-0013 size limits.
    ///
    /// - `description` — raw task description text.
    /// - `output_type` — optional output type hint; only `"text"` triggers the
    ///   output-size check.
    /// - `estimated_output_chars` — estimated character count of the expected
    ///   output; only inspected when `output_type == Some("text")`.
    ///
    /// Returns `Some(TaskSizeViolation)` when at least one limit is exceeded,
    /// or `None` when the task is within bounds.
    pub fn check(
        description: &str,
        output_type: Option<&str>,
        estimated_output_chars: Option<usize>,
    ) -> Option<TaskSizeViolation> {
        let word_count = description.split_whitespace().count();

        let output_chars = if output_type == Some("text") {
            estimated_output_chars
        } else {
            None
        };

        let word_violation = word_count > MAX_DESCRIPTION_WORD_COUNT;
        let output_violation = output_chars.is_some_and(|c| c > MAX_OUTPUT_CHARS);

        if !word_violation && !output_violation {
            return None;
        }

        let word_splits = if word_violation {
            word_count.div_ceil(MAX_DESCRIPTION_WORD_COUNT)
        } else {
            1
        };
        let output_splits = match output_chars {
            Some(chars) if chars > MAX_OUTPUT_CHARS => chars.div_ceil(MAX_OUTPUT_CHARS),
            _ => 1,
        };
        let suggested_splits = word_splits.max(output_splits).max(2);

        Some(TaskSizeViolation {
            word_count,
            output_chars,
            suggested_splits,
        })
    }

    /// Insert **C-0009** (word count) and **C-0010** (output chars) into
    /// `store`.
    ///
    /// Calling this in addition to the seed store ensures the constraints are
    /// available even when `default_store()` is not used.  In practice the
    /// seed already registers these constraints; `register` is provided for
    /// custom stores and testing.
    pub fn register(store: &mut PraxisStore) {
        // C-0009 — task description word count ≤ 200
        store.upsert_constraint(Constraint {
            id: "C-0009".into(),
            description: "Sub-agent task descriptions must not exceed 200 words (ADR-0013)."
                .into(),
            when: Condition::FieldGt {
                field: "task_description_word_count".into(),
                threshold: MAX_DESCRIPTION_WORD_COUNT as f64,
            },
            require: Condition::FieldLt {
                field: "task_description_word_count".into(),
                threshold: MAX_DESCRIPTION_WORD_COUNT as f64,
            },
            fix: "Decompose the task into sub-tasks each with ≤ 200-word descriptions. \
                  Emit task_decomposition_required with suggested split points."
                .into(),
            evidence: vec!["ADR-0013".into()],
            severity: Severity::Error,
        });

        // C-0010 — expected text output ≤ 2 000 chars
        store.upsert_constraint(Constraint {
            id: "C-0010".into(),
            description:
                "Sub-agent tasks expecting text output must not exceed 2000 estimated chars \
                 (ADR-0013)."
                    .into(),
            when: Condition::All {
                conditions: vec![
                    Condition::FieldEq {
                        field: "expected_output_type".into(),
                        value: json!("text"),
                    },
                    Condition::FieldGt {
                        field: "expected_output_chars".into(),
                        threshold: MAX_OUTPUT_CHARS as f64,
                    },
                ],
            },
            require: Condition::FieldLt {
                field: "expected_output_chars".into(),
                threshold: MAX_OUTPUT_CHARS as f64,
            },
            fix: "Decompose the task so each sub-task produces ≤ 2000 chars of text output. \
                  Emit task_decomposition_required with suggested split points."
                .into(),
            evidence: vec!["ADR-0013".into()],
            severity: Severity::Error,
        });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_praxis::db::{
        procedures::{evaluate, on_action},
        schema::{AgentContext, SessionType},
        store::PraxisStore,
    };

    // ── TaskSizeConstraint::check ─────────────────────────────────────────────

    #[test]
    fn check_199_words_is_ok() {
        let description = "word ".repeat(199);
        assert!(TaskSizeConstraint::check(&description, None, None).is_none());
    }

    #[test]
    fn check_200_words_is_ok() {
        let description = "word ".repeat(200);
        assert!(TaskSizeConstraint::check(&description, None, None).is_none());
    }

    #[test]
    fn check_201_words_is_violation() {
        let description = "word ".repeat(201);
        let v = TaskSizeConstraint::check(&description, None, None).expect("should violate");
        assert_eq!(v.word_count, 201);
        assert!(v.suggested_splits >= 2);
    }

    #[test]
    fn check_output_1999_chars_is_ok() {
        let content = "x".repeat(1999);
        assert!(TaskSizeConstraint::check("ok", Some("text"), Some(content.len())).is_none());
    }

    #[test]
    fn check_output_2000_chars_is_ok() {
        let content = "x".repeat(2000);
        assert!(TaskSizeConstraint::check("ok", Some("text"), Some(content.len())).is_none());
    }

    #[test]
    fn check_output_2001_chars_is_violation() {
        let content = "x".repeat(2001);
        let v = TaskSizeConstraint::check("ok", Some("text"), Some(content.len()))
            .expect("should violate");
        assert_eq!(v.output_chars, Some(2001));
        assert!(v.suggested_splits >= 2);
    }

    #[test]
    fn check_non_text_output_type_ignored() {
        // Even with huge char count, non-text output type must not trigger violation
        assert!(
            TaskSizeConstraint::check("ok", Some("binary"), Some(99_999)).is_none(),
            "binary output type should not trigger text-output constraint"
        );
    }

    #[test]
    fn check_both_violations_picks_larger_split() {
        let description = "word ".repeat(401); // → ceil(401/200) = 3
        let v = TaskSizeConstraint::check(&description, Some("text"), Some(4001))
            .expect("should violate");
        // ceil(4001/2000) = 3 — both are 3, result is max(3,3).max(2) = 3
        assert_eq!(v.suggested_splits, 3);
    }

    // ── TaskSizeViolation::into_event ─────────────────────────────────────────

    #[test]
    fn into_event_produces_correct_kind() {
        let description = "word ".repeat(201);
        let ev = TaskSizeConstraint::check(&description, None, None)
            .expect("should violate")
            .into_event();
        assert_eq!(ev.kind(), "task_decomposition_required");
    }

    // ── TaskSizeConstraint::register ─────────────────────────────────────────

    #[test]
    fn register_inserts_c0009_and_c0010() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        assert!(store.get_constraint("C-0009").is_some());
        assert!(store.get_constraint("C-0010").is_some());
    }

    #[test]
    fn c0009_fires_for_201_words_via_store() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("task_description_word_count", json!(201));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(ids.contains(&"C-0009"), "C-0009 should fire for 201 words; got: {ids:?}");
    }

    #[test]
    fn c0009_does_not_fire_for_200_words_via_store() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("task_description_word_count", json!(200));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(!ids.contains(&"C-0009"), "C-0009 must not fire for 200 words; got: {ids:?}");
    }

    #[test]
    fn c0010_fires_for_2001_chars_text_via_store() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("expected_output_type", json!("text"))
            .with_meta("expected_output_chars", json!(2001));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            ids.contains(&"C-0010"),
            "C-0010 should fire for 2001 chars; got: {ids:?}"
        );
    }

    #[test]
    fn c0010_does_not_fire_for_2000_chars_text_via_store() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("expected_output_type", json!("text"))
            .with_meta("expected_output_chars", json!(2000));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            !ids.contains(&"C-0010"),
            "C-0010 must not fire for 2000 chars; got: {ids:?}"
        );
    }

    #[test]
    fn c0010_does_not_fire_for_non_text_output_via_store() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("expected_output_type", json!("binary"))
            .with_meta("expected_output_chars", json!(99_999));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            !ids.contains(&"C-0010"),
            "C-0010 must not fire for non-text output; got: {ids:?}"
        );
    }

    #[test]
    fn on_action_blocks_oversized_task() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let ctx = AgentContext::new("dispatch_task", "agent-a", SessionType::SubAgent)
            .with_meta("task_description_word_count", json!(201));
        assert!(
            on_action(&store, &ctx).is_err(),
            "on_action should block tasks with >200 word descriptions"
        );
    }

    #[test]
    fn c0009_severity_is_error() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let c = store.get_constraint("C-0009").expect("C-0009 must be registered");
        assert_eq!(c.severity, Severity::Error);
    }

    #[test]
    fn c0010_severity_is_error() {
        let mut store = PraxisStore::new();
        TaskSizeConstraint::register(&mut store);
        let c = store.get_constraint("C-0010").expect("C-0010 must be registered");
        assert_eq!(c.severity, Severity::Error);
    }
}
