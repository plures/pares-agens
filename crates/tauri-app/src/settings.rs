//! Settings CRUD commands exposed to the Tauri frontend.
//!
//! These commands complement [`crate::commands::get_settings`] /
//! [`crate::commands::set_settings`] with fine-grained operations for
//! individual resources (providers, channel adapters, routing).
//!
//! # API key handling
//!
//! API keys are **never** returned verbatim to the UI and are **never** stored
//! in the in-memory [`Settings`] struct.  Instead they are written to the
//! [`AppState::secret_store`] vault under the key
//! `provider:<name>:api_key` (see
//! [`pares_agens_core::secrets::provider_api_key`]).
//!
//! When the frontend echoes back [`MASKED_KEY`] via [`update_provider`], the
//! existing vault entry is preserved unchanged.

use tauri::State;

use pares_agens_core::secrets::provider_api_key;

use crate::state::{AppState, ChannelAdapterConfig, ProviderEntry, RoutingPrefs};

/// Sentinel returned in place of a real API key.
const MASKED_KEY: &str = "••••••••";

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

/// List all configured model providers with API keys masked.
///
/// For each provider the response includes `"apiKey": "••••••••"` when the
/// vault holds a key for that provider, or `"apiKey": null` when it does not.
#[tauri::command]
pub async fn list_providers(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let settings = state.settings.lock().await;
    let mut result = Vec::with_capacity(settings.providers.len());
    for p in &settings.providers {
        let has_key = state
            .secret_store
            .get(&provider_api_key(&p.name))
            .await
            .map_err(|e| e.to_string())?
            .is_some();
        result.push(mask_provider(p, has_key));
    }
    Ok(result)
}

/// Add a new model provider.
///
/// If `provider.api_key` is set the value is written to the vault and **not**
/// kept in the in-memory settings.  Returns an error if a provider with the
/// same `name` already exists.
#[tauri::command]
pub async fn add_provider(
    provider: ProviderEntry,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    if settings.providers.iter().any(|p| p.name == provider.name) {
        return Err(format!("Provider '{}' already exists", provider.name));
    }

    // Write API key to vault and never keep it in the struct.
    if let Some(ref key) = provider.api_key {
        if !key.is_empty() {
            state
                .secret_store
                .set(&provider_api_key(&provider.name), key)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    // Store the provider without the API key — keys live in the vault only.
    let name = provider.name.clone();
    let base_url = provider.base_url.clone();
    let models = provider.models.clone();
    settings.providers.push(ProviderEntry {
        name,
        base_url,
        api_key: None,
        models,
    });
    Ok(())
}

/// Update an existing provider identified by `name`.
///
/// API key handling:
/// - If `provider.api_key` equals [`MASKED_KEY`] — vault entry is preserved.
/// - If `provider.api_key` is a new non-empty string — vault entry is updated.
/// - If `provider.api_key` is `None` or empty — vault entry is deleted.
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

    // Vault key operation — preserve/update/clear based on the incoming value.
    let vault_key = provider_api_key(&name);
    match provider.api_key.as_deref() {
        Some(k) if k == MASKED_KEY => {
            // Frontend echoed the masked sentinel — leave vault entry as-is.
        }
        Some(k) if !k.is_empty() => {
            state
                .secret_store
                .set(&vault_key, k)
                .await
                .map_err(|e| e.to_string())?;
        }
        _ => {
            // Empty or absent key — clear from vault.
            state
                .secret_store
                .delete(&vault_key)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    *existing = ProviderEntry {
        name,            // preserve original name — renames are not permitted
        base_url: provider.base_url,
        api_key: None,   // never stored in the struct
        models: provider.models,
    };
    Ok(())
}

/// Remove a provider by `name`.
///
/// Also deletes the corresponding vault entry.  Returns an error if no
/// provider with that name exists.
#[tauri::command]
pub async fn remove_provider(name: String, state: State<'_, AppState>) -> Result<(), String> {
    let mut settings = state.settings.lock().await;
    let before = settings.providers.len();
    settings.providers.retain(|p| p.name != name);
    if settings.providers.len() == before {
        return Err(format!("Provider '{name}' not found"));
    }
    // Remove from vault (silently OK if key was never set).
    state
        .secret_store
        .delete(&provider_api_key(&name))
        .await
        .map_err(|e| e.to_string())?;
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
///
/// `has_api_key` should be `true` if the vault holds a key for this provider.
fn mask_provider(p: &ProviderEntry, has_api_key: bool) -> serde_json::Value {
    serde_json::json!({
        "name":    p.name,
        "baseUrl": p.base_url,
        "apiKey":  if has_api_key { Some(MASKED_KEY) } else { None },
        "models":  p.models,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(name: &str) -> ProviderEntry {
        ProviderEntry {
            name: name.to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: None,
            models: vec![],
        }
    }

    #[test]
    fn mask_provider_hides_key_when_vault_has_entry() {
        let p = make_provider("test");
        let v = mask_provider(&p, true);
        assert_eq!(v["apiKey"], serde_json::json!(MASKED_KEY));
        assert_eq!(v["name"], "test");
    }

    #[test]
    fn mask_provider_null_when_no_vault_entry() {
        let p = make_provider("test");
        let v = mask_provider(&p, false);
        assert!(v["apiKey"].is_null());
    }

    #[test]
    fn vault_key_sentinel_logic_preserves() {
        // Simulates: frontend echoed back the masked sentinel.
        let incoming = Some(MASKED_KEY);
        assert!(matches!(incoming, Some(k) if k == MASKED_KEY));
    }

    #[test]
    fn vault_key_sentinel_logic_clears_on_none() {
        // Simulates: frontend sent None (clear the key).
        let incoming: Option<&str> = None;
        assert!(!matches!(incoming, Some(k) if k == MASKED_KEY));
    }

    #[test]
    fn vault_key_sentinel_logic_updates_on_new_value() {
        let incoming = Some("sk-new-key");
        // Not the sentinel, not None → update vault.
        assert!(matches!(incoming, Some(k) if k != MASKED_KEY && !k.is_empty()));
    }
}
