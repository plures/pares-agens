//! Lifecycle — Praxis-driven PR and issue management for the plures org.
//!
//! Replaces the 500+ line JavaScript `copilot-pr-lifecycle.yml` with
//! Rust rules evaluated against PluresDB facts.
//!
//! # Architecture
//!
//! ```text
//! GitHub webhook → pares-agens webhook receiver
//!   → extract facts (PR state, CI status, reviews, labels)
//!   → evaluate Praxis rules against facts
//!   → execute actions (merge, assign, rerun, create issue)
//!   → capture outcomes as new facts
//! ```

pub mod rules;
pub mod facts;
pub mod actions;

use serde::{Deserialize, Serialize};

/// A GitHub event received via webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubEvent {
    /// Event type: pull_request, check_suite, issues, etc.
    pub event_type: String,
    /// Action: opened, closed, completed, submitted, etc.
    pub action: String,
    /// Repository (owner/name).
    pub repo: String,
    /// Raw payload (parsed as needed by rules).
    pub payload: serde_json::Value,
}

/// The result of evaluating lifecycle rules against an event.
#[derive(Debug, Clone)]
pub struct RuleResult {
    /// Actions to execute.
    pub actions: Vec<LifecycleAction>,
    /// Facts to capture from this evaluation.
    pub new_facts: Vec<LifecycleFact>,
    /// Rule that matched.
    pub matched_rule: String,
}

/// An action the lifecycle should execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum LifecycleAction {
    /// Merge a PR.
    MergePR { repo: String, number: u64, method: String },
    /// Rerun failed CI jobs.
    RerunCI { repo: String, run_id: u64 },
    /// Assign Copilot to an issue.
    AssignCopilot { repo: String, issue_number: u64 },
    /// Create a ci-feedback issue.
    CreateCIFeedback { repo: String, pr_number: u64, error_details: String, is_infra: bool },
    /// Close an issue.
    CloseIssue { repo: String, issue_number: u64, reason: String },
    /// Add a label to a PR.
    AddLabel { repo: String, number: u64, label: String },
    /// Remove a label from a PR.
    RemoveLabel { repo: String, number: u64, label: String },
    /// Auto-approve a PR.
    ApprovePR { repo: String, number: u64 },
    /// Create a release.
    CreateRelease { repo: String, tag: String, title: String, body: String },
    /// Send a Telegram notification.
    Notify { message: String },
    /// No action needed.
    Noop { reason: String },
}

/// A fact captured from a lifecycle event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifecycleFact {
    /// Fact category.
    pub category: String,
    /// Fact content.
    pub content: String,
    /// Tags for indexing.
    pub tags: Vec<String>,
}
pub mod engine;
