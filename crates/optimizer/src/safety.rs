//! Safety state enforcement for the optimization runtime.
//!
//! The control-plane evaluates three gate outputs before an optimization run
//! may proceed:
//!
//! - [`SafetyState::Ready`] — all checks passed; optimization is permitted.
//! - [`SafetyState::InsufficientData`] — not enough evidence to make a safe
//!   decision; the run is halted and an [`EvidenceRequest`] is emitted.
//! - [`SafetyState::UnsafeSolution`] — the candidate solution was flagged as
//!   unsafe; the run is blocked and remediation evidence is requested.
//!
//! [`MaxMinOptimizer::run`](crate::engine::MaxMinOptimizer::run) enforces these
//! states at the entry point of every optimization episode.

use serde::{Deserialize, Serialize};

// ── SafetyState ───────────────────────────────────────────────────────────────

/// Gate output produced by the control-plane for an optimization episode.
///
/// The optimizer enforces this state at runtime: only [`SafetyState::Ready`]
/// episodes are executed.  All other states cause the run to be halted and an
/// [`EvidenceRequest`] to be emitted via the telemetry channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SafetyState {
    /// All control-plane checks passed; optimization may proceed.
    Ready,

    /// The control-plane determined that there is not enough data to make a
    /// safe optimization decision.
    InsufficientData {
        /// Names of the data fields or evidence keys that are absent.
        missing_fields: Vec<String>,
    },

    /// The control-plane flagged the candidate solution as unsafe.
    UnsafeSolution {
        /// Human-readable explanation of why the solution is considered unsafe.
        reason: String,
        /// Evidence items required to remediate the unsafe state before the run
        /// can be retried.  These are the specific data or sign-off artefacts
        /// that the calling system must gather (e.g. `["safety_review", "risk_sign_off"]`).
        remediation: Vec<String>,
    },
}

impl Default for SafetyState {
    /// The default safety state is [`SafetyState::Ready`], preserving
    /// backward-compatibility for existing [`OptimizerInput`](crate::OptimizerInput)
    /// construction that does not specify an explicit state.
    fn default() -> Self {
        Self::Ready
    }
}

impl SafetyState {
    /// Returns `true` when optimization may proceed without restriction.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Collect the evidence items required to transition to [`SafetyState::Ready`].
    ///
    /// Returns an empty `Vec` when already `Ready`.
    #[must_use]
    pub fn required_evidence(&self) -> Vec<String> {
        match self {
            Self::Ready => vec![],
            Self::InsufficientData { missing_fields } => missing_fields.clone(),
            Self::UnsafeSolution { remediation, .. } => remediation.clone(),
        }
    }

    /// A short, human-readable label suitable for log messages and telemetry tags.
    ///
    /// The labels match the control-plane gate output names:
    /// - `"ready"`
    /// - `"insufficient_data"`
    /// - `"unsafe_solution"`
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::InsufficientData { .. } => "insufficient_data",
            Self::UnsafeSolution { .. } => "unsafe_solution",
        }
    }
}

// ── EvidenceRequest ───────────────────────────────────────────────────────────

/// Request for additional evidence emitted when an optimization run is blocked.
///
/// Callers that receive an [`OptimizerError::SafetyBlocked`](crate::OptimizerError)
/// can inspect the `state` and `required_evidence` fields to determine what data
/// must be gathered before the run can be retried as [`SafetyState::Ready`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRequest {
    /// The run identifier from the blocked [`OptimizerInput`](crate::OptimizerInput).
    pub run_id: String,

    /// The policy identifier from the blocked [`OptimizerInput`](crate::OptimizerInput).
    pub policy_id: String,

    /// The safety state that caused the block.
    pub state: SafetyState,

    /// Evidence items that must be supplied before the run can proceed.
    pub required_evidence: Vec<String>,
}

impl EvidenceRequest {
    /// Construct an `EvidenceRequest` from run identifiers and a [`SafetyState`].
    #[must_use]
    pub fn from_state(
        run_id: impl Into<String>,
        policy_id: impl Into<String>,
        state: SafetyState,
    ) -> Self {
        let required_evidence = state.required_evidence();
        Self {
            run_id: run_id.into(),
            policy_id: policy_id.into(),
            state,
            required_evidence,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_state_is_ready() {
        assert!(SafetyState::Ready.is_ready());
    }

    #[test]
    fn insufficient_data_is_not_ready() {
        let state = SafetyState::InsufficientData {
            missing_fields: vec!["score_history".into()],
        };
        assert!(!state.is_ready());
    }

    #[test]
    fn unsafe_solution_is_not_ready() {
        let state = SafetyState::UnsafeSolution {
            reason: "constraint budget exhausted".into(),
            remediation: vec![],
        };
        assert!(!state.is_ready());
    }

    #[test]
    fn ready_requires_no_evidence() {
        assert!(SafetyState::Ready.required_evidence().is_empty());
    }

    #[test]
    fn insufficient_data_evidence_lists_missing_fields() {
        let state = SafetyState::InsufficientData {
            missing_fields: vec!["field_a".into(), "field_b".into()],
        };
        assert_eq!(state.required_evidence(), vec!["field_a", "field_b"]);
    }

    #[test]
    fn unsafe_solution_evidence_contains_remediation_items() {
        let state = SafetyState::UnsafeSolution {
            reason: "safety threshold exceeded".into(),
            remediation: vec!["safety_review".into(), "risk_sign_off".into()],
        };
        assert_eq!(state.required_evidence(), vec!["safety_review", "risk_sign_off"]);
    }

    #[test]
    fn labels_match_control_plane_gate_names() {
        assert_eq!(SafetyState::Ready.label(), "ready");
        assert_eq!(
            SafetyState::InsufficientData { missing_fields: vec![] }.label(),
            "insufficient_data"
        );
        assert_eq!(
            SafetyState::UnsafeSolution { reason: String::new(), remediation: vec![] }.label(),
            "unsafe_solution"
        );
    }

    #[test]
    fn default_safety_state_is_ready() {
        assert_eq!(SafetyState::default(), SafetyState::Ready);
    }

    #[test]
    fn evidence_request_from_state_captures_required_evidence() {
        let state = SafetyState::InsufficientData {
            missing_fields: vec!["baseline_score".into()],
        };
        let req = EvidenceRequest::from_state("run-1", "policy-v1", state);
        assert_eq!(req.run_id, "run-1");
        assert_eq!(req.policy_id, "policy-v1");
        assert_eq!(req.required_evidence, vec!["baseline_score"]);
    }

    #[test]
    fn evidence_request_for_unsafe_solution_captures_remediation() {
        let state = SafetyState::UnsafeSolution {
            reason: "exceeds risk tolerance".into(),
            remediation: vec!["risk_assessment".into()],
        };
        let req = EvidenceRequest::from_state("run-2", "policy-v2", state);
        assert_eq!(req.required_evidence, vec!["risk_assessment"]);
    }
}
