//! Facts extracted from GitHub events for rule evaluation.

use serde::{Deserialize, Serialize};

/// PR facts derived from a GitHub pull_request event.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PRFacts {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub is_copilot: bool,
    pub is_draft: bool,
    pub is_merged: bool,
    pub mergeable: bool,
    pub ci_status: CIStatus,
    pub failing_checks: Vec<String>,
    pub retry_count: u32,
    pub has_review: bool,
    pub has_approval: bool,
    pub age_minutes: u64,
    pub labels: Vec<String>,
}

/// Aggregated CI status for a PR.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum CIStatus {
    #[default]
    Pending,
    Green,
    Failing,
    Mixed,
}

/// Issue facts derived from GitHub events.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IssueFacts {
    pub repo: String,
    pub number: u64,
    pub has_label: bool,
    pub has_type: bool,
    pub has_body: bool,
    pub is_copilot_assigned: bool,
    pub milestone: Option<String>,
    pub labels: Vec<String>,
    pub age_minutes: u64,
}

/// Milestone facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MilestoneFacts {
    pub repo: String,
    pub title: String,
    pub number: u64,
    pub open_issues: u32,
    pub closed_issues: u32,
    pub version: Option<String>,
}

/// Classify CI failure as infrastructure or code.
pub fn classify_failure(check_names: &[String], error_output: &str) -> FailureClass {
    let combined = format!("{} {}", check_names.join(" "), error_output).to_lowercase();

    let infra_patterns = [
        "sccache", "cache storage failed", "services aren't available",
        "network", "timeout", "runner", "sigterm", "sigkill",
        "rate limit", "secondary rate", "502", "503",
    ];

    if infra_patterns.iter().any(|p| combined.contains(p)) {
        FailureClass::Infrastructure
    } else {
        FailureClass::Code
    }
}

/// CI failure classification.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureClass {
    Infrastructure,
    Code,
}
