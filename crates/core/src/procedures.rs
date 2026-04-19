//! Default PluresLM procedure library shipped with pares-agens.
//!
//! These bundled JSON procedure groups are intended to be imported into
//! PluresDB-backed deployments on first run. Consumers can apply
//! [`DefaultProcedureLoadConfig`] to disable specific default procedures.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Single bundled JSON file of default procedures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultProcedureBundle {
    /// Stable bundle identifier.
    pub name: &'static str,
    /// Source file name in `src/procedures/defaults`.
    pub file_name: &'static str,
    /// Raw JSON content.
    pub json: &'static str,
}

/// Flattened default procedure entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefaultProcedure {
    /// Procedure name.
    pub name: String,
    /// Bundle identifier the procedure came from.
    pub bundle: String,
    /// Procedure JSON definition.
    pub definition: serde_json::Value,
}

/// Configuration used when loading bundled defaults.
///
/// Persist this (for example in PluresDB state) to disable specific shipped
/// procedure names while still loading the rest of the default library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultProcedureLoadConfig {
    /// Procedure names to skip when loading defaults.
    #[serde(default)]
    pub disabled: BTreeSet<String>,
}

impl DefaultProcedureLoadConfig {
    /// Returns `true` when `procedure_name` should be loaded.
    pub fn is_enabled(&self, procedure_name: &str) -> bool {
        !self.disabled.contains(procedure_name)
    }
}

const MEMORY_HYGIENE_JSON: &str = include_str!("procedures/defaults/memory-hygiene.json");
const KNOWLEDGE_SYNTHESIS_JSON: &str = include_str!("procedures/defaults/knowledge-synthesis.json");
const TASK_LIFECYCLE_JSON: &str = include_str!("procedures/defaults/task-lifecycle.json");
const AGENT_ORCHESTRATION_JSON: &str = include_str!("procedures/defaults/agent-orchestration.json");
const QUALITY_GATES_JSON: &str = include_str!("procedures/defaults/quality-gates.json");

const DEFAULT_BUNDLES: [DefaultProcedureBundle; 5] = [
    DefaultProcedureBundle {
        name: "memory-hygiene",
        file_name: "memory-hygiene.json",
        json: MEMORY_HYGIENE_JSON,
    },
    DefaultProcedureBundle {
        name: "knowledge-synthesis",
        file_name: "knowledge-synthesis.json",
        json: KNOWLEDGE_SYNTHESIS_JSON,
    },
    DefaultProcedureBundle {
        name: "task-lifecycle",
        file_name: "task-lifecycle.json",
        json: TASK_LIFECYCLE_JSON,
    },
    DefaultProcedureBundle {
        name: "agent-orchestration",
        file_name: "agent-orchestration.json",
        json: AGENT_ORCHESTRATION_JSON,
    },
    DefaultProcedureBundle {
        name: "quality-gates",
        file_name: "quality-gates.json",
        json: QUALITY_GATES_JSON,
    },
];

/// Return all shipped default procedure bundles.
pub fn default_procedure_bundles() -> &'static [DefaultProcedureBundle] {
    &DEFAULT_BUNDLES
}

/// Return a single shipped bundle by its identifier.
pub fn default_procedure_bundle(name: &str) -> Option<&'static DefaultProcedureBundle> {
    DEFAULT_BUNDLES.iter().find(|bundle| bundle.name == name)
}

/// Load all default procedures from bundled JSON, applying `config.disabled`.
pub fn load_default_procedures(
    config: &DefaultProcedureLoadConfig,
) -> Result<Vec<DefaultProcedure>, serde_json::Error> {
    let mut loaded = Vec::new();

    for bundle in default_procedure_bundles() {
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(bundle.json)?;
        for (name, definition) in map {
            if config.is_enabled(&name) {
                loaded.push(DefaultProcedure {
                    name,
                    bundle: bundle.name.to_string(),
                    definition,
                });
            }
        }
    }

    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_defaults_match_expected_files() {
        let files: Vec<_> = default_procedure_bundles()
            .iter()
            .map(|b| b.file_name)
            .collect();
        assert_eq!(
            files,
            vec![
                "memory-hygiene.json",
                "knowledge-synthesis.json",
                "task-lifecycle.json",
                "agent-orchestration.json",
                "quality-gates.json"
            ]
        );
    }

    #[test]
    fn bundled_default_json_parses() {
        for bundle in default_procedure_bundles() {
            let parsed: serde_json::Value = serde_json::from_str(bundle.json).unwrap();
            assert!(
                parsed.is_object(),
                "{} must be a JSON object",
                bundle.file_name
            );
        }
    }

    #[test]
    fn load_default_procedures_respects_disabled_config() {
        let mut config = DefaultProcedureLoadConfig::default();
        config.disabled.insert("staleness-on-access".to_string());

        let loaded = load_default_procedures(&config).unwrap();

        assert!(!loaded.iter().any(|p| p.name == "staleness-on-access"));
        assert!(loaded.iter().any(|p| p.name == "issue-quality-gate"));
    }
}
