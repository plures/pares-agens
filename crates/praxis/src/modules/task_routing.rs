//! `task-routing` Praxis module.
//!
//! Governs task assignment, priority calculation, and load-balancing
//! decisions in the agent mesh.
//!
//! ## Input rules
//! - `task_id_present` — every task must carry a non-empty `task_id`
//! - `task_type_known` — `task_type` must be one of the recognised types
//! - `required_capability_declared` — task must declare at least one required
//!   capability so the router can match agents
//!
//! ## State rules
//! - `task_state_transition_valid` — task transitions must follow the state
//!   machine (queued → assigned → in_progress → complete | failed)
//! - `assigned_agent_present_when_in_progress` — `assigned_agent_id` must be
//!   set when a task enters the `in_progress` state
//!
//! ## Data rules
//! - `priority_in_range` — `priority` must be an integer 1 – 10 (1 = highest)
//! - `load_balance_headroom` — warn when `queue_depth` exceeds 80 % of
//!   `agent_capacity` (load-balancing trigger)

use crate::module::PraxisModule;
use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// Known task types
// ---------------------------------------------------------------------------

const KNOWN_TASK_TYPES: &[&str] = &[
    "inference",
    "retrieval",
    "summarisation",
    "code_generation",
    "tool_call",
    "evaluation",
    "coordination",
];

// ---------------------------------------------------------------------------
// Allowed task state transitions
// ---------------------------------------------------------------------------

const VALID_TASK_TRANSITIONS: &[(&str, &str)] = &[
    ("queued", "assigned"),
    ("assigned", "in_progress"),
    ("assigned", "queued"), // re-queue on agent retirement
    ("in_progress", "complete"),
    ("in_progress", "failed"),
    ("failed", "queued"), // retry
];

fn task_transition_allowed(from: &str, to: &str) -> bool {
    VALID_TASK_TRANSITIONS
        .iter()
        .any(|(f, t)| *f == from && *t == to)
}

// ---------------------------------------------------------------------------
// Input rules
// ---------------------------------------------------------------------------

struct TaskIdPresent;
impl Rule for TaskIdPresent {
    fn name(&self) -> &str {
        "task_id_present"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_str("task_id") {
            None | Some("") => RuleResult::Fail {
                reason: "task_id is required and must not be empty".into(),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct TaskTypeKnown;
impl Rule for TaskTypeKnown {
    fn name(&self) -> &str {
        "task_type_known"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_str("task_type") {
            None | Some("") => RuleResult::Fail {
                reason: "task_type is required".into(),
            },
            Some(t) if !KNOWN_TASK_TYPES.contains(&t) => RuleResult::Fail {
                reason: format!(
                    "unknown task_type `{t}`; expected one of: {}",
                    KNOWN_TASK_TYPES.join(", ")
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct RequiredCapabilityDeclared;
impl Rule for RequiredCapabilityDeclared {
    fn name(&self) -> &str {
        "required_capability_declared"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_array_len("required_capabilities") {
            None => RuleResult::Fail {
                reason: "task must declare at least one required capability".into(),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// State rules
// ---------------------------------------------------------------------------

struct TaskStateTransitionValid;
impl Rule for TaskStateTransitionValid {
    fn name(&self) -> &str {
        "task_state_transition_valid"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let from = ctx.payload_str("from_state").unwrap_or("");
        let to = ctx.payload_str("to_state").unwrap_or("");
        if from.is_empty() || to.is_empty() {
            return RuleResult::Fail {
                reason: "task transition requires `from_state` and `to_state`".into(),
            };
        }
        if task_transition_allowed(from, to) {
            RuleResult::Pass
        } else {
            RuleResult::Fail {
                reason: format!("task transition `{from}` → `{to}` is not permitted"),
            }
        }
    }
}

struct AssignedAgentPresentWhenInProgress;
impl Rule for AssignedAgentPresentWhenInProgress {
    fn name(&self) -> &str {
        "assigned_agent_present_when_in_progress"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let to_state = ctx.payload_str("to_state").unwrap_or("");
        if to_state != "in_progress" {
            return RuleResult::Pass; // rule only applies on this specific transition
        }
        match ctx.payload_str("assigned_agent_id") {
            None | Some("") => RuleResult::Fail {
                reason: "assigned_agent_id must be set when transitioning to in_progress".into(),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// Data rules
// ---------------------------------------------------------------------------

struct PriorityInRange;
impl Rule for PriorityInRange {
    fn name(&self) -> &str {
        "priority_in_range"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_u64("priority") {
            None => RuleResult::Warning {
                message:
                    "priority not set; caller should provide a value in [1, 10] (e.g., 5 = medium)"
                        .into(),
            },
            Some(p) if !(1..=10).contains(&p) => RuleResult::Fail {
                reason: format!("priority {p} is out of range [1, 10]"),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct LoadBalanceHeadroom;
impl Rule for LoadBalanceHeadroom {
    fn name(&self) -> &str {
        "load_balance_headroom"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let queue = ctx.payload_u64("queue_depth").unwrap_or(0) as f64;
        let cap = ctx.payload_u64("agent_capacity").unwrap_or(0) as f64;
        if cap == 0.0 {
            return RuleResult::Warning {
                message: "agent_capacity not provided; load-balance check skipped".into(),
            };
        }
        let utilisation = queue / cap;
        if utilisation >= 0.8 {
            RuleResult::Warning {
                message: format!(
                    "queue utilisation {:.0}% ≥ 80%; consider spawning additional agents",
                    utilisation * 100.0
                ),
            }
        } else {
            RuleResult::Pass
        }
    }
}

// ---------------------------------------------------------------------------
// TaskRoutingModule
// ---------------------------------------------------------------------------

/// Praxis module for task routing and load balancing.
///
/// See the [module docs](self) for the full rule list.
pub struct TaskRoutingModule {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for TaskRoutingModule {
    fn default() -> Self {
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(TaskIdPresent),
            Box::new(TaskTypeKnown),
            Box::new(RequiredCapabilityDeclared),
            Box::new(TaskStateTransitionValid),
            Box::new(AssignedAgentPresentWhenInProgress),
            Box::new(PriorityInRange),
            Box::new(LoadBalanceHeadroom),
        ];
        Self { rules }
    }
}

impl PraxisModule for TaskRoutingModule {
    fn name(&self) -> &str {
        "task-routing"
    }

    fn rules(&self) -> &[Box<dyn Rule>] {
        &self.rules
    }

    fn expectations(&self) -> Vec<String> {
        vec![
            "Task IDs are generated by the caller before submission.".into(),
            "Task types are drawn from the agreed registry; unknown types are rejected.".into(),
            "Agent capacity metrics are refreshed before load-balance checks.".into(),
            "Priority values follow the 1 (highest) – 10 (lowest) convention.".into(),
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

    fn module() -> TaskRoutingModule {
        TaskRoutingModule::default()
    }

    // ── Input: task_id_present ───────────────────────────────────────────────
    #[test]
    fn task_id_present_passes() {
        let ctx = RuleContext::new("route_task", json!({"task_id": "t-001"}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_id_present")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn task_id_present_fails_missing() {
        let ctx = RuleContext::new("route_task", json!({}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_id_present")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Input: task_type_known ───────────────────────────────────────────────
    #[test]
    fn task_type_known_passes_inference() {
        let ctx = RuleContext::new("route_task", json!({"task_type": "inference"}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_type_known")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn task_type_known_fails_unknown_type() {
        let ctx = RuleContext::new("route_task", json!({"task_type": "telepathy"}));
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_type_known")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── State: task_state_transition_valid ───────────────────────────────────
    #[test]
    fn task_transition_queued_to_assigned_passes() {
        let ctx = RuleContext::new(
            "transition",
            json!({"from_state": "queued", "to_state": "assigned"}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_state_transition_valid")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn task_transition_complete_to_in_progress_fails() {
        let ctx = RuleContext::new(
            "transition",
            json!({"from_state": "complete", "to_state": "in_progress"}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "task_state_transition_valid")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn assigned_agent_required_for_in_progress() {
        let ctx = RuleContext::new("transition", json!({"to_state": "in_progress"}));
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "assigned_agent_present_when_in_progress")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Data: priority_in_range ──────────────────────────────────────────────
    #[test]
    fn priority_in_range_passes() {
        let ctx = RuleContext::new("route_task", json!({"priority": 3}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "priority_in_range")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn priority_out_of_range_fails() {
        let ctx = RuleContext::new("route_task", json!({"priority": 11}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "priority_in_range")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    #[test]
    fn load_balance_warns_at_high_utilisation() {
        let ctx = RuleContext::new(
            "load_check",
            json!({"queue_depth": 90, "agent_capacity": 100}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "load_balance_headroom")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Warning { .. }));
    }

    #[test]
    fn load_balance_passes_below_80_pct() {
        let ctx = RuleContext::new(
            "load_check",
            json!({"queue_depth": 50, "agent_capacity": 100}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "load_balance_headroom")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    // ── Audit ─────────────────────────────────────────────────────────────────
    #[test]
    fn module_audit_is_complete() {
        let report = module().audit();
        assert!(
            report.is_complete(),
            "task-routing should cover all categories"
        );
        assert_eq!(report.completeness_pct, 100.0);
    }
}
