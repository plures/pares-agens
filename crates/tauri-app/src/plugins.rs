//! Tauri commands for plugin management and entity CRUD.

use serde::Serialize;
use tauri::State;

use crate::state::AppState;

/// Summary info returned by `plugin_list`.
#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub entities: Vec<EntityInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityInfo {
    pub name: String,
    pub display_name: String,
    pub fields: Vec<FieldInfo>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldInfo {
    pub name: String,
    pub field_type: String,
    pub required: bool,
    pub description: Option<String>,
}

fn field_type_label(ft: &pares_radix_core::plugins::FieldType) -> String {
    match ft {
        pares_radix_core::plugins::FieldType::String => "String".into(),
        pares_radix_core::plugins::FieldType::Number => "Number".into(),
        pares_radix_core::plugins::FieldType::Date => "Date".into(),
        pares_radix_core::plugins::FieldType::Boolean => "Boolean".into(),
        pares_radix_core::plugins::FieldType::Reference(r) => format!("Reference({r})"),
        pares_radix_core::plugins::FieldType::Enum(vals) => format!("Enum({})", vals.join(",")),
        pares_radix_core::plugins::FieldType::Currency => "Currency".into(),
        pares_radix_core::plugins::FieldType::Image => "Image".into(),
        pares_radix_core::plugins::FieldType::Location => "Location".into(),
    }
}

fn manifest_to_info(m: &pares_radix_core::plugins::PluginManifest) -> PluginInfo {
    PluginInfo {
        name: m.name.clone(),
        version: m.version.clone(),
        description: m.description.clone(),
        entities: m
            .schema
            .entities
            .iter()
            .map(|e| EntityInfo {
                name: e.name.clone(),
                display_name: e.display_name.clone(),
                icon: e.icon.clone(),
                fields: e
                    .fields
                    .iter()
                    .map(|f| FieldInfo {
                        name: f.name.clone(),
                        field_type: field_type_label(&f.field_type),
                        required: f.required,
                        description: f.description.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

// ── Plugin management commands ───────────────────────────────────────────────

#[tauri::command]
pub async fn plugin_install(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let toml_str = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
    let name = state
        .plugin_runtime
        .install_from_toml(&toml_str)
        .await
        .map_err(|e| format!("{e}"))?;
    Ok(name)
}

#[tauri::command]
pub async fn plugin_list(state: State<'_, AppState>) -> Result<Vec<PluginInfo>, String> {
    let manifests = state.plugin_runtime.list().await;
    Ok(manifests.iter().map(manifest_to_info).collect())
}

#[tauri::command]
pub async fn plugin_uninstall(name: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .plugin_runtime
        .uninstall(&name, false)
        .await
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn plugin_schema(name: String, state: State<'_, AppState>) -> Result<String, String> {
    let manifest = state
        .plugin_runtime
        .get(&name)
        .await
        .ok_or_else(|| format!("plugin '{name}' not found"))?;
    serde_json::to_string_pretty(&manifest.schema).map_err(|e| format!("{e}"))
}

// ── Entity CRUD commands ─────────────────────────────────────────────────────

fn get_executor(
    state: &AppState,
) -> Result<&std::sync::Arc<pares_radix_core::plugins::PluginCrudExecutor>, String> {
    state
        .plugin_executor
        .as_ref()
        .ok_or_else(|| "plugin executor not initialised (no PluresDB store)".to_string())
}

#[tauri::command]
pub async fn plugin_crud_create(
    plugin: String,
    entity_type: String,
    fields: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let executor = get_executor(&state)?;
    executor
        .create(&entity_type, &plugin, fields)
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn plugin_crud_list(
    plugin: String,
    entity_type: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let executor = get_executor(&state)?;
    executor
        .list(&entity_type, &plugin, None, 200)
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn plugin_crud_update(
    entity_id: String,
    fields: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let executor = get_executor(&state)?;
    executor
        .update(&entity_id, fields)
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn plugin_crud_delete(
    entity_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let executor = get_executor(&state)?;
    executor.delete(&entity_id).map_err(|e| format!("{e}"))
}

#[tauri::command]
pub async fn plugin_crud_search(
    query: String,
    plugin: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let executor = get_executor(&state)?;
    executor
        .search(&query, &plugin, None, 50)
        .map_err(|e| format!("{e}"))
}
