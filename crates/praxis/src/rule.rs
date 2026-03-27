//! Core rule primitives: [`Rule`], [`RuleResult`], [`RuleContext`], [`RuleCategory`].

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// RuleResult
// ---------------------------------------------------------------------------

/// Typed outcome returned by every [`Rule::evaluate`] call.
///
/// Mirrors the `RuleResult` type from `@plures/praxis` (v1.4.0) and extends it
/// with a [`Gate`](RuleResult::Gate) variant for approval-gate integration with
/// the existing [`praxis_ledger`] subsystem.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleResult {
    /// The rule is satisfied — the action may proceed.
    Pass,
    /// The rule is violated — the action must be blocked.
    Fail {
        /// Human-readable explanation of the failure.
        reason: String,
    },
    /// The rule is satisfied but a non-blocking concern was detected.
    Warning {
        /// Description of the concern.
        message: String,
    },
    /// The rule requires explicit human approval before the action may proceed.
    ///
    /// This variant bridges directly into the `praxis_ledger` gate flow.
    Gate {
        /// The action requiring approval.
        action: String,
        /// Rationale to present to the approver.
        rationale: String,
    },
}

impl RuleResult {
    /// Returns `true` when the result allows the action to proceed without
    /// human intervention (i.e. [`Pass`](RuleResult::Pass) or
    /// [`Warning`](RuleResult::Warning)).
    #[must_use]
    pub fn is_permitted(&self) -> bool {
        matches!(self, Self::Pass | Self::Warning { .. })
    }

    /// Returns `true` when the result blocks the action
    /// ([`Fail`](RuleResult::Fail) or [`Gate`](RuleResult::Gate)).
    #[must_use]
    pub fn is_blocking(&self) -> bool {
        matches!(self, Self::Fail { .. } | Self::Gate { .. })
    }
}

// ---------------------------------------------------------------------------
// RuleCategory
// ---------------------------------------------------------------------------

/// Factory category for a rule — controls which factory slot a rule occupies.
///
/// Mirrors the three factory rule types from the issue specification:
/// - `inputRules` → [`RuleCategory::Input`]
/// - `stateRules` → [`RuleCategory::State`]
/// - `dataRules`  → [`RuleCategory::Data`]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RuleCategory {
    /// Validates inbound requests and registration payloads before any state
    /// change is applied.
    Input,
    /// Governs agent lifecycle and task state-machine transitions.
    State,
    /// Computes scores, rankings, and matching weights used in routing and
    /// priority decisions.
    Data,
}

impl RuleCategory {
    /// Human-readable label used in completeness reports.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Input => "inputRules",
            Self::State => "stateRules",
            Self::Data => "dataRules",
        }
    }
}

// ---------------------------------------------------------------------------
// RuleContext
// ---------------------------------------------------------------------------

/// Immutable context bag passed to [`Rule::evaluate`].
///
/// Contains the action name, the primary JSON payload, and an optional map
/// of typed metadata that rules can inspect without deserialising the full
/// payload.
#[derive(Debug, Clone)]
pub struct RuleContext {
    /// Short identifier for the action being evaluated (e.g. `"spawn_agent"`,
    /// `"route_task"`, `"escalate"`).
    pub action: String,
    /// Primary payload for the rule.  Rules extract fields from this value as
    /// needed; unknown fields are silently ignored.
    pub payload: serde_json::Value,
    /// Auxiliary key/value metadata (e.g. `"agent_count"`, `"queue_depth"`).
    pub metadata: HashMap<String, serde_json::Value>,
}

impl RuleContext {
    /// Create a context with no metadata.
    pub fn new(action: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            action: action.into(),
            payload,
            metadata: HashMap::new(),
        }
    }

    /// Create a context with explicit metadata.
    pub fn with_metadata(
        action: impl Into<String>,
        payload: serde_json::Value,
        metadata: HashMap<String, serde_json::Value>,
    ) -> Self {
        Self {
            action: action.into(),
            payload,
            metadata,
        }
    }

    /// Return the value at `key` in the payload if it exists and is a `u64`.
    #[must_use]
    pub fn payload_u64(&self, key: &str) -> Option<u64> {
        self.payload.get(key)?.as_u64()
    }

    /// Return the value at `key` in the payload if it exists and is a `f64`.
    #[must_use]
    pub fn payload_f64(&self, key: &str) -> Option<f64> {
        self.payload.get(key)?.as_f64()
    }

    /// Return the value at `key` in the payload if it exists and is a `&str`.
    #[must_use]
    pub fn payload_str(&self, key: &str) -> Option<&str> {
        self.payload.get(key)?.as_str()
    }

    /// Return the length of the array at `key` in the payload, or `None` if
    /// the key is absent, the value is not an array, or the array is empty.
    #[must_use]
    pub fn payload_array_len(&self, key: &str) -> Option<usize> {
        let len = self.payload.get(key)?.as_array()?.len();
        if len == 0 {
            None
        } else {
            Some(len)
        }
    }
}

// ---------------------------------------------------------------------------
// Rule trait
// ---------------------------------------------------------------------------

/// A named, categorised decision rule.
///
/// Implement this trait for every discrete piece of business logic in the
/// agent mesh.  Rules must be stateless — all context is provided via
/// [`RuleContext`].
pub trait Rule: Send + Sync {
    /// Short, stable identifier for this rule (e.g. `"agent_id_format"`).
    fn name(&self) -> &str;

    /// The factory category this rule belongs to.
    fn category(&self) -> RuleCategory;

    /// Evaluate the rule against the given context and return a typed result.
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pass_is_permitted_not_blocking() {
        assert!(RuleResult::Pass.is_permitted());
        assert!(!RuleResult::Pass.is_blocking());
    }

    #[test]
    fn fail_is_blocking_not_permitted() {
        let r = RuleResult::Fail {
            reason: "bad".into(),
        };
        assert!(r.is_blocking());
        assert!(!r.is_permitted());
    }

    #[test]
    fn warning_is_permitted_not_blocking() {
        let r = RuleResult::Warning {
            message: "watch out".into(),
        };
        assert!(r.is_permitted());
        assert!(!r.is_blocking());
    }

    #[test]
    fn gate_is_blocking_not_permitted() {
        let r = RuleResult::Gate {
            action: "publish".into(),
            rationale: "public broadcast".into(),
        };
        assert!(r.is_blocking());
        assert!(!r.is_permitted());
    }

    #[test]
    fn rule_context_payload_helpers() {
        let ctx = RuleContext::new(
            "spawn_agent",
            json!({"max_agents": 10, "score": 0.8, "name": "alpha", "caps": [1, 2]}),
        );
        assert_eq!(ctx.payload_u64("max_agents"), Some(10));
        assert_eq!(ctx.payload_f64("score"), Some(0.8));
        assert_eq!(ctx.payload_str("name"), Some("alpha"));
        assert_eq!(ctx.payload_array_len("caps"), Some(2));
        assert!(ctx.payload_u64("missing").is_none());
    }

    #[test]
    fn payload_array_len_returns_none_for_empty_array() {
        let ctx = RuleContext::new("test", json!({"items": []}));
        assert!(ctx.payload_array_len("items").is_none());
    }

    #[test]
    fn payload_array_len_returns_none_for_missing_key() {
        let ctx = RuleContext::new("test", json!({}));
        assert!(ctx.payload_array_len("items").is_none());
    }

    #[test]
    fn rule_category_labels() {
        assert_eq!(RuleCategory::Input.label(), "inputRules");
        assert_eq!(RuleCategory::State.label(), "stateRules");
        assert_eq!(RuleCategory::Data.label(), "dataRules");
    }

    #[test]
    fn rule_result_serde_roundtrip() {
        let cases = vec![
            RuleResult::Pass,
            RuleResult::Fail {
                reason: "oops".into(),
            },
            RuleResult::Warning {
                message: "heads up".into(),
            },
            RuleResult::Gate {
                action: "delete".into(),
                rationale: "irreversible".into(),
            },
        ];
        for r in cases {
            let json = serde_json::to_string(&r).unwrap();
            let back: RuleResult = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }
}
