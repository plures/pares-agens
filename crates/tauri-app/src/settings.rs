//! Settings CRUD commands exposed to the Tauri frontend.
//!
//! These commands complement [`crate::commands::get_settings`] /
//! [`crate::commands::set_settings`] with fine-grained operations for
//! individual resources (providers, channel adapters, routing).
//!
//! # API key handling
//!
//! API keys are **never** returned verbatim to the UI.  Any stored key is
//! replaced by [`MASKED_KEY`] in responses from [`list_providers`].  When the
//! frontend echoes that sentinel back via [`update_provider`], the original
//! value is preserved.  In a full PluresDB integration the keys would also be
//! encrypted at rest.

use tauri::State;

use crate::state::{AppState, ChannelAdapterConfig, ProviderEntry, RoutingPrefs};

/// Sentinel returned in place of a real API key.
const MASKED_KEY: &str = "••••••••";

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// List all configured model providers with API keys masked.
#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let settings = state.settings.lock().await;
    Ok(settings.providers.iter().map(mask_provider).collect())
}

/// Add a new model provider.
///
/// Returns an error if a provider with the same `name` already exists.
#[tauri::command]
pub async fn add_provider(
    provider: ProviderEntry,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    if settings.providers.iter().any(|p| p.name == provider.name) {
        return Err(format!("Provider '{}' already exists", provider.name));
    }
    settings.providers.push(provider);
    Ok(())
}

/// Update an existing provider identified by `name`.
///
/// If the incoming `api_key` equals [`MASKED_KEY`] the existing key is
/// preserved unchanged — this prevents the frontend from accidentally
/// overwriting a key it never received.
#[tauri::command]
pub async fn update_provider(
    name: String,
    provider: ProviderEntry,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    let existing = settings
        .providers
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Provider '{name}' not found"))?;

    // Preserve the stored key if the frontend echoes back the masked sentinel.
    let api_key = resolve_api_key(provider.api_key.as_deref(), &existing.api_key);

    *existing = ProviderEntry {
        name,  // preserve original name — renames are not permitted via this command
        base_url: provider.base_url,
        api_key,
        models: provider.models,
    };
    Ok(())
}

/// Remove a provider by `name`.
///
/// Returns an error if no provider with that name exists.
#[tauri::command]
pub async fn remove_provider(name: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    let before = settings.providers.len();
    settings.providers.retain(|p| p.name != name);
    if settings.providers.len() == before {
        return Err(format!("Provider '{name}' not found"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Channel adapters
// ---------------------------------------------------------------------------

/// Add or update a channel adapter configuration (matched by `kind`).
///
/// If an adapter with the same `kind` already exists it is replaced;
/// otherwise the new entry is appended.
#[tauri::command]
pub async fn upsert_channel_adapter(
    adapter: ChannelAdapterConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    if let Some(existing) = settings
        .channel_adapters
        .iter_mut()
        .find(|a| a.kind == adapter.kind)
    {
        *existing = adapter;
    } else {
        settings.channel_adapters.push(adapter);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Persist updated routing rule preferences.
#[tauri::command]
pub async fn set_routing(routing: RoutingPrefs, state: State<'_, AppState>) -> Result<(), String> {
    state.settings.lock().await.routing = routing;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a JSON representation of a provider with the API key masked.
fn mask_provider(p: &ProviderEntry) -> serde_json::Value {
    serde_json::json!({
        "name":    p.name,
        "baseUrl": p.base_url,
        "apiKey":  p.api_key.as_deref().map(|_| MASKED_KEY),
        "models":  p.models,
    })
}

/// Resolve the API key to store: preserve the existing key when the frontend
/// echoes back the [`MASKED_KEY`] sentinel, otherwise use the new value.
fn resolve_api_key(new_key: Option<&str>, existing_key: &Option<String>) -> Option<String> {
    match new_key {
        Some(k) if k == MASKED_KEY => existing_key.clone(),
        other => other.map(str::to_owned),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(name: &str, key: Option<&str>) -> ProviderEntry {
        ProviderEntry {
            name: name.to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: key.map(str::to_owned),
            models: vec![],
        }
    }

    #[test]
    fn mask_provider_hides_key() {
        let p = make_provider("test", Some("sk-secret"));
        let v = mask_provider(&p);
        assert_eq!(v["apiKey"], serde_json::json!(MASKED_KEY));
        assert_eq!(v["name"], "test");
    }

    #[test]
    fn mask_provider_null_when_no_key() {
        let p = make_provider("test", None);
        let v = mask_provider(&p);
        assert!(v["apiKey"].is_null());
    }

    #[test]
    fn resolve_api_key_preserves_key_on_sentinel() {
        let existing = Some("sk-real-key".to_string());
        let result = resolve_api_key(Some(MASKED_KEY), &existing);
        assert_eq!(result, existing);
    }

    #[test]
    fn resolve_api_key_clears_key_on_empty() {
        let existing = Some("sk-real-key".to_string());
        let result = resolve_api_key(None, &existing);
        assert!(result.is_none());
    }

    #[test]
    fn resolve_api_key_updates_key_on_new_value() {
        let existing = Some("sk-old-key".to_string());
        let result = resolve_api_key(Some("sk-new-key"), &existing);
        assert_eq!(result, Some("sk-new-key".to_string()));
    }
}
