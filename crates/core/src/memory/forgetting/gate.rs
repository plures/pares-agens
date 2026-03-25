//! Approval gates — destructive actions require confirmation before execution.
//!
//! [`ApprovalGate`] is a synchronous trait intentionally.  Async approval
//! (e.g. prompting the user over an IPC channel) can be implemented by
//! blocking inside the implementation or by pre-computing the decision
//! externally and wrapping it in [`PreApprovedGate`].

use super::engine::PurgeReport;

// ---------------------------------------------------------------------------
// ApprovalGate trait
// ---------------------------------------------------------------------------

/// Guards destructive purge operations.
///
/// [`ForgettingEngine::execute`][super::engine::ForgettingEngine::execute]
/// calls [`approve`][Self::approve] with the dry-run [`PurgeReport`] before
/// performing any mutations.  Return `false` to abort.
pub trait ApprovalGate: Send + Sync {
    /// Return `true` to allow the purge to proceed; `false` to abort.
    fn approve(&self, report: &PurgeReport) -> bool;
}

// ---------------------------------------------------------------------------
// AutoApproveGate
// ---------------------------------------------------------------------------

/// Always approves — suitable for scheduled / automated purge passes.
///
/// **Use with care**: no confirmation is requested and data will be permanently
/// deleted.
pub struct AutoApproveGate;

impl ApprovalGate for AutoApproveGate {
    fn approve(&self, _report: &PurgeReport) -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// DenyAllGate
// ---------------------------------------------------------------------------

/// Always denies — useful in tests and read-only contexts.
pub struct DenyAllGate;

impl ApprovalGate for DenyAllGate {
    fn approve(&self, _report: &PurgeReport) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// PreApprovedGate
// ---------------------------------------------------------------------------

/// Approves only when `approved` is `true`.
///
/// Useful for wrapping a user's interactive yes/no decision that was obtained
/// before calling [`ForgettingEngine::execute`].
pub struct PreApprovedGate {
    approved: bool,
}

impl PreApprovedGate {
    /// Create a gate that will approve (`true`) or deny (`false`) the purge.
    pub fn new(approved: bool) -> Self {
        Self { approved }
    }
}

impl ApprovalGate for PreApprovedGate {
    fn approve(&self, _report: &PurgeReport) -> bool {
        self.approved
    }
}

// ---------------------------------------------------------------------------
// ThresholdGate
// ---------------------------------------------------------------------------

/// Approves only when the number of affected entries is at or below a
/// threshold, preventing accidental mass-deletions.
pub struct ThresholdGate {
    max_affected: usize,
}

impl ThresholdGate {
    /// Create a gate that denies purges affecting more than `max_affected`
    /// entries.
    pub fn new(max_affected: usize) -> Self {
        Self { max_affected }
    }
}

impl ApprovalGate for ThresholdGate {
    fn approve(&self, report: &PurgeReport) -> bool {
        report.total_affected <= self.max_affected
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn report(n: usize) -> PurgeReport {
        PurgeReport {
            entries: vec![],
            total_affected: n,
            is_dry_run: true,
        }
    }

    #[test]
    fn auto_approve_always_true() {
        assert!(AutoApproveGate.approve(&report(0)));
        assert!(AutoApproveGate.approve(&report(9999)));
    }

    #[test]
    fn deny_all_always_false() {
        assert!(!DenyAllGate.approve(&report(0)));
        assert!(!DenyAllGate.approve(&report(1)));
    }

    #[test]
    fn pre_approved_gate_follows_flag() {
        assert!(PreApprovedGate::new(true).approve(&report(5)));
        assert!(!PreApprovedGate::new(false).approve(&report(5)));
    }

    #[test]
    fn threshold_gate_approves_at_or_below() {
        let gate = ThresholdGate::new(10);
        assert!(gate.approve(&report(10)));
        assert!(gate.approve(&report(0)));
        assert!(!gate.approve(&report(11)));
    }
}
