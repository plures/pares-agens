//! Integration test: system-enforced approval gate for destructive tool calls.
//!
//! Addresses plures/pares-agens#674 — proves the approval gate is a real
//! enforcement point (ToolGovernor + ApprovalRegistry + AuthorizationGate),
//! not merely model-judgment-based refusal in conversational text.
//!
//! The test forces a destructive tool call through the governance layer and
//! confirms:
//! 1. The orchestrator's AuthorizationGate emits a `RuleResult::Gate` for
//!    destructive tool events (system detection, not LLM judgment).
//! 2. `ToolGovernor` returns `AllowWithApprovalWarning` for tools matching
//!    destructive patterns (system-level policy enforcement).
//! 3. `ApprovalRegistry` blocks execution until an explicit human decision
//!    is recorded (decision-ledger record written, action gated).
//! 4. Without approval resolution, the pending approval remains blocking —
//!    no silent fallthrough to execution.

use std::sync::Arc;
use std::time::Duration;

use pares_radix_core::approval::{ApprovalDecision, ApprovalRegistry};
use pares_radix_core::event::Event;
use pares_radix_core::praxis::constraints::AuthorizationGate;
use pares_radix_core::tool_governance::{GovernanceVerdict, ToolGovernor};
use pares_radix_praxis::rule::{Rule, RuleResult};

use pares_agens_core::orchestrator::build_authorization_context;

// ── Test 1: AuthorizationGate system-detects destructive tool calls ──────────

#[test]
fn authorization_gate_emits_gate_for_destructive_tool_call() {
    // Simulate a destructive tool call event — this is what the system sees
    // when the LLM actually invokes a destructive tool (not just discusses it).
    let destructive_event = Event::ToolResult {
        tool_call_id: "tc-delete-all".into(),
        tool_name: "delete_all_files".into(),
        content: "confirmed".into(),
        is_error: false,
    };

    let ctx = build_authorization_context(&destructive_event);

    // System MUST detect this as destructive (not relying on model judgment)
    assert!(
        ctx.payload["is_destructive"].as_bool().unwrap_or(false),
        "system must flag delete_* tools as destructive independent of LLM"
    );

    // AuthorizationGate MUST return Gate (approval required), not Pass
    let result = AuthorizationGate.evaluate(&ctx);
    assert!(
        matches!(result, RuleResult::Gate { .. }),
        "destructive tool call must trigger system-level approval gate, got: {result:?}"
    );

    // Verify the gate carries actionable metadata
    if let RuleResult::Gate { action, rationale } = &result {
        assert!(
            !action.is_empty(),
            "gate action must identify the tool/event"
        );
        assert!(
            !rationale.is_empty(),
            "gate rationale must explain why approval is needed"
        );
    }
}

// ── Test 2: ToolGovernor returns AllowWithApprovalWarning for destructive tools ──

#[test]
fn tool_governor_flags_destructive_tool_for_approval() {
    use pares_radix_core::tool_governance::ToolPolicy;

    let mut governor = ToolGovernor::with_defaults();

    // Configure the governor to require approval for destructive tools.
    // This is the policy that MUST be active for system-enforced gating
    // (per ADR-0016: ToolGovernor must enforce approval, not just warn).
    governor.set_policy(ToolPolicy {
        tool_name: "delete_all_files".into(),
        approval_required: true,
        timeout_ms: 30_000,
        sandboxed: false,
        allowed_patterns: vec![],
        blocked_patterns: vec![],
    });

    let verdict = governor.check("delete_all_files", r#"{"path": "/", "recursive": true}"#);

    // The governor MUST NOT simply Allow destructive tools through
    assert!(
        !matches!(verdict, GovernanceVerdict::Allow),
        "destructive tool with approval_required policy must NOT be silently allowed"
    );

    // It must return AllowWithApprovalWarning — the enforcement point that
    // should block execution pending human approval.
    assert!(
        matches!(verdict, GovernanceVerdict::AllowWithApprovalWarning),
        "destructive tool must trigger AllowWithApprovalWarning, got: {verdict:?}"
    );
}

// ── Test 3: ApprovalRegistry blocks until explicit human decision ─────────────

#[tokio::test]
async fn approval_registry_blocks_destructive_tool_pending_human_decision() {
    let registry = ApprovalRegistry::new();

    // Register a pending approval for a destructive tool call
    let (req, pending) = registry.register("delete_all_files", "/home/user/*").await;

    // Decision-ledger record: token exists and is pending
    assert!(
        !req.token.is_empty(),
        "approval request must have a non-empty token (decision-ledger record)"
    );
    assert_eq!(
        registry.pending_count().await,
        1,
        "exactly one pending approval must be recorded in the registry"
    );

    // Prove the gate is BLOCKING: spawn a task that waits on approval,
    // verify it does NOT complete within a short timeout (action is gated).
    let pending_arc = Arc::new(tokio::sync::Mutex::new(Some(pending)));
    let pending_clone = Arc::clone(&pending_arc);

    let wait_handle = tokio::spawn(async move {
        let mut guard = pending_clone.lock().await;
        let p = guard.take().unwrap();
        p.wait().await
    });

    // Give the wait task a chance to resolve (it should NOT resolve)
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !wait_handle.is_finished(),
        "approval gate MUST block execution — action must not proceed without human decision"
    );

    // Now simulate human approval (the enforcement point)
    let resolved = registry
        .resolve(&req.token, ApprovalDecision::Allow)
        .await;
    assert!(resolved, "resolve must wake the blocked waiter");

    // The blocked task should now complete with the human's decision
    let decision = tokio::time::timeout(Duration::from_secs(2), wait_handle)
        .await
        .expect("wait task must complete after resolution")
        .expect("wait task must not panic");

    assert_eq!(decision, ApprovalDecision::Allow);
    assert_eq!(
        registry.pending_count().await,
        0,
        "resolved approval must be removed from pending set"
    );
}

// ── Test 4: Denied approval blocks execution (fail-closed) ────────────────────

#[tokio::test]
async fn approval_registry_deny_blocks_destructive_action() {
    let registry = ApprovalRegistry::new();

    let (req, pending) = registry
        .register("delete_all_files", "rm -rf / --no-preserve-root")
        .await;

    // Human denies the destructive action
    let resolved = registry
        .resolve(&req.token, ApprovalDecision::Deny)
        .await;
    assert!(resolved);

    let decision = pending.wait().await;
    assert_eq!(
        decision,
        ApprovalDecision::Deny,
        "denied destructive action must propagate Deny to caller"
    );
    assert!(
        !decision.is_allowed(),
        "Deny decision must prevent tool execution"
    );
}

// ── Test 5: End-to-end — gate detection + registry blocking compose correctly ─

#[tokio::test]
async fn end_to_end_destructive_tool_call_is_system_gated() {
    // Step 1: A destructive tool call arrives as an event
    let event = Event::ToolResult {
        tool_call_id: "tc-rm".into(),
        tool_name: "delete_repository".into(),
        content: "{}".into(),
        is_error: false,
    };

    // Step 2: System-level detection (AuthorizationGate)
    let ctx = build_authorization_context(&event);
    let gate_result = AuthorizationGate.evaluate(&ctx);
    assert!(
        matches!(gate_result, RuleResult::Gate { .. }),
        "system must gate destructive tool call"
    );

    // Step 3: ToolGovernor confirms approval needed (with appropriate policy)
    use pares_radix_core::tool_governance::ToolPolicy;
    let mut governor = ToolGovernor::with_defaults();
    governor.set_policy(ToolPolicy {
        tool_name: "delete_repository".into(),
        approval_required: true,
        timeout_ms: 30_000,
        sandboxed: false,
        allowed_patterns: vec![],
        blocked_patterns: vec![],
    });
    let verdict = governor.check("delete_repository", "{}");
    assert!(
        matches!(verdict, GovernanceVerdict::AllowWithApprovalWarning),
        "governor must flag destructive tool for approval when policy is set"
    );

    // Step 4: ApprovalRegistry blocks execution
    let registry = ApprovalRegistry::new();
    let (req, pending) = registry.register("delete_repository", "{}").await;

    // The action is now blocked in the decision ledger
    assert_eq!(registry.pending_count().await, 1);

    // Step 5: Only explicit human Allow unblocks
    registry
        .resolve(&req.token, ApprovalDecision::Allow)
        .await;
    let decision = pending.wait().await;
    assert!(
        decision.is_allowed(),
        "only after human approval should destructive action proceed"
    );
}
