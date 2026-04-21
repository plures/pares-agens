//! Praxis rules for the PR lifecycle.
//!
//! Each rule is a function: facts → Option<RuleResult>.
//! Rules are evaluated in priority order. First match wins.

use super::{LifecycleAction, LifecycleFact, RuleResult};
use super::facts::{PRFacts, CIStatus, IssueFacts, MilestoneFacts, FailureClass, classify_failure};

/// Evaluate all PR rules against the given facts. Returns the first matching result.
#[allow(clippy::type_complexity, clippy::vec_init_then_push)]
pub fn evaluate_pr(facts: &PRFacts) -> RuleResult {
    let rules: Vec<(&str, fn(&PRFacts) -> Option<Vec<LifecycleAction>>)> = vec![
        ("skip-draft", rule_skip_draft),
        ("ci-green-reviewed-merge", rule_ci_green_merge),
        ("ci-green-request-review", rule_ci_green_request_review),
        ("ci-failing-retry", rule_ci_failing_retry),
        ("ci-failing-force-merge", rule_ci_failing_force_merge),
        ("ci-pending-wait", rule_ci_pending_wait),
        ("non-copilot-skip", rule_non_copilot_skip),
    ];

    for (name, rule) in &rules {
        if let Some(actions) = rule(facts) {
            return RuleResult {
                actions,
                new_facts: vec![],
                matched_rule: name.to_string(),
            };
        }
    }

    RuleResult {
        actions: vec![LifecycleAction::Noop { reason: "no rule matched".into() }],
        new_facts: vec![],
        matched_rule: "none".into(),
    }
}

// ── Individual rules ─────────────────────────────────────────────────────────

fn rule_skip_draft(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if facts.is_draft && !facts.is_copilot {
        Some(vec![LifecycleAction::Noop { reason: "draft PR, not Copilot".into() }])
    } else {
        None
    }
}

fn rule_ci_green_merge(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if !matches!(facts.ci_status, CIStatus::Green) { return None; }
    if !facts.has_review { return None; }
    if !facts.is_copilot { return None; }

    let mut actions = vec![];

    if !facts.has_approval {
        actions.push(LifecycleAction::ApprovePR {
            repo: facts.repo.clone(),
            number: facts.number,
        });
    }

    actions.push(LifecycleAction::MergePR {
        repo: facts.repo.clone(),
        number: facts.number,
        method: "squash".into(),
    });

    Some(actions)
}

fn rule_ci_green_request_review(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if !matches!(facts.ci_status, CIStatus::Green) { return None; }
    if facts.has_review { return None; }
    if !facts.is_copilot { return None; }

    // Copilot review is automatic via org ruleset — just wait
    Some(vec![LifecycleAction::Noop { reason: "CI green, waiting for Copilot review".into() }])
}

fn rule_ci_failing_retry(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if !matches!(facts.ci_status, CIStatus::Failing) { return None; }
    if !facts.is_copilot { return None; }
    if facts.retry_count >= 2 { return None; } // exhausted retries

    let next_retry = facts.retry_count + 1;
    let mut actions = vec![];

    // Remove old retry label, add new one
    if facts.retry_count > 0 {
        actions.push(LifecycleAction::RemoveLabel {
            repo: facts.repo.clone(),
            number: facts.number,
            label: format!("ci-retry-{}", facts.retry_count),
        });
    }

    actions.push(LifecycleAction::AddLabel {
        repo: facts.repo.clone(),
        number: facts.number,
        label: format!("ci-retry-{next_retry}"),
    });

    // TODO: rerun CI (need run_id from facts)

    Some(actions)
}

#[allow(clippy::vec_init_then_push)]
fn rule_ci_failing_force_merge(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if !matches!(facts.ci_status, CIStatus::Failing) { return None; }
    if !facts.is_copilot { return None; }
    if facts.retry_count < 2 { return None; } // not exhausted yet

    let failure_class = classify_failure(&facts.failing_checks, "");
    let is_infra = failure_class == FailureClass::Infrastructure;

    let mut actions = vec![];

    actions.push(LifecycleAction::CreateCIFeedback {
        repo: facts.repo.clone(),
        pr_number: facts.number,
        error_details: facts.failing_checks.join(", "),
        is_infra,
    });

    actions.push(LifecycleAction::MergePR {
        repo: facts.repo.clone(),
        number: facts.number,
        method: "squash".into(),
    });

    Some(actions)
}

fn rule_ci_pending_wait(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if !matches!(facts.ci_status, CIStatus::Pending) { return None; }

    Some(vec![LifecycleAction::Noop { reason: "CI still running".into() }])
}

fn rule_non_copilot_skip(facts: &PRFacts) -> Option<Vec<LifecycleAction>> {
    if facts.is_copilot { return None; }
    if !matches!(facts.ci_status, CIStatus::Failing) { return None; }

    Some(vec![LifecycleAction::Noop { reason: "non-Copilot PR with CI failures — skipping".into() }])
}

/// Evaluate queue-advance rules: which issue should Copilot work on next?
pub fn evaluate_queue(
    issues: &[IssueFacts],
    has_active_copilot_pr: bool,
) -> Option<RuleResult> {
    if has_active_copilot_pr {
        return None; // busy
    }

    // Priority order: CI fixes → bugs → critical → doc debt → improvement → strategic
    let priority_labels = [
        ("ci-failure", "CI fix"),
        ("bug", "Bug fix"),
        ("critical", "Critical"),
        ("documentation", "Doc debt"),
        ("continuous-improvement", "Improvement"),
        ("strategic-gate", "Strategic"),
        ("enhancement", "Feature"),
    ];

    for (label, desc) in &priority_labels {
        if let Some(issue) = issues.iter().find(|i|
            i.labels.iter().any(|l| l == *label) && !i.is_copilot_assigned
        ) {
            return Some(RuleResult {
                actions: vec![LifecycleAction::AssignCopilot {
                    repo: issue.repo.clone(),
                    issue_number: issue.number,
                }],
                new_facts: vec![LifecycleFact {
                    category: "work-in-progress".into(),
                    content: format!("{desc}: assigned #{} to Copilot", issue.number),
                    tags: vec!["lifecycle".into(), "queue-advance".into()],
                }],
                matched_rule: format!("queue-{label}"),
            });
        }
    }

    None
}

/// Check if a milestone is complete and should be released.
pub fn evaluate_milestone(ms: &MilestoneFacts) -> Option<RuleResult> {
    if ms.open_issues > 0 || ms.closed_issues == 0 {
        return None;
    }

    let version = ms.version.as_ref()?;
    let tag = format!("v{version}");

    Some(RuleResult {
        actions: vec![
            LifecycleAction::CreateRelease {
                repo: ms.repo.clone(),
                tag: tag.clone(),
                title: format!("{tag} — {}", ms.title),
                body: format!("Milestone **{}** complete. {} issues closed.", ms.title, ms.closed_issues),
            },
            LifecycleAction::Notify {
                message: format!("🚀 Released {tag} for {}", ms.repo),
            },
        ],
        new_facts: vec![LifecycleFact {
            category: "decision".into(),
            content: format!("Released {tag} from milestone '{}' ({} issues)", ms.title, ms.closed_issues),
            tags: vec!["release".into(), "milestone".into()],
        }],
        matched_rule: "milestone-complete-release".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ci_green_copilot_reviewed_merges() {
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 1,
            is_copilot: true,
            ci_status: CIStatus::Green,
            has_review: true,
            has_approval: false,
            ..Default::default()
        };
        let result = evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "ci-green-reviewed-merge");
        assert!(result.actions.iter().any(|a| matches!(a, LifecycleAction::MergePR { .. })));
    }

    #[test]
    fn ci_failing_retries_before_force_merge() {
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 2,
            is_copilot: true,
            ci_status: CIStatus::Failing,
            retry_count: 0,
            ..Default::default()
        };
        let result = evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "ci-failing-retry");
    }

    #[test]
    fn ci_failing_force_merges_after_retries() {
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 3,
            is_copilot: true,
            ci_status: CIStatus::Failing,
            retry_count: 2,
            failing_checks: vec!["ci / rust".into()],
            ..Default::default()
        };
        let result = evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "ci-failing-force-merge");
        assert!(result.actions.iter().any(|a| matches!(a, LifecycleAction::MergePR { .. })));
    }

    #[test]
    fn queue_prioritizes_ci_fixes() {
        let issues = vec![
            IssueFacts {
                repo: "plures/test".into(),
                number: 10,
                labels: vec!["enhancement".into()],
                ..Default::default()
            },
            IssueFacts {
                repo: "plures/test".into(),
                number: 11,
                labels: vec!["ci-failure".into()],
                ..Default::default()
            },
        ];
        let result = evaluate_queue(&issues, false).unwrap();
        assert_eq!(result.matched_rule, "queue-ci-failure");
        assert!(matches!(&result.actions[0], LifecycleAction::AssignCopilot { issue_number: 11, .. }));
    }

    #[test]
    fn milestone_complete_triggers_release() {
        let ms = MilestoneFacts {
            repo: "plures/test".into(),
            title: "v1.0.0 — The Replacement".into(),
            number: 1,
            open_issues: 0,
            closed_issues: 10,
            version: Some("1.0.0".into()),
        };
        let result = evaluate_milestone(&ms).unwrap();
        assert_eq!(result.matched_rule, "milestone-complete-release");
        assert!(result.actions.iter().any(|a| matches!(a, LifecycleAction::CreateRelease { .. })));
    }
}
