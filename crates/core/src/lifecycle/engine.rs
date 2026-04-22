//! Declarative lifecycle rules stored as data, not code.
//!
//! Rules are JSON records in PluresDB. The engine evaluates them against
//! facts using pattern matching. Users create their own rules without
//! writing Rust.
//!
//! ```json
//! {
//!   "id": "merge-on-green",
//!   "name": "Merge when CI green + reviewed",
//!   "priority": 100,
//!   "enabled": true,
//!   "when": {
//!     "ci_status": "green",
//!     "has_review": true,
//!     "is_copilot": true
//!   },
//!   "then": [
//!     { "kind": "approve_pr" },
//!     { "kind": "merge_pr", "method": "squash" }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{LifecycleAction, LifecycleFact, RuleResult};
use super::facts::PRFacts;

/// A declarative lifecycle rule stored in PluresDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Unique rule identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Priority — higher runs first.
    #[serde(default = "default_priority")]
    pub priority: i32,
    /// Whether this rule is active.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Conditions that must ALL be true for the rule to fire.
    pub when: HashMap<String, serde_json::Value>,
    /// Actions to execute when the rule fires.
    pub then: Vec<ActionSpec>,
    /// Optional: facts to capture when the rule fires.
    #[serde(default)]
    pub capture: Vec<FactSpec>,
}

fn default_priority() -> i32 { 50 }
fn default_true() -> bool { true }

/// An action specification in a rule's `then` clause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionSpec {
    /// Action kind (merge_pr, rerun_ci, assign_copilot, etc.)
    pub kind: String,
    /// Optional parameters.
    #[serde(flatten)]
    pub params: HashMap<String, serde_json::Value>,
}

/// A fact capture specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactSpec {
    pub category: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The declarative rule engine.
pub struct RuleEngine {
    rules: Vec<Rule>,
}

impl RuleEngine {
    /// Create a new engine with the given rules, sorted by priority (highest first).
    pub fn new(mut rules: Vec<Rule>) -> Self {
        rules.sort_by_key(|r| std::cmp::Reverse(r.priority));
        Self { rules }
    }

    /// Load rules from a JSON string.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let rules: Vec<Rule> = serde_json::from_str(json)?;
        Ok(Self::new(rules))
    }

    /// Evaluate rules against PR facts. Returns the first matching rule's result.
    pub fn evaluate_pr(&self, facts: &PRFacts) -> RuleResult {
        let fact_map = pr_facts_to_map(facts);

        for rule in &self.rules {
            if !rule.enabled { continue; }
            if matches_conditions(&rule.when, &fact_map) {
                let actions = rule.then.iter()
                    .map(|spec| spec_to_action(spec, facts))
                    .collect();
                let new_facts = rule.capture.iter()
                    .map(|spec| LifecycleFact {
                        category: spec.category.clone(),
                        content: interpolate(&spec.content, facts),
                        tags: spec.tags.clone(),
                    })
                    .collect();
                return RuleResult {
                    actions,
                    new_facts,
                    matched_rule: rule.id.clone(),
                };
            }
        }

        RuleResult {
            actions: vec![LifecycleAction::Noop { reason: "no rule matched".into() }],
            new_facts: vec![],
            matched_rule: "none".into(),
        }
    }

    /// Return all loaded rules.
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }
}

/// Convert PR facts to a string→Value map for condition matching.
fn pr_facts_to_map(facts: &PRFacts) -> HashMap<String, serde_json::Value> {
    let mut map = HashMap::new();
    map.insert("is_copilot".into(), serde_json::json!(facts.is_copilot));
    map.insert("is_draft".into(), serde_json::json!(facts.is_draft));
    map.insert("is_merged".into(), serde_json::json!(facts.is_merged));
    map.insert("mergeable".into(), serde_json::json!(facts.mergeable));
    map.insert("ci_status".into(), serde_json::json!(format!("{:?}", facts.ci_status).to_lowercase()));
    map.insert("has_review".into(), serde_json::json!(facts.has_review));
    map.insert("has_approval".into(), serde_json::json!(facts.has_approval));
    map.insert("retry_count".into(), serde_json::json!(facts.retry_count));
    map.insert("age_minutes".into(), serde_json::json!(facts.age_minutes));
    map
}

/// Check if all conditions match against the fact map.
fn matches_conditions(
    conditions: &HashMap<String, serde_json::Value>,
    facts: &HashMap<String, serde_json::Value>,
) -> bool {
    for (key, expected) in conditions {
        // Support operators: key__gte, key__lte, key__gt, key__lt, key__ne
        if key.contains("__") {
            let parts: Vec<&str> = key.splitn(2, "__").collect();
            let field = parts[0];
            let op = parts[1];
            let actual = match facts.get(field) {
                Some(v) => v,
                None => return false,
            };
            match op {
                "gte" => {
                    if let (Some(a), Some(e)) = (actual.as_u64(), expected.as_u64()) {
                        if a < e { return false; }
                    } else { return false; }
                }
                "lte" => {
                    if let (Some(a), Some(e)) = (actual.as_u64(), expected.as_u64()) {
                        if a > e { return false; }
                    } else { return false; }
                }
                "gt" => {
                    if let (Some(a), Some(e)) = (actual.as_u64(), expected.as_u64()) {
                        if a <= e { return false; }
                    } else { return false; }
                }
                "lt" => {
                    if let (Some(a), Some(e)) = (actual.as_u64(), expected.as_u64()) {
                        if a >= e { return false; }
                    } else { return false; }
                }
                "ne" => {
                    if actual == expected { return false; }
                }
                _ => return false,
            }
        } else {
            match facts.get(key) {
                Some(actual) if actual == expected => {}
                _ => return false,
            }
        }
    }
    true
}

/// Convert an action spec to a concrete LifecycleAction.
fn spec_to_action(spec: &ActionSpec, facts: &PRFacts) -> LifecycleAction {
    match spec.kind.as_str() {
        "merge_pr" => LifecycleAction::MergePR {
            repo: facts.repo.clone(),
            number: facts.number,
            method: spec.params.get("method")
                .and_then(|v| v.as_str())
                .unwrap_or("squash")
                .to_string(),
        },
        "approve_pr" => LifecycleAction::ApprovePR {
            repo: facts.repo.clone(),
            number: facts.number,
        },
        "add_label" => LifecycleAction::AddLabel {
            repo: facts.repo.clone(),
            number: facts.number,
            label: spec.params.get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
        },
        "notify" => LifecycleAction::Notify {
            message: spec.params.get("message")
                .and_then(|v| v.as_str())
                .map(|s| interpolate(s, facts))
                .unwrap_or_default(),
        },
        "noop" => LifecycleAction::Noop {
            reason: spec.params.get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("rule matched")
                .to_string(),
        },
        other => LifecycleAction::Noop {
            reason: format!("unknown action kind: {other}"),
        },
    }
}

/// Simple string interpolation: {{repo}}, {{number}}, etc.
fn interpolate(template: &str, facts: &PRFacts) -> String {
    template
        .replace("{{repo}}", &facts.repo)
        .replace("{{number}}", &facts.number.to_string())
        .replace("{{author}}", &facts.author)
}

// ── Default rules (shipped with pares-agens) ─────────────────────────────────

/// The default lifecycle rules that ship with pares-agens.
/// Users can override or extend these via PluresDB.
pub fn default_rules() -> Vec<Rule> {
    serde_json::from_str(DEFAULT_RULES_JSON).expect("default rules must parse")
}

const DEFAULT_RULES_JSON: &str = r#"[
  {
    "id": "skip-draft",
    "name": "Skip non-Copilot draft PRs",
    "priority": 200,
    "when": { "is_draft": true, "is_copilot": false },
    "then": [{ "kind": "noop", "reason": "draft PR, not Copilot" }]
  },
  {
    "id": "merge-on-green",
    "name": "Merge when CI green + reviewed",
    "priority": 100,
    "when": { "ci_status": "green", "has_review": true, "is_copilot": true },
    "then": [
      { "kind": "approve_pr" },
      { "kind": "merge_pr", "method": "squash" }
    ],
    "capture": [
      { "category": "work-in-progress", "content": "Merged PR #{{number}} on {{repo}}", "tags": ["lifecycle", "merge"] }
    ]
  },
  {
    "id": "ci-green-wait-review",
    "name": "CI green but no review yet — wait",
    "priority": 90,
    "when": { "ci_status": "green", "has_review": false, "is_copilot": true },
    "then": [{ "kind": "noop", "reason": "CI green, waiting for review" }]
  },
  {
    "id": "ci-failing-retry",
    "name": "CI failing — retry (up to 2x)",
    "priority": 80,
    "when": { "ci_status": "failing", "is_copilot": true, "retry_count__lt": 2 },
    "then": [
      { "kind": "add_label", "label": "ci-retry-{{retry_next}}" },
      { "kind": "notify", "message": "PR #{{number}} CI failing — retry {{retry_next}}/2" }
    ]
  },
  {
    "id": "ci-failing-force-merge",
    "name": "CI exhausted retries — force merge",
    "priority": 70,
    "when": { "ci_status": "failing", "is_copilot": true, "retry_count__gte": 2 },
    "then": [
      { "kind": "merge_pr", "method": "squash" },
      { "kind": "notify", "message": "Force-merged PR #{{number}} after 2 retries" }
    ],
    "capture": [
      { "category": "error-fix", "content": "Force-merged PR #{{number}} on {{repo}} after CI failures", "tags": ["lifecycle", "force-merge"] }
    ]
  },
  {
    "id": "ci-pending-wait",
    "name": "CI still running — wait",
    "priority": 60,
    "when": { "ci_status": "pending" },
    "then": [{ "kind": "noop", "reason": "CI still running" }]
  }
]"#;

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::facts::CIStatus;

    #[test]
    fn default_rules_parse() {
        let rules = default_rules();
        assert!(rules.len() >= 6);
    }

    #[test]
    fn engine_merges_on_green() {
        let engine = RuleEngine::new(default_rules());
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 1,
            is_copilot: true,
            ci_status: CIStatus::Green,
            has_review: true,
            ..Default::default()
        };
        let result = engine.evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "merge-on-green");
    }

    #[test]
    fn engine_retries_on_failure() {
        let engine = RuleEngine::new(default_rules());
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 2,
            is_copilot: true,
            ci_status: CIStatus::Failing,
            retry_count: 0,
            ..Default::default()
        };
        let result = engine.evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "ci-failing-retry");
    }

    #[test]
    fn engine_force_merges_after_retries() {
        let engine = RuleEngine::new(default_rules());
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 3,
            is_copilot: true,
            ci_status: CIStatus::Failing,
            retry_count: 2,
            ..Default::default()
        };
        let result = engine.evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "ci-failing-force-merge");
    }

    #[test]
    fn custom_rule_overrides_default() {
        let mut rules = default_rules();
        rules.push(Rule {
            id: "always-merge".into(),
            name: "Always merge Copilot PRs".into(),
            priority: 999, // highest priority
            enabled: true,
            when: HashMap::from([("is_copilot".into(), serde_json::json!(true))]),
            then: vec![ActionSpec {
                kind: "merge_pr".into(),
                params: HashMap::from([("method".into(), serde_json::json!("squash"))]),
            }],
            capture: vec![],
        });

        let engine = RuleEngine::new(rules);
        let facts = PRFacts {
            repo: "plures/test".into(),
            number: 1,
            is_copilot: true,
            ci_status: CIStatus::Failing, // would normally retry
            ..Default::default()
        };
        let result = engine.evaluate_pr(&facts);
        assert_eq!(result.matched_rule, "always-merge"); // custom rule wins
    }
}
