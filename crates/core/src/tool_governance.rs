//! Tool execution governance — policy checks, timeouts, and blocked-command filtering.
//!
//! Every tool call passes through [`ToolGovernor`] before execution.  The
//! governor loads [`ToolPolicy`] records (from defaults or PluresDB) and
//! enforces:
//!
//! * **Blocked patterns** — commands matching any pattern are rejected immediately.
//! * **Approval gates** — tools marked `approval_required` log a warning and
//!   proceed (full approval UI is Phase 5+).
//! * **Timeouts** — callers use [`ToolPolicy::timeout`] to wrap execution.
//!
//! # Event spine integration
//!
//! The governor emits structured log events.  Callers that hold an
//! [`EventSpine`] reference should emit `ToolBlocked` / `ToolExecutionComplete`
//! events after checking the governor verdict.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{info, warn};

/// Policy governing a single tool's execution constraints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPolicy {
    /// Tool name this policy applies to (e.g. `"run_command"`).
    pub tool_name: String,
    /// Whether user confirmation is required before executing.
    pub approval_required: bool,
    /// Maximum execution time in milliseconds.
    pub timeout_ms: u64,
    /// Whether the tool should run in a restricted environment.
    pub sandboxed: bool,
    /// Substring patterns that are allowed (empty = allow all).
    pub allowed_patterns: Vec<String>,
    /// Substring patterns that block execution immediately.
    pub blocked_patterns: Vec<String>,
}

impl ToolPolicy {
    /// Timeout as a [`Duration`].
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

/// Result of a pre-execution governance check.
#[derive(Debug, Clone)]
pub enum GovernanceVerdict {
    /// Proceed with execution.
    Allow,
    /// Proceed but log that approval was required (Phase 5+ will block).
    AllowWithApprovalWarning,
    /// Blocked — do not execute.  Contains the matched pattern.
    Blocked { pattern: String },
}

/// Central governance engine for tool execution.
pub struct ToolGovernor {
    policies: HashMap<String, ToolPolicy>,
}

impl ToolGovernor {
    /// Create a governor with default policies for all built-in tools.
    pub fn with_defaults() -> Self {
        let defaults = vec![
            ToolPolicy {
                tool_name: "run_command".into(),
                approval_required: false,
                timeout_ms: 30_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![
                    "rm -rf /".into(),
                    "mkfs".into(),
                    "dd if=".into(),
                    "> /dev/".into(),
                    ":(){ :|:& };:".into(),
                ],
            },
            ToolPolicy {
                tool_name: "write_file".into(),
                approval_required: false,
                timeout_ms: 10_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
            ToolPolicy {
                tool_name: "read_file".into(),
                approval_required: false,
                timeout_ms: 10_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
            ToolPolicy {
                tool_name: "edit_file".into(),
                approval_required: false,
                timeout_ms: 10_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
            ToolPolicy {
                tool_name: "list_directory".into(),
                approval_required: false,
                timeout_ms: 10_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
            ToolPolicy {
                tool_name: "web_fetch".into(),
                approval_required: false,
                timeout_ms: 15_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
            ToolPolicy {
                tool_name: "web_search".into(),
                approval_required: false,
                timeout_ms: 10_000,
                sandboxed: false,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
        ];

        let policies = defaults
            .into_iter()
            .map(|p| (p.tool_name.clone(), p))
            .collect();

        Self { policies }
    }

    /// Register or update a policy for a tool.
    pub fn set_policy(&mut self, policy: ToolPolicy) {
        self.policies.insert(policy.tool_name.clone(), policy);
    }

    /// Get the policy for a tool, or a permissive default if none is registered.
    pub fn policy_for(&self, tool_name: &str) -> ToolPolicy {
        self.policies.get(tool_name).cloned().unwrap_or(ToolPolicy {
            tool_name: tool_name.to_string(),
            approval_required: false,
            timeout_ms: 30_000,
            sandboxed: false,
            allowed_patterns: vec![],
            blocked_patterns: vec![],
        })
    }

    /// All registered policies.
    pub fn all_policies(&self) -> Vec<&ToolPolicy> {
        self.policies.values().collect()
    }

    /// Check whether a tool call should proceed.
    ///
    /// For `run_command`, `arguments_str` should be the command string.
    /// For other tools, pass the serialized arguments JSON.
    pub fn check(&self, tool_name: &str, arguments_str: &str) -> GovernanceVerdict {
        let policy = self.policy_for(tool_name);

        // Check blocked patterns
        for pattern in &policy.blocked_patterns {
            if arguments_str.contains(pattern.as_str()) {
                warn!(
                    tool = tool_name,
                    pattern = pattern.as_str(),
                    "tool call blocked by governance policy"
                );
                return GovernanceVerdict::Blocked {
                    pattern: pattern.clone(),
                };
            }
        }

        // Check approval requirement (Phase 5+ will actually block here)
        if policy.approval_required {
            info!(
                tool = tool_name,
                "tool requires approval — proceeding with warning (approval UI is Phase 5+)"
            );
            return GovernanceVerdict::AllowWithApprovalWarning;
        }

        GovernanceVerdict::Allow
    }

    /// Format all policies for display (e.g. `/tools` command).
    pub fn format_policies(&self) -> String {
        let mut output = String::from("Tool Governance Policies\n");
        let mut policies: Vec<_> = self.policies.values().collect();
        policies.sort_by_key(|p| &p.tool_name);

        for policy in policies {
            output.push_str(&format!(
                "\n📎 {}\n  Timeout: {}s | Approval: {} | Sandboxed: {}\n",
                policy.tool_name,
                policy.timeout_ms / 1000,
                if policy.approval_required {
                    "required"
                } else {
                    "no"
                },
                if policy.sandboxed { "yes" } else { "no" },
            ));
            if !policy.blocked_patterns.is_empty() {
                output.push_str(&format!(
                    "  Blocked: {}\n",
                    policy.blocked_patterns.join(", ")
                ));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policies_exist() {
        let gov = ToolGovernor::with_defaults();
        let policy = gov.policy_for("run_command");
        assert_eq!(policy.timeout_ms, 30_000);
        assert!(!policy.blocked_patterns.is_empty());
    }

    #[test]
    fn blocked_pattern_rejects() {
        let gov = ToolGovernor::with_defaults();
        match gov.check("run_command", "rm -rf /") {
            GovernanceVerdict::Blocked { pattern } => {
                assert_eq!(pattern, "rm -rf /");
            }
            _ => panic!("expected Blocked verdict"),
        }
    }

    #[test]
    fn safe_command_allowed() {
        let gov = ToolGovernor::with_defaults();
        assert!(matches!(
            gov.check("run_command", "ls -la"),
            GovernanceVerdict::Allow
        ));
    }

    #[test]
    fn unknown_tool_gets_permissive_default() {
        let gov = ToolGovernor::with_defaults();
        let policy = gov.policy_for("some_new_tool");
        assert_eq!(policy.timeout_ms, 30_000);
        assert!(!policy.approval_required);
    }

    #[test]
    fn approval_required_returns_warning() {
        let mut gov = ToolGovernor::with_defaults();
        gov.set_policy(ToolPolicy {
            tool_name: "dangerous_tool".into(),
            approval_required: true,
            timeout_ms: 5_000,
            sandboxed: true,
            allowed_patterns: vec![],
            blocked_patterns: vec![],
        });
        assert!(matches!(
            gov.check("dangerous_tool", "anything"),
            GovernanceVerdict::AllowWithApprovalWarning
        ));
    }

    #[test]
    fn fork_bomb_blocked() {
        let gov = ToolGovernor::with_defaults();
        assert!(matches!(
            gov.check("run_command", ":(){ :|:& };:"),
            GovernanceVerdict::Blocked { .. }
        ));
    }

    #[test]
    fn format_policies_includes_all_tools() {
        let gov = ToolGovernor::with_defaults();
        let output = gov.format_policies();
        assert!(output.contains("run_command"));
        assert!(output.contains("read_file"));
        assert!(output.contains("web_fetch"));
    }
}
