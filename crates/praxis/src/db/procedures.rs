//! Praxis procedures evaluated against [`PraxisStore`].
//!
//! These are the four procedures specified in the issue:
//!
//! | Procedure | Description |
//! |-----------|-------------|
//! | [`evaluate`] | Returns all [`Constraint`]s violated by an [`AgentContext`] |
//! | [`on_action`] | Pre-action hook — returns `Err` when a blocking constraint fires |
//! | [`compile_nl`] | Compiles natural-language text into a [`Constraint`] insert |
//! | [`query_gaps`] | Returns [`Evidence`] records whose `result` is `Unknown` |

use crate::db::schema::{AgentContext, Condition, Constraint, Evidence, EvidenceResult, Severity};
use crate::db::store::PraxisStore;

// ---------------------------------------------------------------------------
// Violation
// ---------------------------------------------------------------------------

/// A constraint that was violated by a given [`AgentContext`].
#[derive(Debug, Clone)]
pub struct Violation {
    /// The violated constraint.
    pub constraint: Constraint,
    /// Human-readable explanation (includes the constraint's `fix` instruction).
    pub message: String,
}

// ---------------------------------------------------------------------------
// ProcedureError
// ---------------------------------------------------------------------------

/// Error type returned by [`on_action`] when the action is blocked.
#[derive(Debug, Clone)]
pub struct ActionBlocked {
    /// All violations that caused the block.
    pub violations: Vec<Violation>,
}

impl std::fmt::Display for ActionBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "action blocked by {} constraint(s):", self.violations.len())?;
        for v in &self.violations {
            write!(f, "\n  [{}] {} — fix: {}", v.constraint.id, v.message, v.constraint.fix)?;
        }
        Ok(())
    }
}

impl std::error::Error for ActionBlocked {}

// ---------------------------------------------------------------------------
// evaluate
// ---------------------------------------------------------------------------

/// Return all constraints whose precondition (`when`) holds for `ctx` but
/// whose invariant (`require`) does **not** hold.
///
/// Constraints are checked in an unspecified order.  All violations are
/// returned, not just the first.
///
/// # Example
///
/// ```rust
/// use pares_agens_praxis::db::{AgentContext, SessionType};
/// use pares_agens_praxis::db::procedures::evaluate;
/// use pares_agens_praxis::db::seed::default_store;
/// use serde_json::json;
///
/// let store = default_store();
/// let ctx = AgentContext::new("write_file", "config.toml", SessionType::Main)
///     .with_meta("resource_owner", json!(""));
/// let violations = evaluate(&store, &ctx);
/// // resource_owner_declared constraint fires because owner is empty
/// assert!(!violations.is_empty());
/// ```
pub fn evaluate(store: &PraxisStore, ctx: &AgentContext) -> Vec<Violation> {
    store
        .constraints()
        .filter(|c| {
            // when holds AND require does NOT hold
            c.when.evaluate(ctx) && !c.require.evaluate(ctx)
        })
        .map(|c| {
            let message = format!(
                "constraint {} violated for action `{}`",
                c.id, ctx.action_type
            );
            Violation {
                constraint: c.clone(),
                message,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// on_action
// ---------------------------------------------------------------------------

/// Pre-action hook that blocks when any **error-severity** constraint is violated.
///
/// Warnings are collected and returned separately so the caller can log them
/// without blocking.
///
/// Returns `Ok(warnings)` when the action may proceed (zero or only warning
/// violations), or `Err(ActionBlocked)` when at least one `Error`-severity
/// constraint fires.
///
/// # Example
///
/// ```rust
/// use pares_agens_praxis::db::{AgentContext, SessionType};
/// use pares_agens_praxis::db::procedures::on_action;
/// use pares_agens_praxis::db::seed::default_store;
/// use serde_json::json;
///
/// let store = default_store();
/// let ctx = AgentContext::new("read_file", "README.md", SessionType::Main);
/// assert!(on_action(&store, &ctx).is_ok());
/// ```
pub fn on_action(
    store: &PraxisStore,
    ctx: &AgentContext,
) -> Result<Vec<Violation>, ActionBlocked> {
    let all = evaluate(store, ctx);
    let (errors, warnings): (Vec<_>, Vec<_>) = all
        .into_iter()
        .partition(|v| v.constraint.severity == Severity::Error);

    if errors.is_empty() {
        Ok(warnings)
    } else {
        Err(ActionBlocked { violations: errors })
    }
}

// ---------------------------------------------------------------------------
// compile_nl
// ---------------------------------------------------------------------------

/// Compile a natural-language constraint description into a [`Constraint`]
/// record ready to be inserted into the store.
///
/// This is a best-effort heuristic parser.  It understands a small set of
/// keyword patterns:
///
/// | Pattern in text | Generated condition |
/// |-----------------|---------------------|
/// | `"write_"` in text | `when: ActionStartsWith { prefix: "write_" }` |
/// | `"delete_"` in text | `when: ActionStartsWith { prefix: "delete_" }` |
/// | `"resource_owner"` in text | `require: FieldEq { field: "resource_owner", value: non-empty }` |
/// | `"privilege_level"` in text | `require: FieldLt { field: "privilege_level", threshold: 3.0 }` |
/// | `"risk_score"` in text | `require: FieldLt { field: "risk_score", threshold: 0.9 }` |
/// | anything else | `when: Always, require: Always` (always satisfied — no-op) |
///
/// The `id` is derived from the first non-space token in `text`.  The `fix`
/// field is set to `"Review the constraint text and implement the appropriate check."`.
///
/// In production this would call the PluresDB `compile_nl` procedure backed by
/// an embedded LLM; here we ship a deterministic fallback so the crate compiles
/// without any ML dependency.
pub fn compile_nl(text: &str, id: impl Into<String>) -> Constraint {
    let lower = text.to_lowercase();

    let when = if lower.contains("write_") {
        Condition::ActionStartsWith { prefix: "write_".into() }
    } else if lower.contains("delete_") {
        Condition::ActionStartsWith { prefix: "delete_".into() }
    } else {
        Condition::Always
    };

    let require = if lower.contains("resource_owner") {
        // require that resource_owner is a non-empty string
        Condition::Not {
            condition: Box::new(Condition::FieldEq {
                field: "resource_owner".into(),
                value: serde_json::Value::String(String::new()),
            }),
        }
    } else if lower.contains("privilege_level") {
        Condition::FieldLt {
            field: "privilege_level".into(),
            threshold: 3.0,
        }
    } else if lower.contains("risk_score") {
        Condition::FieldLt {
            field: "risk_score".into(),
            threshold: 0.9,
        }
    } else {
        Condition::Always
    };

    let severity = if lower.contains("error") || lower.contains("block") || lower.contains("must") {
        Severity::Error
    } else {
        Severity::Warning
    };

    Constraint {
        id: id.into(),
        description: text.trim().to_string(),
        when,
        require,
        fix: "Review the constraint text and implement the appropriate check.".into(),
        evidence: vec![],
        severity,
    }
}

// ---------------------------------------------------------------------------
// query_gaps
// ---------------------------------------------------------------------------

/// Return all [`Evidence`] records whose `result` is [`EvidenceResult::Unknown`].
///
/// These represent "gaps" — constraints or decisions that have not yet been
/// validated by a concrete test run.
pub fn query_gaps(store: &PraxisStore) -> Vec<&Evidence> {
    store
        .evidence_records()
        .filter(|e| e.result == EvidenceResult::Unknown)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::{EvidenceResult, SessionType};
    use crate::db::seed::default_store;
    use chrono::Utc;
    use serde_json::json;
    use std::collections::HashMap;

    // ── evaluate ─────────────────────────────────────────────────────────────

    #[test]
    fn evaluate_clean_context_returns_no_violations() {
        let store = default_store();
        let ctx = AgentContext::new("read_file", "README.md", SessionType::Main);
        let violations = evaluate(&store, &ctx);
        assert!(
            violations.is_empty(),
            "read_file with no metadata should not violate any constraint; got: {:?}",
            violations.iter().map(|v| &v.constraint.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn evaluate_missing_resource_owner_on_write() {
        let store = default_store();
        let ctx = AgentContext::new("write_file", "config.toml", SessionType::Main)
            .with_meta("resource_owner", json!(""));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            ids.contains(&"C-0002"),
            "C-0002 (resource_owner_declared) should fire; got: {ids:?}"
        );
    }

    #[test]
    fn evaluate_high_privilege_triggers_violation() {
        let store = default_store();
        let ctx = AgentContext::new("admin_action", "system", SessionType::Main)
            .with_meta("privilege_level", json!(4));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            ids.contains(&"C-0003"),
            "C-0003 (privilege escalation) should fire; got: {ids:?}"
        );
    }

    #[test]
    fn evaluate_extreme_risk_score_triggers_violation() {
        let store = default_store();
        let ctx = AgentContext::new("deploy", "production", SessionType::Main)
            .with_meta("risk_score", json!(0.95));
        let violations = evaluate(&store, &ctx);
        let ids: Vec<&str> = violations.iter().map(|v| v.constraint.id.as_str()).collect();
        assert!(
            ids.contains(&"C-0004"),
            "C-0004 (risk score) should fire; got: {ids:?}"
        );
    }

    // ── on_action ────────────────────────────────────────────────────────────

    #[test]
    fn on_action_permits_safe_read() {
        let store = default_store();
        let ctx = AgentContext::new("read_config", "settings.toml", SessionType::Main);
        assert!(on_action(&store, &ctx).is_ok());
    }

    #[test]
    fn on_action_blocks_write_without_owner() {
        let store = default_store();
        let ctx = AgentContext::new("write_file", "config.toml", SessionType::Main)
            .with_meta("resource_owner", json!(""));
        let result = on_action(&store, &ctx);
        assert!(result.is_err(), "should be blocked");
        let blocked = result.unwrap_err();
        assert!(!blocked.violations.is_empty());
    }

    #[test]
    fn action_blocked_display_includes_fix() {
        let store = default_store();
        let ctx = AgentContext::new("write_file", "x", SessionType::Main)
            .with_meta("resource_owner", json!(""));
        let err = on_action(&store, &ctx).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("blocked"), "display should mention blocked");
        assert!(msg.contains("fix:"), "display should include fix");
    }

    // ── compile_nl ───────────────────────────────────────────────────────────

    #[test]
    fn compile_nl_write_action_pattern() {
        let c = compile_nl(
            "write_ actions must declare a resource_owner field.",
            "C-TEST",
        );
        assert_eq!(c.id, "C-TEST");
        assert_eq!(c.severity, Severity::Error);
        assert!(matches!(c.when, Condition::ActionStartsWith { .. }));
    }

    #[test]
    fn compile_nl_privilege_pattern() {
        let c = compile_nl("privilege_level should stay below 3", "C-PRIV");
        assert!(matches!(c.require, Condition::FieldLt { .. }));
    }

    #[test]
    fn compile_nl_unknown_text_produces_always_pass() {
        let c = compile_nl("something entirely unrecognised", "C-UNK");
        assert!(matches!(c.when, Condition::Always));
        assert!(matches!(c.require, Condition::Always));
    }

    // ── query_gaps ───────────────────────────────────────────────────────────

    #[test]
    fn query_gaps_returns_unknown_evidence() {
        let mut store = default_store();
        store.insert_evidence(Evidence {
            id: "EV-GAP".into(),
            tested_at: Utc::now(),
            condition: HashMap::new(),
            result: EvidenceResult::Unknown,
            reference: "https://github.com/plures/pares-agens/issues/999".into(),
        }).unwrap();

        let gaps = query_gaps(&store);
        let ids: Vec<&str> = gaps.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"EV-GAP"));
    }

    #[test]
    fn query_gaps_excludes_passed_evidence() {
        let store = default_store();
        // default_store seeds passed evidence; ensure gaps don't include it
        let gaps = query_gaps(&store);
        for ev in &gaps {
            assert_eq!(ev.result, EvidenceResult::Unknown);
        }
    }

    #[test]
    fn query_gaps_returns_adr0004_gap_evidence() {
        let store = default_store();
        let gaps = query_gaps(&store);
        // ADR-0004 seeds an EvidenceResult::Unknown record for CI validation
        let ids: Vec<&str> = gaps.iter().map(|e| e.id.as_str()).collect();
        assert!(
            ids.contains(&"EV-ADR0004-CI"),
            "EV-ADR0004-CI (unknown) should appear in gaps; got: {ids:?}"
        );
    }
}
