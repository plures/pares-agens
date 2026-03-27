//! `safety` Praxis module — **highest-impact module, implemented first**.
//!
//! Governs agent action boundaries, resource limits, and escalation triggers
//! in the Pares mesh.  Every potentially destructive or high-privilege action
//! must pass these rules before execution.
//!
//! ## Input rules
//! - `action_name_present` — every evaluation must carry a non-empty
//!   `action` field
//! - `resource_owner_declared` — the `resource_owner` must be set for
//!   resource-mutation actions
//!
//! ## State rules
//! - `escalation_required_for_high_privilege` — actions with
//!   `privilege_level` ≥ 3 require a [`RuleResult::Gate`] (escalation)
//! - `resource_limit_not_exceeded` — `requested_units` must not exceed
//!   `resource_limit`
//!
//! ## Data rules
//! - `risk_score_within_bounds` — block when `risk_score` > 0.9; gate when
//!   > 0.7; warn when > 0.5
//! - `rate_limit_not_exceeded` — warn when `calls_per_minute` > 60 (default
//!   rate limit)

use crate::module::PraxisModule;
use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// Resource-mutation action prefixes (require `resource_owner`)
// ---------------------------------------------------------------------------

const RESOURCE_MUTATION_PREFIXES: &[&str] = &[
    "write_", "delete_", "update_", "create_", "publish_", "send_", "post_",
];

fn is_resource_mutation(action: &str) -> bool {
    RESOURCE_MUTATION_PREFIXES
        .iter()
        .any(|p| action.starts_with(p))
}

// ---------------------------------------------------------------------------
// Input rules
// ---------------------------------------------------------------------------

struct ActionNamePresent;
impl Rule for ActionNamePresent {
    fn name(&self) -> &str {
        "action_name_present"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        // Primary source: ctx.action; fallback to a payload "action" field so
        // callers that embed action in the payload are also supported.
        let has_action = !ctx.action.is_empty()
            || ctx
                .payload_str("action")
                .is_some_and(|v| !v.trim().is_empty());
        if has_action {
            RuleResult::Pass
        } else {
            RuleResult::Fail {
                reason: "action name must not be empty".into(),
            }
        }
    }
}

struct ResourceOwnerDeclared;
impl Rule for ResourceOwnerDeclared {
    fn name(&self) -> &str {
        "resource_owner_declared"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        if !is_resource_mutation(&ctx.action) {
            return RuleResult::Pass; // non-mutation actions don't need an owner
        }
        match ctx.payload_str("resource_owner") {
            None | Some("") => RuleResult::Fail {
                reason: format!(
                    "action `{}` mutates a resource but `resource_owner` is not declared",
                    ctx.action
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// State rules
// ---------------------------------------------------------------------------

struct EscalationRequiredForHighPrivilege;
impl Rule for EscalationRequiredForHighPrivilege {
    fn name(&self) -> &str {
        "escalation_required_for_high_privilege"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_u64("privilege_level") {
            Some(level) if level >= 3 => RuleResult::Gate {
                action: ctx.action.clone(),
                rationale: format!("privilege_level {level} ≥ 3 requires explicit user approval"),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct ResourceLimitNotExceeded;
impl Rule for ResourceLimitNotExceeded {
    fn name(&self) -> &str {
        "resource_limit_not_exceeded"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let requested = ctx.payload_u64("requested_units");
        let limit = ctx.payload_u64("resource_limit");
        match (requested, limit) {
            (Some(req), Some(lim)) if req > lim => RuleResult::Fail {
                reason: format!("requested {req} units but resource_limit is {lim}"),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// Data rules
// ---------------------------------------------------------------------------

struct RiskScoreWithinBounds;
impl Rule for RiskScoreWithinBounds {
    fn name(&self) -> &str {
        "risk_score_within_bounds"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_f64("risk_score") {
            None => RuleResult::Pass, // no risk assessment → no constraint
            Some(s) if s > 0.9 => RuleResult::Fail {
                reason: format!(
                    "risk_score {s:.2} exceeds 0.9 — action blocked until risk is mitigated"
                ),
            },
            Some(s) if s > 0.7 => RuleResult::Gate {
                action: ctx.action.clone(),
                rationale: format!("risk_score {s:.2} > 0.7 — requires explicit approval"),
            },
            Some(s) if s > 0.5 => RuleResult::Warning {
                message: format!("risk_score {s:.2} is elevated (> 0.5); review before proceeding"),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct RateLimitNotExceeded;
impl Rule for RateLimitNotExceeded {
    fn name(&self) -> &str {
        "rate_limit_not_exceeded"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        const DEFAULT_LIMIT: u64 = 60;
        match ctx.payload_u64("calls_per_minute") {
            Some(rate) if rate > DEFAULT_LIMIT => RuleResult::Warning {
                message: format!(
                    "calls_per_minute {rate} exceeds default limit of {DEFAULT_LIMIT}; \
                     consider throttling"
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// SafetyModule
// ---------------------------------------------------------------------------

/// Praxis module for agent safety constraints.
///
/// This is the highest-impact module — implement and evaluate it first before
/// any other module.
///
/// See the [module docs](self) for the full rule list.
pub struct SafetyModule {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for SafetyModule {
    fn default() -> Self {
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(ActionNamePresent),
            Box::new(ResourceOwnerDeclared),
            Box::new(EscalationRequiredForHighPrivilege),
            Box::new(ResourceLimitNotExceeded),
            Box::new(RiskScoreWithinBounds),
            Box::new(RateLimitNotExceeded),
        ];
        Self { rules }
    }
}

impl PraxisModule for SafetyModule {
    fn name(&self) -> &str {
        "safety"
    }

    fn rules(&self) -> &[Box<dyn Rule>] {
        &self.rules
    }

    fn expectations(&self) -> Vec<String> {
        vec![
            "Every agent action passes through safety evaluation before execution.".into(),
            "Privilege levels are assigned by the orchestration layer, not self-reported.".into(),
            "Resource limits are configured per-deployment and injected into the payload.".into(),
            "Risk scores are computed by an independent risk-assessment subsystem.".into(),
            "Rate limit enforcement is a last-resort signal — prefer circuit breakers upstream."
                .into(),
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

    fn module() -> SafetyModule {
        SafetyModule::default()
    }

    // ── Input: action_name_present ───────────────────────────────────────────
    #[test]
    fn action_name_present_passes() {
        let ctx = RuleContext::new("send_email", json!({}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "action_name_present")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn action_name_present_fails_empty_action() {
        let ctx = RuleContext::new("", json!({}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "action_name_present")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Input: resource_owner_declared ───────────────────────────────────────
    #[test]
    fn resource_owner_required_for_write_action() {
        let ctx = RuleContext::new("write_file", json!({}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "resource_owner_declared")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn resource_owner_passes_for_non_mutation_action() {
        let ctx = RuleContext::new("read_file", json!({}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "resource_owner_declared")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn resource_owner_passes_when_set_for_write_action() {
        let ctx = RuleContext::new("write_file", json!({"resource_owner": "user-123"}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "resource_owner_declared")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    // ── State: escalation_required_for_high_privilege ────────────────────────
    #[test]
    fn high_privilege_triggers_gate() {
        let ctx = RuleContext::new("admin_action", json!({"privilege_level": 3}));
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "escalation_required_for_high_privilege")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Gate { .. }));
    }

    #[test]
    fn low_privilege_passes() {
        let ctx = RuleContext::new("read_action", json!({"privilege_level": 1}));
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "escalation_required_for_high_privilege")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    // ── State: resource_limit_not_exceeded ───────────────────────────────────
    #[test]
    fn resource_limit_passes_under_limit() {
        let ctx = RuleContext::new(
            "compute",
            json!({"requested_units": 50, "resource_limit": 100}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "resource_limit_not_exceeded")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn resource_limit_fails_over_limit() {
        let ctx = RuleContext::new(
            "compute",
            json!({"requested_units": 150, "resource_limit": 100}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "resource_limit_not_exceeded")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Data: risk_score_within_bounds ───────────────────────────────────────
    #[test]
    fn risk_score_above_0_9_blocks() {
        let ctx = RuleContext::new("deploy", json!({"risk_score": 0.95}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "risk_score_within_bounds")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn risk_score_above_0_7_gates() {
        let ctx = RuleContext::new("deploy", json!({"risk_score": 0.8}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "risk_score_within_bounds")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Gate { .. }));
    }

    #[test]
    fn risk_score_above_0_5_warns() {
        let rule = RiskScoreWithinBounds;
        let ctx = RuleContext::new("deploy", json!({"risk_score": 0.6}));
        let result = rule.evaluate(&ctx);
        assert!(matches!(result, RuleResult::Warning { .. }));
    }

    #[test]
    fn risk_score_low_passes() {
        let rule = RiskScoreWithinBounds;
        let ctx = RuleContext::new("deploy", json!({"risk_score": 0.2}));
        assert_eq!(rule.evaluate(&ctx), RuleResult::Pass);
    }

    // ── Data: rate_limit_not_exceeded ────────────────────────────────────────
    #[test]
    fn rate_limit_warns_above_60() {
        let rule = RateLimitNotExceeded;
        let ctx = RuleContext::new("call", json!({"calls_per_minute": 100}));
        assert!(matches!(rule.evaluate(&ctx), RuleResult::Warning { .. }));
    }

    #[test]
    fn rate_limit_passes_under_60() {
        let rule = RateLimitNotExceeded;
        let ctx = RuleContext::new("call", json!({"calls_per_minute": 30}));
        assert_eq!(rule.evaluate(&ctx), RuleResult::Pass);
    }

    // ── Audit ─────────────────────────────────────────────────────────────────
    #[test]
    fn module_audit_is_complete() {
        let report = module().audit();
        assert!(report.is_complete(), "safety should cover all categories");
        assert_eq!(report.completeness_pct, 100.0);
    }

    #[test]
    fn module_has_expectations() {
        assert!(!module().expectations().is_empty());
    }

    // ── is_resource_mutation helper ───────────────────────────────────────────
    #[test]
    fn resource_mutation_prefixes_detected() {
        assert!(is_resource_mutation("write_config"));
        assert!(is_resource_mutation("delete_user"));
        assert!(is_resource_mutation("publish_post"));
        assert!(!is_resource_mutation("read_config"));
        assert!(!is_resource_mutation("list_agents"));
    }
}
