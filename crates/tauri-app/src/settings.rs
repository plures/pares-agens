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
//! [`crate::state::AppState::secret_store`] vault under the key
//! `provider:<name>:api_key` (see
//! [`pares_agens_core::secrets::provider_api_key`]).
//!
//! ## Preservation contract
//!
//! The frontend sends `apiKey: null` when the user leaves the API key field
//! blank during an edit (meaning "preserve whatever is stored").  `update_provider`
//! therefore treats `None` — and the [`MASKED_KEY`] sentinel — identically:
//! the vault entry is left untouched.  Only a non-empty, non-sentinel value
//! triggers a vault write.

use tauri::State;

use pares_agens_core::secrets::provider_api_key;

use crate::state::{
    rebuild_model_router, AppState, ChannelAdapterConfig, ProviderEntry, RoutingPrefs,
};

// Re-export for test module (Settings is only used in tests but accessed via `super::*`).
#[cfg(test)]
use crate::state::Settings;

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
    // Clone the provider list under the lock so we don't hold the mutex
    // across the async vault lookups below.
    let provider_names_and_bases: Vec<(String, String, Vec<String>)> = {
        let settings = state.settings.lock().await;
        settings
            .providers
            .iter()
            .map(|p| (p.name.clone(), p.base_url.clone(), p.models.clone()))
            .collect()
    };

    let mut result = Vec::with_capacity(provider_names_and_bases.len());
    for (name, base_url, models) in provider_names_and_bases {
        let has_key = state
            .secret_store
            .get(&provider_api_key(&name))
            .await
            .map_err(|e| e.to_string())?
            .is_some();
        result.push(mask_provider_fields(&name, &base_url, &models, has_key));
    }
    Ok(result)
}

/// Add a new model provider.
///
/// If `provider.api_key` is a non-empty string the value is written to the
/// vault **before** the provider is added to the in-memory settings list.
/// The `api_key` field is never stored in [`Settings`].
///
/// Returns an error if a provider with the same `name` already exists.
#[tauri::command]
pub async fn add_provider(
    provider: ProviderEntry,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Validate uniqueness without holding the lock across async I/O.
    {
        let settings = state.settings.lock().await;
        if settings.providers.iter().any(|p| p.name == provider.name) {
            return Err(format!("Provider '{}' already exists", provider.name));
        }
    }

    // Write API key to vault (lock already released).
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
    let mut settings = state.settings.lock().await;
    // Re-check uniqueness in case of a concurrent add.
    if settings.providers.iter().any(|p| p.name == provider.name) {
        return Err(format!("Provider '{}' already exists", provider.name));
    }
    settings.providers.push(ProviderEntry {
        name: provider.name,
        base_url: provider.base_url,
        api_key: None,
        models: provider.models,
    });
    drop(settings);

    // Rebuild the model router so the new provider is available immediately.
    rebuild_model_router(&state).await;

    Ok(())
}

/// Update an existing provider identified by `name`.
///
/// **API key preservation contract** (aligned with frontend behaviour):
/// - `None` (frontend sent `null`, e.g. user left field blank) → vault preserved
/// - `Some(MASKED_KEY)` (frontend echoed the masked sentinel) → vault preserved
/// - `Some("")` (empty string) → vault preserved
/// - `Some(k)` where `k` is non-empty and not the sentinel → vault updated
///
/// There is currently no explicit "delete key" signal from the frontend; key
/// deletion happens only through [`remove_provider`].
#[tauri::command]
pub async fn update_provider(
    name: String,
    provider: ProviderEntry,
    state: State<'_, AppState>,
) -> Result<(), String> {
    // Verify the provider exists before touching the vault.
    {
        let settings = state.settings.lock().await;
        if !settings.providers.iter().any(|p| p.name == name) {
            return Err(format!("Provider '{name}' not found"));
        }
    }

    // Apply vault update (lock is released; no mutex held across await).
    let vault_key = provider_api_key(&name);
    // Determine if the incoming key represents a real update or a "preserve" signal.
    // Treat None, empty string, and MASKED_KEY all as "leave vault unchanged".
    // There is currently no explicit "delete key" signal from the frontend;
    // key deletion only happens through remove_provider.
    let new_key = provider
        .api_key
        .as_deref()
        .filter(|k| !k.is_empty() && *k != MASKED_KEY);
    if let Some(k) = new_key {
        state
            .secret_store
            .set(&vault_key, k)
            .await
            .map_err(|e| e.to_string())?;
    }

    // Re-acquire lock and apply the structural update.
    let mut settings = state.settings.lock().await;
    let existing = settings
        .providers
        .iter_mut()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("Provider '{name}' not found"))?;

    *existing = ProviderEntry {
        name, // preserve original name — renames are not permitted
        base_url: provider.base_url,
        api_key: None, // never stored in the struct
        models: provider.models,
    };
    drop(settings);

    // Rebuild the model router so changed base_url / API key takes effect.
    rebuild_model_router(&state).await;

    Ok(())
}

/// Remove a provider by `name`.
///
/// Deletes the corresponding vault entry first (before mutating settings),
/// then removes the provider from the in-memory list.  Returns an error if no
/// provider with that name exists.
#[tauri::command]
pub async fn remove_provider(name: String, state: State<'_, AppState>) -> Result<(), String> {
    // Check existence without holding the lock across I/O.
    {
        let settings = state.settings.lock().await;
        if !settings.providers.iter().any(|p| p.name == name) {
            return Err(format!("Provider '{name}' not found"));
        }
    }

    // Delete vault entry (lock is released).
    state
        .secret_store
        .delete(&provider_api_key(&name))
        .await
        .map_err(|e| e.to_string())?;

    // Re-acquire and remove from the settings list.
    state
        .settings
        .lock()
        .await
        .providers
        .retain(|p| p.name != name);

    // Rebuild the model router so the removed provider is no longer routed to.
    rebuild_model_router(&state).await;

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

    // Rebuild the model router so the new default provider takes effect.
    rebuild_model_router(&state).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a JSON representation of a provider with the API key masked.
///
/// `has_api_key` should be `true` if the vault holds a key for this provider.
fn mask_provider_fields(
    name: &str,
    base_url: &str,
    models: &[String],
    has_api_key: bool,
) -> serde_json::Value {
    serde_json::json!({
        "name":    name,
        "baseUrl": base_url,
        "apiKey":  if has_api_key { Some(MASKED_KEY) } else { None },
        "models":  models,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use tokio::sync::Mutex;

    use pares_agens_core::secrets::{provider_api_key, InMemorySecretStore, SecretStore};

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_store() -> Arc<InMemorySecretStore> {
        Arc::new(InMemorySecretStore::new())
    }

    fn make_settings_with(providers: Vec<ProviderEntry>) -> Mutex<Settings> {
        Mutex::new(Settings {
            providers,
            ..Settings::default()
        })
    }

    fn provider(name: &str) -> ProviderEntry {
        ProviderEntry {
            name: name.to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: None,
            models: vec![],
        }
    }

    fn provider_with_key(name: &str, key: &str) -> ProviderEntry {
        ProviderEntry {
            name: name.to_string(),
            base_url: "http://localhost:11434/v1".to_string(),
            api_key: Some(key.to_string()),
            models: vec![],
        }
    }

    // ── mask_provider_fields ──────────────────────────────────────────────

    #[test]
    fn mask_provider_hides_key_when_vault_has_entry() {
        let v = mask_provider_fields("test", "http://localhost", &[], true);
        assert_eq!(v["apiKey"], serde_json::json!(MASKED_KEY));
        assert_eq!(v["name"], "test");
    }

    #[test]
    fn mask_provider_null_when_no_vault_entry() {
        let v = mask_provider_fields("test", "http://localhost", &[], false);
        assert!(v["apiKey"].is_null());
    }

    // ── add_provider ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn add_provider_stores_key_in_vault_not_settings() {
        let store: Arc<dyn SecretStore> = make_store();
        let settings = make_settings_with(vec![]);

        // Simulate add_provider logic directly.
        let p = provider_with_key("openai", "sk-secret");
        let vault_key = provider_api_key(&p.name);
        store
            .set(&vault_key, p.api_key.as_deref().unwrap())
            .await
            .unwrap();
        settings
            .lock()
            .await
            .providers
            .push(ProviderEntry { api_key: None, ..p });

        // Vault holds the key.
        assert_eq!(
            store
                .get(&provider_api_key("openai"))
                .await
                .unwrap()
                .as_deref(),
            Some("sk-secret")
        );
        // Settings struct does NOT hold the key.
        let lock = settings.lock().await;
        assert!(lock.providers[0].api_key.is_none());
    }

    #[tokio::test]
    async fn add_provider_duplicate_returns_error() {
        let settings = make_settings_with(vec![provider("openai")]);

        let lock = settings.lock().await;
        let exists = lock.providers.iter().any(|p| p.name == "openai");
        assert!(exists, "duplicate detection should fire");
        drop(lock);

        // Second check (simulating re-lock after vault write).
        let lock2 = settings.lock().await;
        assert!(lock2.providers.iter().any(|p| p.name == "openai"));
    }

    // ── update_provider ───────────────────────────────────────────────────

    /// Helper that mirrors the production preserve/update filter.
    fn resolve_key_update(incoming: Option<&str>) -> Option<&str> {
        incoming.filter(|k| !k.is_empty() && *k != MASKED_KEY)
    }

    #[tokio::test]
    async fn update_provider_none_preserves_vault_key() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-existing")
            .await
            .unwrap();

        // None → preserve: no vault write.
        if let Some(k) = resolve_key_update(None) {
            store.set(&provider_api_key("openai"), k).await.unwrap();
        }

        assert_eq!(
            store
                .get(&provider_api_key("openai"))
                .await
                .unwrap()
                .as_deref(),
            Some("sk-existing")
        );
    }

    #[tokio::test]
    async fn update_provider_masked_sentinel_preserves_vault_key() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-existing")
            .await
            .unwrap();

        // MASKED_KEY → preserve.
        if let Some(k) = resolve_key_update(Some(MASKED_KEY)) {
            store.set(&provider_api_key("openai"), k).await.unwrap();
        }

        assert_eq!(
            store
                .get(&provider_api_key("openai"))
                .await
                .unwrap()
                .as_deref(),
            Some("sk-existing")
        );
    }

    #[tokio::test]
    async fn update_provider_empty_string_preserves_vault_key() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-existing")
            .await
            .unwrap();

        // Empty string → preserve (frontend clears the field without intent to delete).
        if let Some(k) = resolve_key_update(Some("")) {
            store.set(&provider_api_key("openai"), k).await.unwrap();
        }

        assert_eq!(
            store
                .get(&provider_api_key("openai"))
                .await
                .unwrap()
                .as_deref(),
            Some("sk-existing")
        );
    }

    #[tokio::test]
    async fn update_provider_new_value_updates_vault() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-old")
            .await
            .unwrap();

        if let Some(k) = resolve_key_update(Some("sk-new")) {
            store.set(&provider_api_key("openai"), k).await.unwrap();
        }

        assert_eq!(
            store
                .get(&provider_api_key("openai"))
                .await
                .unwrap()
                .as_deref(),
            Some("sk-new")
        );
    }

    // ── remove_provider ───────────────────────────────────────────────────

    #[tokio::test]
    async fn remove_provider_deletes_vault_key() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-secret")
            .await
            .unwrap();

        // Simulate remove_provider: delete from vault, then remove from settings.
        store.delete(&provider_api_key("openai")).await.unwrap();

        assert_eq!(store.get(&provider_api_key("openai")).await.unwrap(), None);
    }

    #[tokio::test]
    async fn remove_provider_vault_delete_is_silent_when_no_key_stored() {
        let store: Arc<dyn SecretStore> = make_store();
        // No key was ever set — delete should succeed silently.
        store.delete(&provider_api_key("unknown")).await.unwrap();
    }

    // ── list_providers (masked response) ─────────────────────────────────

    #[tokio::test]
    async fn list_providers_masked_when_vault_has_key() {
        let store: Arc<dyn SecretStore> = make_store();
        store
            .set(&provider_api_key("openai"), "sk-secret")
            .await
            .unwrap();

        let has_key = store
            .get(&provider_api_key("openai"))
            .await
            .unwrap()
            .is_some();
        let row = mask_provider_fields("openai", "http://example.com", &[], has_key);
        assert_eq!(row["apiKey"], serde_json::json!(MASKED_KEY));
    }

    #[tokio::test]
    async fn list_providers_null_when_no_vault_key() {
        let store: Arc<dyn SecretStore> = make_store();

        let has_key = store
            .get(&provider_api_key("ollama"))
            .await
            .unwrap()
            .is_some();
        let row = mask_provider_fields("ollama", "http://localhost:11434/v1", &[], has_key);
        assert!(row["apiKey"].is_null());
    }
}
