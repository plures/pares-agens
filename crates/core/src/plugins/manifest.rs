//! Plugin manifest types — the declaration of what a plugin provides.

use serde::{Deserialize, Serialize};

/// A plugin manifest — the full declaration of a plugin's capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin name (kebab-case).
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: Option<String>,

    /// PluresDB schema definitions.
    #[serde(default)]
    pub schema: PluginSchema,

    /// Praxis rules and constraints.
    #[serde(default)]
    pub logic: PluginLogic,

    /// Custom tools the plugin provides to the agent.
    #[serde(default)]
    pub tools: Vec<PluginTool>,

    /// UI component paths (Svelte, for design-dojo).
    #[serde(default)]
    pub ui: Option<PluginUI>,

    /// Permissions the plugin requires.
    #[serde(default)]
    pub permissions: PluginPermissions,
}

// ── Schema ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginSchema {
    /// Entity types this plugin manages.
    #[serde(default)]
    pub entities: Vec<EntityDefinition>,
    /// Relationships between entities.
    #[serde(default)]
    pub relationships: Vec<RelationshipDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityDefinition {
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub name: String,
    pub field_type: FieldType,
    #[serde(default)]
    pub required: bool,
    pub default: Option<serde_json::Value>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldType {
    String,
    Number,
    Date,
    Boolean,
    /// Reference to another entity type by name.
    Reference(std::string::String),
    /// Allowed values.
    Enum(Vec<std::string::String>),
    Currency,
    /// URL or file path.
    Image,
    /// Lat/lng or address string.
    Location,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipDefinition {
    pub name: String,
    pub from_entity: String,
    pub to_entity: String,
    /// `"many_to_one"`, `"one_to_many"`, `"many_to_many"`.
    pub cardinality: String,
}

// ── Logic ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginLogic {
    #[serde(default)]
    pub rules: Vec<PluginRule>,
    #[serde(default)]
    pub constraints: Vec<PluginConstraint>,
    #[serde(default)]
    pub procedures: Vec<PluginProcedure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginRule {
    pub name: String,
    pub description: String,
    /// DSL or natural language condition.
    pub condition: String,
    /// What happens when condition is met.
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConstraint {
    pub name: String,
    pub description: String,
    /// Validation expression.
    pub check: String,
    pub error_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginProcedure {
    pub name: String,
    pub description: String,
    /// Event that triggers this procedure.
    pub trigger: String,
    /// Steps to execute.
    pub steps: Vec<serde_json::Value>,
}

// ── Tools ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub parameters: Vec<ToolParameter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    pub param_type: String,
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

// ── UI ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginUI {
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub routes: Vec<UIRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UIRoute {
    pub path: String,
    pub component: String,
    pub label: String,
}

// ── Permissions ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginPermissions {
    #[serde(default)]
    pub pluresdb_scopes: Vec<String>,
    #[serde(default)]
    pub tool_access: Vec<String>,
    #[serde(default)]
    pub network: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_manifest_from_json() {
        let json = serde_json::json!({
            "name": "test-plugin",
            "version": "0.1.0",
            "description": "A test plugin",
            "schema": {
                "entities": [{
                    "name": "widget",
                    "display_name": "Widget",
                    "fields": [{
                        "name": "color",
                        "field_type": "String",
                        "required": true
                    }]
                }],
                "relationships": []
            },
            "logic": { "rules": [], "constraints": [], "procedures": [] },
            "tools": [],
            "permissions": { "pluresdb_scopes": ["read"], "tool_access": [], "network": false }
        });
        let manifest: PluginManifest = serde_json::from_value(json).unwrap();
        assert_eq!(manifest.name, "test-plugin");
        assert_eq!(manifest.schema.entities.len(), 1);
        assert_eq!(manifest.schema.entities[0].fields[0].name, "color");
    }

    #[test]
    fn parse_enum_field_type() {
        let json = serde_json::json!({
            "name": "status",
            "field_type": { "Enum": ["active", "archived"] },
            "required": false
        });
        let field: FieldDefinition = serde_json::from_value(json).unwrap();
        match &field.field_type {
            FieldType::Enum(vals) => assert_eq!(vals, &["active", "archived"]),
            _ => panic!("expected Enum"),
        }
    }
}
