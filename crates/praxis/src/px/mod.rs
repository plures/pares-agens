//! Praxis Intent Language (.px) parser.
//!
//! Parses `.px` files into typed AST nodes using the pest PEG grammar.

pub mod builder;

use pest::Parser;
use pest_derive::Parser;
use serde::{Deserialize, Serialize};

mod parser_impl {
    use super::*;

    #[derive(Parser)]
    #[grammar = "px/grammar.pest"]
    pub struct PxParser;
}

/// A parsed .px document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxDocument {
    /// Imported modules.
    pub imports: Vec<PxImport>,
    /// Declared fact types.
    pub facts: Vec<PxFact>,
    /// Decision rules.
    pub rules: Vec<PxRule>,
    /// Constraint definitions.
    pub constraints: Vec<PxConstraint>,
    /// Behavioral contracts.
    pub contracts: Vec<PxContract>,
    /// Function declarations.
    pub functions: Vec<PxFunction>,
    /// Trigger declarations.
    pub triggers: Vec<PxTrigger>,
}

/// Import statement in a `.px` document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxImport {
    /// Import path.
    pub path: String,
    /// Optional local alias.
    pub alias: Option<String>,
}

/// Fact definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxFact {
    /// Fact name.
    pub name: String,
    /// Typed fields for the fact.
    pub fields: Vec<PxField>,
}

/// Typed fact field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxField {
    /// Field name.
    pub name: String,
    /// Field type expression.
    pub type_expr: String,
}

/// Rule definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxRule {
    /// Rule name.
    pub name: String,
    /// Optional priority (higher runs first).
    pub priority: Option<i32>,
    /// Rule `when` conditions.
    pub conditions: Vec<String>,
    /// Local `let` bindings.
    pub lets: Vec<(String, String)>,
    /// `then` actions.
    pub actions: Vec<PxAction>,
    /// Captured facts on match.
    pub captures: Vec<PxCapture>,
}

/// Action specification within a rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxAction {
    /// Action kind identifier.
    pub kind: String,
    /// Arbitrary action parameters.
    pub params: std::collections::HashMap<String, serde_json::Value>,
    /// Optional action-level condition.
    pub condition: Option<String>,
}

/// Capture specification within a rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxCapture {
    /// Captured content.
    pub content: String,
    /// Optional category.
    pub category: Option<String>,
    /// Capture tags.
    pub tags: Vec<String>,
}

/// Constraint definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxConstraint {
    /// Constraint name.
    pub name: String,
    /// Optional scope (for example `compile`).
    pub scope: Option<String>,
    /// `when` expression.
    pub when_expr: String,
    /// `require` expression.
    pub require_expr: String,
    /// Severity level.
    pub severity: String,
    /// Optional human-readable message.
    pub message: Option<String>,
}

/// Contract definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxContract {
    /// Contract name.
    pub name: String,
    /// `given` description.
    pub given: Option<String>,
    /// `when` description.
    pub when_desc: Option<String>,
    /// `then` description.
    pub then_desc: Option<String>,
    /// Optional passing threshold.
    pub threshold: Option<f64>,
    /// Contract examples.
    pub examples: Vec<PxExample>,
}

/// Contract example.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxExample {
    /// Example input payload.
    pub input: serde_json::Value,
    /// Expected result payload.
    pub expect: serde_json::Value,
    /// Optional example threshold override.
    pub threshold: Option<f64>,
}

/// Function definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxFunction {
    /// Function name.
    pub name: String,
    /// Function parameters.
    pub params: Vec<PxField>,
    /// Return type expression.
    pub return_type: String,
    /// Execution mode.
    pub mode: FunctionMode,
    /// Human-readable function description.
    pub docstring: String,
}

/// Function execution mode.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum FunctionMode {
    /// Deterministic execution.
    #[default]
    Deterministic,
    /// Probabilistic execution.
    Probabilistic,
    /// Hybrid deterministic/probabilistic execution.
    Hybrid,
}

/// Trigger definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PxTrigger {
    /// Trigger name.
    pub name: String,
    /// Event that activates the trigger.
    pub on_event: String,
    /// Optional schedule expression.
    pub schedule: Option<String>,
    /// Target function/procedure to run.
    pub run: String,
}

/// Parse a .px source string into a document AST.
pub fn parse(source: &str) -> Result<PxDocument, String> {
    let pairs = parser_impl::PxParser::parse(parser_impl::Rule::document, source)
        .map_err(|e| format!("parse error: {e}"))?;

    Ok(builder::build(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_compiles() {
        let _ = parser_impl::PxParser::parse(parser_impl::Rule::ident, "hello");
    }

    #[test]
    fn parse_simple_fact() {
        let result = parser_impl::PxParser::parse(parser_impl::Rule::ident, "pr_state");
        assert!(result.is_ok());
    }

    #[test]
    fn parse_constraint_expr() {
        let result =
            parser_impl::PxParser::parse(parser_impl::Rule::expr, "pr.ci_status == green");
        assert!(result.is_ok(), "failed to parse expression");
    }

    #[test]
    fn parse_value_types() {
        assert!(parser_impl::PxParser::parse(parser_impl::Rule::value, "\"hello\"").is_ok());
        assert!(parser_impl::PxParser::parse(parser_impl::Rule::value, "42").is_ok());
        assert!(parser_impl::PxParser::parse(parser_impl::Rule::value, "3.14").is_ok());
        assert!(parser_impl::PxParser::parse(parser_impl::Rule::value, "true").is_ok());
        assert!(parser_impl::PxParser::parse(parser_impl::Rule::value, "false").is_ok());
    }

    #[test]
    fn parse_plures_dev_guide_constraints_file() {
        let source = include_str!("../../../../praxis/plures-dev-guide.px");
        let doc = parse(source).expect("plures-dev-guide.px should parse");
        assert!(!doc.constraints.is_empty(), "expected at least one constraint");
    }

}
pub mod compiler;
