//! Generated-contract surface for the checked autonomous spine procedures.
//!
//! This module is the single declaration of the narrow task-dispatch host
//! boundary. Procedure policy stays in `.px`; these entries describe only the
//! concrete actions the runtime can supply.

use px_check::{CallableContract, ContractCatalog, ExecutionProfile, SchemaRef, StaticType};
use pluresdb_px::px::pxlang::px_ast::{BaseType, TypeExpr};

pub const SPINE_SCHEMA_ID: &str = "pares-agens.spine";
pub const SPINE_SCHEMA_VERSION: u32 = 1;
pub const SPINE_SCHEMA_FINGERPRINT: &str = "autonomous-dispatch-v1";

fn string() -> TypeExpr {
    TypeExpr::Base(BaseType::String)
}

fn integer() -> TypeExpr {
    TypeExpr::Base(BaseType::Int)
}

fn action(params: impl IntoIterator<Item = (&'static str, TypeExpr)>) -> CallableContract {
    params.into_iter().fold(
        CallableContract::new(StaticType::Any),
        |contract, (name, value_type)| contract.required(name, value_type),
    )
}

fn dynamic_action(params: impl IntoIterator<Item = &'static str>) -> CallableContract {
    params
        .into_iter()
        .fold(CallableContract::new(StaticType::Any), |contract, name| {
            contract.required_any(name)
        })
}

/// Contract consumed by both static QA and the PluresDB activation gate.
pub fn autonomous_dispatch_catalog() -> ContractCatalog {
    let mut catalog = ContractCatalog::with_schema(SchemaRef::new(
        SPINE_SCHEMA_ID,
        SPINE_SCHEMA_VERSION,
        SPINE_SCHEMA_FINGERPRINT,
    ));
    catalog.insert("task_list_evaluable_graph", CallableContract::new(StaticType::Any));
    catalog.insert("timestamp_now", CallableContract::new(StaticType::Known(integer())));
    catalog.insert(
        "get_field",
        dynamic_action(["object"]).required("field", string()),
    );
    catalog.insert(
        "compute_elapsed",
        CallableContract::new(StaticType::Known(integer()))
            .required("start", integer())
            .required("end", integer()),
    );
    catalog.insert("sort_by", dynamic_action(["items", "keys", "orders"]));
    catalog.insert("get_first_item", dynamic_action(["list"]));
    catalog.insert(
        "mark_task_in_progress",
        action([("task_id", string())]),
    );
    catalog.insert(
        "dispatch_task",
        action([("task_id", string()), ("prompt", string())]),
    );
    catalog.insert(
        "format_string",
        CallableContract::new(StaticType::Known(string()))
            .required("template", string())
            .required_any("vars"),
    );
    // Procedure-to-procedure calls are derived from this same source document
    // by px-check. Their declared PX types therefore remain authoritative.
    catalog
}

/// The current executor must preserve complete values passed to an action.
pub const AUTONOMOUS_DISPATCH_PROFILE: ExecutionProfile = ExecutionProfile::ACCESSOR_VALUES;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autonomous_dispatch_source_matches_the_live_spine_contract() {
        let source = include_str!("../../../../praxis/procedures/autonomous-dispatch.px");
        let report = pluresdb_px::px::compiler::validate_and_compile_checked(
            source,
            &autonomous_dispatch_catalog(),
            AUTONOMOUS_DISPATCH_PROFILE,
        );

        assert!(
            report.is_activatable(),
            "autonomous-dispatch must be activatable before it can enter the Spine:\n{}",
            report
                .diagnostics
                .iter()
                .map(|diagnostic| format!(
                    "{} {} step {}: {}",
                    diagnostic.code,
                    diagnostic.procedure,
                    diagnostic.step,
                    diagnostic.message
                ))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
