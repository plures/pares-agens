//! `coordination` Praxis module.
//!
//! Governs multi-agent collaboration, conflict resolution, and consensus
//! workflows in the Pares mesh.
//!
//! ## Input rules
//! - `collaboration_request_valid` — request must carry `requester_id`,
//!   `collaborators` (≥ 1), and `objective`
//! - `no_self_collaboration` — an agent may not list itself as a collaborator
//!
//! ## State rules
//! - `consensus_transition_valid` — consensus states follow:
//!   proposed → voting → accepted | rejected
//! - `quorum_required_for_accepted` — a proposal may only move to `accepted`
//!   when `votes_for` / `total_votes` ≥ 0.51 (simple majority)
//!
//! ## Data rules
//! - `conflict_score_actionable` — warn when `conflict_score` > 0.7; enter
//!   an approval gate when > 0.95 (deadlock territory)
//! - `collaborator_count_sane` — warn when collaborator count > 20 (overhead
//!   concern)

use crate::module::PraxisModule;
use crate::rule::{Rule, RuleCategory, RuleContext, RuleResult};

// ---------------------------------------------------------------------------
// Allowed consensus transitions
// ---------------------------------------------------------------------------

const VALID_CONSENSUS_TRANSITIONS: &[(&str, &str)] = &[
    ("proposed", "voting"),
    ("voting", "accepted"),
    ("voting", "rejected"),
    ("rejected", "proposed"), // retry after revision
];

fn consensus_transition_allowed(from: &str, to: &str) -> bool {
    VALID_CONSENSUS_TRANSITIONS
        .iter()
        .any(|(f, t)| *f == from && *t == to)
}

// ---------------------------------------------------------------------------
// Input rules
// ---------------------------------------------------------------------------

struct CollaborationRequestValid;
impl Rule for CollaborationRequestValid {
    fn name(&self) -> &str {
        "collaboration_request_valid"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let requester_missing = ctx
            .payload_str("requester_id")
            .map(|s| s.is_empty())
            .unwrap_or(true);
        let objective_missing = ctx
            .payload_str("objective")
            .map(|s| s.is_empty())
            .unwrap_or(true);
        let collaborators_empty = ctx
            .payload_array_len("collaborators")
            .map(|n| n == 0)
            .unwrap_or(true);

        if requester_missing || objective_missing || collaborators_empty {
            RuleResult::Fail {
                reason: "collaboration request requires `requester_id`, `objective`, and at least one entry in `collaborators`".into(),
            }
        } else {
            RuleResult::Pass
        }
    }
}

struct NoSelfCollaboration;
impl Rule for NoSelfCollaboration {
    fn name(&self) -> &str {
        "no_self_collaboration"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Input
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let requester = ctx.payload_str("requester_id").unwrap_or("");
        let collaborators = match ctx.payload.get("collaborators").and_then(|v| v.as_array()) {
            Some(arr) => arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>(),
            None => return RuleResult::Pass,
        };
        if collaborators.contains(&requester) {
            RuleResult::Fail {
                reason: format!("agent `{requester}` listed itself as a collaborator"),
            }
        } else {
            RuleResult::Pass
        }
    }
}

// ---------------------------------------------------------------------------
// State rules
// ---------------------------------------------------------------------------

struct ConsensusTransitionValid;
impl Rule for ConsensusTransitionValid {
    fn name(&self) -> &str {
        "consensus_transition_valid"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let from = ctx.payload_str("from_state").unwrap_or("");
        let to = ctx.payload_str("to_state").unwrap_or("");
        if from.is_empty() || to.is_empty() {
            return RuleResult::Fail {
                reason: "consensus transition requires `from_state` and `to_state`".into(),
            };
        }
        if consensus_transition_allowed(from, to) {
            RuleResult::Pass
        } else {
            RuleResult::Fail {
                reason: format!("consensus transition `{from}` → `{to}` is not permitted"),
            }
        }
    }
}

struct QuorumRequiredForAccepted;
impl Rule for QuorumRequiredForAccepted {
    fn name(&self) -> &str {
        "quorum_required_for_accepted"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::State
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        let to_state = ctx.payload_str("to_state").unwrap_or("");
        if to_state != "accepted" {
            return RuleResult::Pass; // rule only applies when accepting
        }
        let votes_for = ctx.payload_u64("votes_for").unwrap_or(0) as f64;
        let total_votes = ctx.payload_u64("total_votes").unwrap_or(0) as f64;
        if total_votes == 0.0 {
            return RuleResult::Fail {
                reason: "cannot accept with zero votes cast".into(),
            };
        }
        let ratio = votes_for / total_votes;
        if ratio >= 0.51 {
            RuleResult::Pass
        } else {
            RuleResult::Fail {
                reason: format!(
                    "quorum not reached: {votes_for}/{total_votes} = {:.0}% (need ≥ 51%)",
                    ratio * 100.0
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Data rules
// ---------------------------------------------------------------------------

struct ConflictScoreActionable;
impl Rule for ConflictScoreActionable {
    fn name(&self) -> &str {
        "conflict_score_actionable"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_f64("conflict_score") {
            None => RuleResult::Pass, // no conflict reported
            Some(s) if s > 0.95 => RuleResult::Gate {
                action: "escalate_conflict".into(),
                rationale: format!(
                    "conflict_score {s:.2} exceeds 0.95 — manual resolution required"
                ),
            },
            Some(s) if s > 0.7 => RuleResult::Warning {
                message: format!(
                    "conflict_score {s:.2} is elevated (> 0.7); monitor collaboration closely"
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

struct CollaboratorCountSane;
impl Rule for CollaboratorCountSane {
    fn name(&self) -> &str {
        "collaborator_count_sane"
    }
    fn category(&self) -> RuleCategory {
        RuleCategory::Data
    }
    fn evaluate(&self, ctx: &RuleContext) -> RuleResult {
        match ctx.payload_array_len("collaborators") {
            Some(n) if n > 20 => RuleResult::Warning {
                message: format!(
                    "{n} collaborators may introduce coordination overhead; consider splitting the task"
                ),
            },
            _ => RuleResult::Pass,
        }
    }
}

// ---------------------------------------------------------------------------
// CoordinationModule
// ---------------------------------------------------------------------------

/// Praxis module for multi-agent coordination and conflict resolution.
///
/// See the [module docs](self) for the full rule list.
pub struct CoordinationModule {
    rules: Vec<Box<dyn Rule>>,
}

impl Default for CoordinationModule {
    fn default() -> Self {
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(CollaborationRequestValid),
            Box::new(NoSelfCollaboration),
            Box::new(ConsensusTransitionValid),
            Box::new(QuorumRequiredForAccepted),
            Box::new(ConflictScoreActionable),
            Box::new(CollaboratorCountSane),
        ];
        Self { rules }
    }
}

impl PraxisModule for CoordinationModule {
    fn name(&self) -> &str {
        "coordination"
    }

    fn rules(&self) -> &[Box<dyn Rule>] {
        &self.rules
    }

    fn expectations(&self) -> Vec<String> {
        vec![
            "Collaboration requests originate from authenticated agents.".into(),
            "Consensus votes are cast once per agent per proposal.".into(),
            "Conflict scores are computed by the orchestration layer, not self-reported.".into(),
            "Proposal IDs are unique and stable across voting rounds.".into(),
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

    fn module() -> CoordinationModule {
        CoordinationModule::default()
    }

    // ── Input: collaboration_request_valid ───────────────────────────────────
    #[test]
    fn collaboration_request_passes_when_valid() {
        let ctx = RuleContext::new(
            "collab",
            json!({
                "requester_id": "agent-a",
                "objective": "summarise",
                "collaborators": ["agent-b"]
            }),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "collaboration_request_valid")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn collaboration_request_fails_missing_collaborators() {
        let ctx = RuleContext::new(
            "collab",
            json!({
                "requester_id": "agent-a",
                "objective": "summarise",
                "collaborators": []
            }),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "collaboration_request_valid")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Input: no_self_collaboration ─────────────────────────────────────────
    #[test]
    fn no_self_collaboration_blocks_self_listing() {
        let ctx = RuleContext::new(
            "collab",
            json!({
                "requester_id": "agent-a",
                "collaborators": ["agent-a", "agent-b"]
            }),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::Input);
        let r = results
            .iter()
            .find(|(n, _)| n == "no_self_collaboration")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── State: consensus_transition_valid ────────────────────────────────────
    #[test]
    fn consensus_proposed_to_voting_passes() {
        let ctx = RuleContext::new(
            "consensus",
            json!({"from_state": "proposed", "to_state": "voting"}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "consensus_transition_valid")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn consensus_accepted_to_rejected_fails() {
        let ctx = RuleContext::new(
            "consensus",
            json!({"from_state": "accepted", "to_state": "rejected"}),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "consensus_transition_valid")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── State: quorum_required_for_accepted ──────────────────────────────────
    #[test]
    fn quorum_passes_with_majority() {
        let ctx = RuleContext::new(
            "vote",
            json!({
                "to_state": "accepted",
                "votes_for": 6,
                "total_votes": 10
            }),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "quorum_required_for_accepted")
            .unwrap();
        assert_eq!(r.1, RuleResult::Pass);
    }

    #[test]
    fn quorum_fails_without_majority() {
        let ctx = RuleContext::new(
            "vote",
            json!({
                "to_state": "accepted",
                "votes_for": 4,
                "total_votes": 10
            }),
        );
        let results = module().evaluate_category(&ctx, RuleCategory::State);
        let r = results
            .iter()
            .find(|(n, _)| n == "quorum_required_for_accepted")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Fail { .. }));
    }

    // ── Data: conflict_score_actionable ──────────────────────────────────────
    #[test]
    fn conflict_score_gates_above_0_95() {
        let ctx = RuleContext::new("check", json!({"conflict_score": 0.97}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "conflict_score_actionable")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Gate { .. }));
    }

    #[test]
    fn conflict_score_warns_above_0_7() {
        let ctx = RuleContext::new("check", json!({"conflict_score": 0.8}));
        let results = module().evaluate_category(&ctx, RuleCategory::Data);
        let r = results
            .iter()
            .find(|(n, _)| n == "conflict_score_actionable")
            .unwrap();
        assert!(matches!(r.1, RuleResult::Warning { .. }));
    }

    // ── Audit ─────────────────────────────────────────────────────────────────
    #[test]
    fn module_audit_is_complete() {
        let report = module().audit();
        assert!(
            report.is_complete(),
            "coordination should cover all categories"
        );
        assert_eq!(report.completeness_pct, 100.0);
    }
}
