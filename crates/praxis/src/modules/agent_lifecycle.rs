//! `agent-lifecycle` Praxis module.
//!
//! Governs agent spawn/retire decisions, health validation, and capability
//! registration.
//!
//! ## Input rules
//! - `agent_id_format` — agent ID must be non-empty and ≤ 128 chars
//! - `capability_list_non_empty` — agent must declare at least one capability
//! - `version_present` — agent registration payload must include a `version`
//!   field
//!
//! ## State rules
//! - `lifecycle_transition_valid` — transitions must follow the allowed state
//!   machine (new → active → suspended → retired)
//!
//! ## Data rules
//! - `health_score_sufficient` — `health_score` must be ≥ 0.5 to remain active
//! - `capability_count_reasonable` — warn when an agent registers > 50
//!   capabilities (possible misconfiguration)

use crate::module::PraxisModule;
use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// Allowed lifecycle transitions
// ---------------------------------------------------------------------------

/// Agent lifecycle states.
const VALID_TRANSITIONS: &[(&str, &str)] = &[
    ("new", "active"),
    ("active", "suspended"),
    ("active", "retired"),
    ("suspended", "active"),
    ("suspended", "retired"),
];

fn transition_allowed(from: &str, to: &str) -> bool {
    VALID_TRANSITIONS
        .iter()
        .any(|(f, t)| *f == from && *t == to)
}

// ---------------------------------------------------------------------------
// Input rules
// ---------------------------------------------------------------------------

struct AgentIdFormat;
impl Rule for AgentIdFormat {
    fn name(&self) -> &str {
        "agent_id_format"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_str("agent_id") {
            None | Some("") => RuleResult::Fail {
                reason: "agent_id is required and must not be empty".into(),
            },
            Some(id) if id.len() > 128 => RuleResult::Fail {
                reason: format!("agent_id exceeds 128 chars (got {})", id.len()),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct CapabilityListNonEmpty;
impl Rule for CapabilityListNonEmpty {
    fn name(&self) -> &str {
        "capability_list_non_empty"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_array_len("capabilities") {
            None => RuleResult::Fail {
                reason: "agent must declare at least one capability".into(),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct VersionPresent;
impl Rule for VersionPresent {
    fn name(&self) -> &str {
        "version_present"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_str("version") {
            None | Some("") => RuleResult::Fail {
                reason: "agent registration must include a non-empty `version` field".into(),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// State rules
// ---------------------------------------------------------------------------

struct LifecycleTransitionValid;
impl Rule for LifecycleTransitionValid {
    fn name(&self) -> &str {
        "lifecycle_transition_valid"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let from = ctx.payload_str("from_state").unwrap_or("");
        let to = ctx.payload_str("to_state").unwrap_or("");
        if from.is_empty() || to.is_empty() {
            return RuleResult::Fail {
                reason: "lifecycle transition requires `from_state` and `to_state`".into(),
            };
        }
        if transition_allowed(from, to) {
            RuleResult::Pass
        } else {
            RuleResult::Fail {
                reason: format!("transition `{from}` → `{to}` is not permitted"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Data rules
// ---------------------------------------------------------------------------

struct HealthScoreSufficient;
impl Rule for HealthScoreSufficient {
    fn name(&self) -> &str {
        "health_score_sufficient"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_f64("health_score") {
            None => RuleResult::Warning {
                message: "health_score not provided; assuming healthy".into(),
            },
            Some(score) if score < 0.5 => RuleResult::Fail {
                reason: format!(
                    "health_score {score:.2} is below the 0.5 threshold; agent should be retired"
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct CapabilityCountReasonable;
impl Rule for CapabilityCountReasonable {
    fn name(&self) -> &str {
        "capability_count_reasonable"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_array_len("capabilities") {
            Some(n) if n > 50 => RuleResult::Warning {
                message: format!(
                    "agent declares {n} capabilities (> 50); verify this is intentional"
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// AgentLifecycleModule
// ---------------------------------------------------------------------------

/// Praxis module for agent lifecycle management.
///
/// See the [module docs](self) for the full rule list.
pub struct AgentLifecycleModule {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for AgentLifecycleModule {
    fn default() -> Self {
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(AgentIdFormat),
            Box::new(CapabilityListNonEmpty),
            Box::new(VersionPresent),
            Box::new(LifecycleTransitionValid),
            Box::new(HealthScoreSufficient),
            Box::new(CapabilityCountReasonable),
        ];
        Self { rules }
    }
}

impl PraxisModule for AgentLifecycleModule {
    fn name(&self) -> &str {
        "agent-lifecycle"
    }

    fn rules(&self) -> &[Box<dyn Rule>] {
        &self.rules
    }

    fn expectations(&self) -> Vec<String> {
        vec![
            "Each agent has a unique, stable agent_id before registration.".into(),
            "Agents declare all capabilities at registration time.".into(),
            "Lifecycle state transitions are initiated by trusted orchestration code.".into(),
            "Health scores are computed externally and passed in the evaluation payload.".into(),
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn module() -> AgentLifecycleModule {
        AgentLifecycleModule::default()
    }

    // ── Input: agent_id_format ───────────────────────────────────────────────
    #[test]
    fn agent_id_format_passes_valid_id() {
        let m = module();
        let ctx = RuleContext::new("register_agent", json!({"agent_id": "alpha-01"}));
        let results = m.evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "agent_id_format")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn agent_id_format_fails_missing_id() {
        let m = module();
        let ctx = RuleContext::new("register_agent", json!({}));
        let results = m.evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "agent_id_format")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn agent_id_format_fails_too_long() {
        let m = module();
        let long_id = "x".repeat(129);
        let ctx = RuleContext::new("register_agent", json!({"agent_id": long_id}));
        let results = m.evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "agent_id_format")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Input: capability_list_non_empty ─────────────────────────────────────
    #[test]
    fn capability_list_non_empty_passes_with_caps() {
        let m = module();
        let ctx = RuleContext::new("register_agent", json!({"capabilities": ["text", "image"]}));
        let results = m.evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "capability_list_non_empty")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn capability_list_non_empty_fails_empty() {
        let m = module();
        let ctx = RuleContext::new("register_agent", json!({"capabilities": []}));
        let results = m.evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "capability_list_non_empty")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── State: lifecycle_transition_valid ────────────────────────────────────
    #[test]
    fn lifecycle_transition_valid_allows_new_to_active() {
        let m = module();
        let ctx = RuleContext::new(
            "transition",
            json!({"from_state": "new", "to_state": "active"}),
        );
        let results = m.evaluate_category(&ctx, RuleCategory::State);
        assert_eq!(results[0].1, RuleResult::Pass);
    }

    #[test]
    fn lifecycle_transition_blocks_invalid_transition() {
        let m = module();
        let ctx = RuleContext::new(
            "transition",
            json!({"from_state": "new", "to_state": "retired"}),
        );
        let results = m.evaluate_category(&ctx, RuleCategory::State);
        assert!(matches!(results[0].1, RuleResult::Fail { .. }));
    }

    // ── Data: health_score_sufficient ────────────────────────────────────────
    #[test]
    fn health_score_passes_above_threshold() {
        let m = module();
        let ctx = RuleContext::new("health_check", json!({"health_score": 0.9}));
        let results = m.evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "health_score_sufficient")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn health_score_fails_below_threshold() {
        let m = module();
        let ctx = RuleContext::new("health_check", json!({"health_score": 0.3}));
        let results = m.evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "health_score_sufficient")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn health_score_warns_when_absent() {
        let m = module();
        let ctx = RuleContext::new("health_check", json!({}));
        let results = m.evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "health_score_sufficient")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Warning { .. }));
    }

    // ── Audit ─────────────────────────────────────────────────────────────────
    #[test]
    fn module_audit_is_complete() {
        let report = module().audit();
        assert!(
            report.is_complete(),
            "agent-lifecycle should cover all categories"
        );
        assert_eq!(report.completeness_pct, 100.0);
    }

    #[test]
    fn module_has_expectations() {
        assert!(!module().expectations().is_empty());
    }
}
