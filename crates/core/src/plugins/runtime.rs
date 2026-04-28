//! Plugin runtime — loads, manages, and exposes plugins to the agent.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde::Deserialize;

use crate::model::ToolDefinition;
use crate::plugins::crud;
use crate::plugins::error::PluginError;
use crate::plugins::manifest::*;

/// A loaded, running plugin.
#[derive(Debug, Clone)]
pub struct LoadedPlugin {
    pub manifest: PluginManifest,
    pub installed_at: u64,
}

/// The plugin runtime manages all installed plugins.
pub struct PluginRuntime {
    plugins: Arc<RwLock<HashMap<String, LoadedPlugin>>>,
}

impl PluginRuntime {
    /// Create an empty plugin runtime.
    pub fn new() -> Self {
        Self {
            plugins: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Install a plugin from a manifest.
    pub async fn install(&self, manifest: PluginManifest) -> Result<(), PluginError> {
        let name = manifest.name.clone();
        let mut plugins = self.plugins.write().await;
        if plugins.contains_key(&name) {
            return Err(PluginError::AlreadyInstalled(name));
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        plugins.insert(
            name,
            LoadedPlugin {
                manifest,
                installed_at: now,
            },
        );
        Ok(())
    }

    /// Install from a TOML string. The TOML uses `[plugin]` as the top-level
    /// table with `schema`, `logic`, `permissions` as siblings.
    pub async fn install_from_toml(&self, toml_str: &str) -> Result<String, PluginError> {
        let manifest = parse_toml_manifest(toml_str)?;
        let name = manifest.name.clone();
        self.install(manifest).await?;
        Ok(name)
    }

    /// List installed plugins.
    pub async fn list(&self) -> Vec<PluginManifest> {
        self.plugins
            .read()
            .await
            .values()
            .map(|p| p.manifest.clone())
            .collect()
    }

    /// Uninstall a plugin by name.
    pub async fn uninstall(&self, name: &str, _delete_data: bool) -> Result<(), PluginError> {
        let mut plugins = self.plugins.write().await;
        if plugins.remove(name).is_none() {
            return Err(PluginError::NotFound(name.to_string()));
        }
        // TODO: if delete_data, remove PluresDB nodes tagged with this plugin
        Ok(())
    }

    /// Get a specific plugin's manifest.
    pub async fn get(&self, name: &str) -> Option<PluginManifest> {
        self.plugins
            .read()
            .await
            .get(name)
            .map(|p| p.manifest.clone())
    }

    /// Build tool definitions for all installed plugins.
    /// Returns the generic CRUD tools parameterized by the available entity types.
    pub async fn tool_definitions(&self) -> Vec<ToolDefinition> {
        let plugins = self.plugins.read().await;
        let mut entity_types: Vec<String> = Vec::new();
        for plugin in plugins.values() {
            for entity in &plugin.manifest.schema.entities {
                entity_types
                    .push(format!("{}/{}", plugin.manifest.name, entity.name));
            }
        }
        if entity_types.is_empty() {
            return Vec::new();
        }
        crud::tool_definitions(&entity_types)
    }

    /// Generate schema context for system prompt injection.
    pub async fn schema_context(&self) -> String {
        let plugins = self.plugins.read().await;
        if plugins.is_empty() {
            return String::new();
        }
        let mut out = String::from("\n## Installed Plugins\n");
        for plugin in plugins.values() {
            let m = &plugin.manifest;
            out.push_str(&format!(
                "\n### {} (v{})\n{}\n",
                m.name, m.version, m.description
            ));
            if !m.schema.entities.is_empty() {
                out.push_str("Entities: ");
                let entities: Vec<String> = m
                    .schema
                    .entities
                    .iter()
                    .map(|e| {
                        let fields: Vec<&str> =
                            e.fields.iter().map(|f| f.name.as_str()).collect();
                        format!("{} ({})", e.display_name, fields.join(", "))
                    })
                    .collect();
                out.push_str(&entities.join(", "));
                out.push('\n');
            }
            if !m.schema.relationships.is_empty() {
                out.push_str("Relationships: ");
                let rels: Vec<String> = m
                    .schema
                    .relationships
                    .iter()
                    .map(|r| {
                        format!("{} {} {}", r.from_entity, r.name, r.to_entity)
                    })
                    .collect();
                out.push_str(&rels.join(", "));
                out.push('\n');
            }
            out.push_str(
                "Tools: plugin_create, plugin_list, plugin_update, plugin_delete, plugin_search\n",
            );
        }
        out
    }
}

impl Default for PluginRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// ── TOML parsing ─────────────────────────────────────────────────────────────

/// Intermediate TOML structure — the `[plugin]` table lives at the root.
#[derive(Deserialize)]
struct TomlRoot {
    plugin: TomlPlugin,
    #[serde(default)]
    schema: TomlSchema,
    #[serde(default)]
    logic: TomlLogic,
    #[serde(default)]
    permissions: TomlPermissions,
}

#[derive(Deserialize)]
struct TomlPlugin {
    name: String,
    version: String,
    description: String,
    author: Option<String>,
}

#[derive(Default, Deserialize)]
struct TomlSchema {
    #[serde(default)]
    entities: Vec<TomlEntity>,
    #[serde(default)]
    relationships: Vec<RelationshipDefinition>,
}

#[derive(Deserialize)]
struct TomlEntity {
    name: String,
    display_name: String,
    icon: Option<String>,
    #[serde(default)]
    fields: Vec<FieldDefinition>,
}

#[derive(Default, Deserialize)]
struct TomlLogic {
    #[serde(default)]
    rules: Vec<PluginRule>,
    #[serde(default)]
    constraints: Vec<PluginConstraint>,
}

#[derive(Default, Deserialize)]
struct TomlPermissions {
    #[serde(default)]
    pluresdb_scopes: Vec<String>,
    #[serde(default)]
    tool_access: Vec<String>,
    #[serde(default)]
    network: bool,
}

fn parse_toml_manifest(toml_str: &str) -> Result<PluginManifest, PluginError> {
    let root: TomlRoot =
        toml::from_str(toml_str).map_err(|e| PluginError::TomlParse(e.to_string()))?;
    Ok(PluginManifest {
        name: root.plugin.name,
        version: root.plugin.version,
        description: root.plugin.description,
        author: root.plugin.author,
        schema: PluginSchema {
            entities: root
                .schema
                .entities
                .into_iter()
                .map(|e| EntityDefinition {
                    name: e.name,
                    display_name: e.display_name,
                    icon: e.icon,
                    fields: e.fields,
                })
                .collect(),
            relationships: root.schema.relationships,
        },
        logic: PluginLogic {
            rules: root.logic.rules,
            constraints: root.logic.constraints,
            procedures: Vec::new(),
        },
        tools: Vec::new(),
        ui: None,
        permissions: PluginPermissions {
            pluresdb_scopes: root.permissions.pluresdb_scopes,
            tool_access: root.permissions.tool_access,
            network: root.permissions.network,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_and_list() {
        let rt = PluginRuntime::new();
        let manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            description: "Test plugin".into(),
            author: None,
            schema: PluginSchema::default(),
            logic: PluginLogic::default(),
            tools: Vec::new(),
            ui: None,
            permissions: PluginPermissions::default(),
        };
        rt.install(manifest).await.unwrap();
        assert_eq!(rt.list().await.len(), 1);
    }

    #[tokio::test]
    async fn duplicate_install_fails() {
        let rt = PluginRuntime::new();
        let manifest = PluginManifest {
            name: "dupe".into(),
            version: "1.0.0".into(),
            description: "".into(),
            author: None,
            schema: PluginSchema::default(),
            logic: PluginLogic::default(),
            tools: Vec::new(),
            ui: None,
            permissions: PluginPermissions::default(),
        };
        rt.install(manifest.clone()).await.unwrap();
        assert!(rt.install(manifest).await.is_err());
    }

    #[tokio::test]
    async fn uninstall_works() {
        let rt = PluginRuntime::new();
        let manifest = PluginManifest {
            name: "removable".into(),
            version: "1.0.0".into(),
            description: "".into(),
            author: None,
            schema: PluginSchema::default(),
            logic: PluginLogic::default(),
            tools: Vec::new(),
            ui: None,
            permissions: PluginPermissions::default(),
        };
        rt.install(manifest).await.unwrap();
        rt.uninstall("removable", false).await.unwrap();
        assert!(rt.list().await.is_empty());
    }

    #[tokio::test]
    async fn schema_context_generation() {
        let rt = PluginRuntime::new();
        let manifest = PluginManifest {
            name: "inventory".into(),
            version: "1.0.0".into(),
            description: "Track stuff".into(),
            author: None,
            schema: PluginSchema {
                entities: vec![super::super::EntityDefinition {
                    name: "item".into(),
                    display_name: "Item".into(),
                    fields: vec![super::super::FieldDefinition {
                        name: "name".into(),
                        field_type: super::super::FieldType::String,
                        required: true,
                        default: None,
                        description: None,
                    }],
                    icon: Some("📦".into()),
                }],
                relationships: Vec::new(),
            },
            logic: PluginLogic::default(),
            tools: Vec::new(),
            ui: None,
            permissions: PluginPermissions::default(),
        };
        rt.install(manifest).await.unwrap();
        let ctx = rt.schema_context().await;
        assert!(ctx.contains("inventory"));
        assert!(ctx.contains("Item (name)"));
        assert!(ctx.contains("plugin_create"));
    }

    #[tokio::test]
    async fn install_from_toml_works() {
        let toml = r#"
[plugin]
name = "from-toml"
version = "0.1.0"
description = "Parsed from TOML"

[[schema.entities]]
name = "thing"
display_name = "Thing"

[[schema.entities.fields]]
name = "label"
field_type = "String"
required = true

[permissions]
pluresdb_scopes = ["read"]
network = false
"#;
        let rt = PluginRuntime::new();
        let name = rt.install_from_toml(toml).await.unwrap();
        assert_eq!(name, "from-toml");
        let plugins = rt.list().await;
        assert_eq!(plugins[0].schema.entities[0].name, "thing");
    }
}
