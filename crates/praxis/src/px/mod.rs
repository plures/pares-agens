//! Praxis Intent Language (.px) parser.
//!
//! Parses `.px` files into typed AST nodes using the pest PEG grammar.

pub mod builder;

use pest::Parser;
use pest_derive::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[grammar = "px/grammar.pest"]
pub struct PxParser;

/// A parsed .px document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxDocument {
    pub imports: Vec<PxImport>,
    pub facts: Vec<PxFact>,
    pub rules: Vec<PxRule>,
    pub constraints: Vec<PxConstraint>,
    pub contracts: Vec<PxContract>,
    pub functions: Vec<PxFunction>,
    pub triggers: Vec<PxTrigger>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxImport {
    pub path: String,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxFact {
    pub name: String,
    pub fields: Vec<PxField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxField {
    pub name: String,
    pub type_expr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxRule {
    pub name: String,
    pub priority: Option<i32>,
    pub conditions: Vec<String>,
    pub lets: Vec<(String, String)>,
    pub actions: Vec<PxAction>,
    pub captures: Vec<PxCapture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxAction {
    pub kind: String,
    pub params: std::collections::HashMap<String, serde_json::Value>,
    pub condition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxCapture {
    pub content: String,
    pub category: Option<String>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxConstraint {
    pub name: String,
    pub scope: Option<String>,
    pub when_expr: String,
    pub require_expr: String,
    pub severity: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxContract {
    pub name: String,
    pub given: Option<String>,
    pub when_desc: Option<String>,
    pub then_desc: Option<String>,
    pub threshold: Option<f64>,
    pub examples: Vec<PxExample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxExample {
    pub input: serde_json::Value,
    pub expect: serde_json::Value,
    pub threshold: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxFunction {
    pub name: String,
    pub params: Vec<PxField>,
    pub return_type: String,
    pub mode: FunctionMode,
    pub docstring: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum FunctionMode {
    #[default]
    Deterministic,
    Probabilistic,
    Hybrid,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxTrigger {
    pub name: String,
    pub on_event: String,
    pub schedule: Option<String>,
    pub run: String,
}

/// Parse a .px source string into a document AST.
pub fn parse(source: &str) -> Result<PxDocument, String> {
    let pairs = PxParser::parse(Rule::document, source)
        .map_err(|e| format!("parse error: {e}"))?;

    Ok(builder::build(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_compiles() {
        let _ = PxParser::parse(Rule::ident, "hello");
    }

    #[test]
    fn parse_simple_fact() {
        let result = PxParser::parse(Rule::ident, "pr_state");
        assert!(result.is_ok());
    }

    #[test]
    fn parse_constraint_expr() {
        let result = PxParser::parse(Rule::expr, "pr.ci_status == green");
        assert!(result.is_ok(), "failed to parse expression");
    }

    #[test]
    fn parse_value_types() {
        assert!(PxParser::parse(Rule::value, "\"hello\"").is_ok());
        assert!(PxParser::parse(Rule::value, "42").is_ok());
        assert!(PxParser::parse(Rule::value, "3.14").is_ok());
        assert!(PxParser::parse(Rule::value, "true").is_ok());
        assert!(PxParser::parse(Rule::value, "false").is_ok());
    }

    #[test]
    fn parse_document_extracts_rule_constraint_and_contract() {
        let source = r#"
fact pr_state:
  ci_status: enum(green, failing, pending)
  has_review: bool

rule auto_merge:
  when:
    - pr_state.ci_status == green
    - pr_state.has_review == true
  then:
    - action: merge_pr method: "squash"
  capture:
    - fact: "Merged PR" category: work_in_progress tags: ["lifecycle", "merge"]

constraint merge_requires_review:
  when: pr_state.ci_status == green
  require: pr_state.has_review == true
  severity: error

contract auto_merge_behavior:
  given: "CI green + reviewed"
  when: "lifecycle evaluates"
  then: "PR merged"
  examples:
    - input: { ci_status: green, has_review: true }
      expect: [merge_pr]
"#;

        let document = parse(source).expect("expected valid .px document");
        assert_eq!(document.facts.len(), 1);
        assert_eq!(document.rules.len(), 1);
        assert_eq!(document.constraints.len(), 1);
        assert_eq!(document.contracts.len(), 1);

        let rule = &document.rules[0];
        assert_eq!(rule.conditions.len(), 2);
        assert_eq!(rule.actions.len(), 1);
        assert_eq!(rule.captures.len(), 1);
        assert_eq!(rule.captures[0].content, "Merged PR");
        assert_eq!(rule.captures[0].category.as_deref(), Some("work_in_progress"));
        assert_eq!(rule.captures[0].tags, vec!["lifecycle", "merge"]);
    }
}
pub mod compiler;
