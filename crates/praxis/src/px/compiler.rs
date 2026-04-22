//! Compiler — transforms a parsed PxDocument into PluresDB records.
//!
//! Each .px primitive becomes a JSON record stored in PluresDB under
//! a namespaced key. The runtime engine reads these records to evaluate
//! rules, check constraints, and execute actions.

use serde_json::json;

use super::{
    FunctionMode, PxConstraint, PxContract, PxDocument, PxFact, PxFunction,
    PxImport, PxRule, PxTrigger,
};

/// A compiled PluresDB record ready for storage.
#[derive(Debug, Clone)]
pub struct CompiledRecord {
    /// PluresDB key (e.g. "px:rule/auto_merge").
    pub key: String,
    /// JSON data to store.
    pub data: serde_json::Value,
    /// Whether this record should be embedded for vector search.
    pub embed: bool,
}

/// Compile a PxDocument into PluresDB records.
pub fn compile(doc: &PxDocument) -> Vec<CompiledRecord> {
    let mut records = Vec::new();

    for import in &doc.imports {
        records.push(compile_import(import));
    }
    for fact in &doc.facts {
        records.push(compile_fact(fact));
    }
    for rule in &doc.rules {
        records.push(compile_rule(rule));
    }
    for constraint in &doc.constraints {
        records.push(compile_constraint(constraint));
    }
    for contract in &doc.contracts {
        records.push(compile_contract(contract));
    }
    for function in &doc.functions {
        records.push(compile_function(function));
    }
    for trigger in &doc.triggers {
        records.push(compile_trigger(trigger));
    }

    records
}

fn compile_import(import: &PxImport) -> CompiledRecord {
    CompiledRecord {
        key: format!("px:import/{}", import.alias.as_deref().unwrap_or(&import.path)),
        data: json!({
            "type": "import",
            "path": import.path,
            "alias": import.alias,
        }),
        embed: false,
    }
}

fn compile_fact(fact: &PxFact) -> CompiledRecord {
    let fields: Vec<serde_json::Value> = fact.fields.iter().map(|f| {
        json!({ "name": f.name, "type": f.type_expr })
    }).collect();

    CompiledRecord {
        key: format!("px:fact/{}", fact.name),
        data: json!({
            "type": "fact",
            "name": fact.name,
            "fields": fields,
        }),
        embed: true, // facts are searchable
    }
}

fn compile_rule(rule: &PxRule) -> CompiledRecord {
    let actions: Vec<serde_json::Value> = rule.actions.iter().map(|a| {
        let mut obj = json!({ "kind": a.kind });
        if let Some(cond) = &a.condition {
            obj["condition"] = json!(cond);
        }
        for (k, v) in &a.params {
            obj[k] = v.clone();
        }
        obj
    }).collect();

    let captures: Vec<serde_json::Value> = rule.captures.iter().map(|c| {
        json!({
            "content": c.content,
            "category": c.category,
            "tags": c.tags,
        })
    }).collect();

    CompiledRecord {
        key: format!("px:rule/{}", rule.name),
        data: json!({
            "type": "rule",
            "name": rule.name,
            "priority": rule.priority.unwrap_or(50),
            "conditions": rule.conditions,
            "lets": rule.lets.iter().map(|(k, v)| json!({"var": k, "expr": v})).collect::<Vec<_>>(),
            "actions": actions,
            "captures": captures,
        }),
        embed: true,
    }
}

fn compile_constraint(constraint: &PxConstraint) -> CompiledRecord {
    CompiledRecord {
        key: format!("px:constraint/{}", constraint.name),
        data: json!({
            "type": "constraint",
            "name": constraint.name,
            "scope": constraint.scope,
            "when": constraint.when_expr,
            "require": constraint.require_expr,
            "severity": constraint.severity,
            "message": constraint.message,
        }),
        embed: true,
    }
}

fn compile_contract(contract: &PxContract) -> CompiledRecord {
    let examples: Vec<serde_json::Value> = contract.examples.iter().map(|e| {
        let mut obj = json!({
            "input": e.input,
            "expect": e.expect,
        });
        if let Some(t) = e.threshold {
            obj["threshold"] = json!(t);
        }
        obj
    }).collect();

    CompiledRecord {
        key: format!("px:contract/{}", contract.name),
        data: json!({
            "type": "contract",
            "name": contract.name,
            "given": contract.given,
            "when": contract.when_desc,
            "then": contract.then_desc,
            "threshold": contract.threshold,
            "examples": examples,
        }),
        embed: true,
    }
}

fn compile_function(function: &PxFunction) -> CompiledRecord {
    let mode = match function.mode {
        FunctionMode::Deterministic => "deterministic",
        FunctionMode::Probabilistic => "probabilistic",
        FunctionMode::Hybrid => "hybrid",
    };

    let params: Vec<serde_json::Value> = function.params.iter().map(|p| {
        json!({ "name": p.name, "type": p.type_expr })
    }).collect();

    CompiledRecord {
        key: format!("px:function/{}", function.name),
        data: json!({
            "type": "function",
            "name": function.name,
            "mode": mode,
            "params": params,
            "return_type": function.return_type,
            "docstring": function.docstring,
        }),
        embed: true, // functions are searchable by description
    }
}

fn compile_trigger(trigger: &PxTrigger) -> CompiledRecord {
    CompiledRecord {
        key: format!("px:trigger/{}", trigger.name),
        data: json!({
            "type": "trigger",
            "name": trigger.name,
            "on": trigger.on_event,
            "schedule": trigger.schedule,
            "run": trigger.run,
        }),
        embed: false,
    }
}

/// Summary of compilation results.
#[derive(Debug)]
pub struct CompileResult {
    pub records: Vec<CompiledRecord>,
    pub stats: CompileStats,
}

#[derive(Debug)]
pub struct CompileStats {
    pub imports: usize,
    pub facts: usize,
    pub rules: usize,
    pub constraints: usize,
    pub contracts: usize,
    pub functions: usize,
    pub triggers: usize,
    pub total: usize,
}

/// Compile with statistics.
pub fn compile_with_stats(doc: &PxDocument) -> CompileResult {
    let records = compile(doc);
    let stats = CompileStats {
        imports: doc.imports.len(),
        facts: doc.facts.len(),
        rules: doc.rules.len(),
        constraints: doc.constraints.len(),
        contracts: doc.contracts.len(),
        functions: doc.functions.len(),
        triggers: doc.triggers.len(),
        total: records.len(),
    };
    CompileResult { records, stats }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::px::{PxAction, PxDocument, PxFact, PxField, PxRule};

    fn empty_doc() -> PxDocument {
        PxDocument {
            imports: vec![], facts: vec![], rules: vec![],
            constraints: vec![], contracts: vec![], functions: vec![],
            triggers: vec![],
        }
    }

    #[test]
    fn compile_empty_doc() {
        let records = compile(&empty_doc());
        assert!(records.is_empty());
    }

    #[test]
    fn compile_fact_produces_record() {
        let mut doc = empty_doc();
        doc.facts.push(PxFact {
            name: "pr_state".into(),
            fields: vec![
                PxField { name: "ci_status".into(), type_expr: "enum(green, failing, pending)".into() },
                PxField { name: "has_review".into(), type_expr: "bool".into() },
            ],
        });

        let records = compile(&doc);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "px:fact/pr_state");
        assert!(records[0].embed);

        let data = &records[0].data;
        assert_eq!(data["type"], "fact");
        assert_eq!(data["fields"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn compile_rule_with_actions() {
        let mut doc = empty_doc();
        doc.rules.push(PxRule {
            name: "auto_merge".into(),
            priority: Some(100),
            conditions: vec!["pr.ci_status == green".into(), "pr.has_review == true".into()],
            lets: vec![],
            actions: vec![
                PxAction {
                    kind: "merge_pr".into(),
                    params: HashMap::from([("method".into(), json!("squash"))]),
                    condition: None,
                },
            ],
            captures: vec![],
        });

        let records = compile(&doc);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].key, "px:rule/auto_merge");

        let data = &records[0].data;
        assert_eq!(data["priority"], 100);
        assert_eq!(data["conditions"].as_array().unwrap().len(), 2);
        assert_eq!(data["actions"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn compile_constraint_includes_severity() {
        let mut doc = empty_doc();
        doc.constraints.push(PxConstraint {
            name: "no_deploy_during_azsecpak".into(),
            scope: None,
            when_expr: "deployment.target == usme".into(),
            require_expr: "deployment.azsecpak_window == false".into(),
            severity: "error".into(),
            message: Some("Cannot deploy during AzSecPak window".into()),
        });

        let records = compile(&doc);
        assert_eq!(records[0].key, "px:constraint/no_deploy_during_azsecpak");
        assert_eq!(records[0].data["severity"], "error");
        assert!(records[0].data["message"].as_str().unwrap().contains("AzSecPak"));
    }

    #[test]
    fn compile_stats_are_accurate() {
        let mut doc = empty_doc();
        doc.facts.push(PxFact { name: "a".into(), fields: vec![] });
        doc.facts.push(PxFact { name: "b".into(), fields: vec![] });
        doc.rules.push(PxRule {
            name: "r".into(), priority: None, conditions: vec![],
            lets: vec![], actions: vec![], captures: vec![],
        });
        doc.constraints.push(PxConstraint {
            name: "c".into(), scope: None, when_expr: "".into(),
            require_expr: "".into(), severity: "warning".into(), message: None,
        });

        let result = compile_with_stats(&doc);
        assert_eq!(result.stats.facts, 2);
        assert_eq!(result.stats.rules, 1);
        assert_eq!(result.stats.constraints, 1);
        assert_eq!(result.stats.total, 4);
    }
}
