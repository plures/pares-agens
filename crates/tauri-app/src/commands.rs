use tauri::State;

use pares_agens_channels::tauri_ipc::TauriIpcMessage;
use pares_agens_core::memory::store::MemoryStore;

use crate::state::{AppState, Settings};

/// Send a user message through the core agent runtime and return the response.
///
/// The frontend calls this via `invoke("send_message", { content })`.
/// The adapter's run-loop processes the event and returns a `ModelResponse`.
#[tauri::command]
pub async fn send_message(
    content: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    state
        .ipc_handle
        .input_tx
        .send(TauriIpcMessage { content, response_tx })
        .await
        .map_err(|e| format!("IPC send failed: {e}"))?;

    match response_rx
        .await
        .map_err(|e| format!("IPC receive failed: {e}"))?
    {
        Some(pares_agens_core::Event::ModelResponse { content, .. }) => Ok(content),
        Some(pares_agens_core::Event::Message { content, .. }) => Ok(content),
        _ => Ok(String::new()),
    }
}

/// Return up to 20 recent memories for the memory sidebar.
///
/// Memories are returned newest-first as plain JSON objects so the frontend
/// can render them without depending on the internal `MemoryEntry` type.
#[tauri::command]
pub async fn get_memories(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let entries = state
        .memory_store
        .all()
        .await
        .map_err(|e| e.to_string())?;

    let recent = entries
        .into_iter()
        .rev()
        .take(20)
        .map(|e| {
            serde_json::json!({
                "id":         e.id,
                "content":    e.content,
                "category":   e.category.as_str(),
                "created_at": e.created_at,
            })
        })
        .collect();

    Ok(recent)
}

/// Return the current application settings.
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    Ok(state.settings.lock().await.clone())
}

/// Persist updated application settings.
///
/// When `settings.auto_start` changes this command also enables or disables
/// the OS-level autostart entry via `tauri-plugin-autostart`.
///
/// Secrets that are never serialised to the frontend (`api_key`,
/// `bot_token`, `phone_number`) are re-merged from the current in-memory
/// state so that calling this command from the UI cannot accidentally clear
/// a stored credential.
#[tauri::command]
pub async fn set_settings(
    mut settings: Settings,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    #[cfg(desktop)]
    {
        use tauri_plugin_autostart::ManagerExt;
        let manager = app.autolaunch();
        if settings.auto_start {
            manager.enable().map_err(|e| e.to_string())?;
        } else {
            manager.disable().map_err(|e| e.to_string())?;
        }
    }
    #[cfg(not(desktop))]
    let _ = app;

    let mut current = state.settings.lock().await;
    // Re-attach secrets the frontend never received so they are not cleared.
    merge_secrets(&current, &mut settings);
    *current = settings;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Re-merge secrets from `existing` into `incoming`.
///
/// Fields marked `#[serde(skip_serializing)]` are never sent to the
/// frontend, so `set_settings` would otherwise clear them on every save.
/// This helper copies:
/// - `ProviderEntry.api_key`      — matched by provider name
/// - `ChannelAdapterConfig.bot_token` / `phone_number` — matched by kind
fn merge_secrets(existing: &Settings, incoming: &mut Settings) {
    for provider in &mut incoming.providers {
        if provider.api_key.is_none() {
            if let Some(ex) = existing.providers.iter().find(|p| p.name == provider.name) {
                provider.api_key = ex.api_key.clone();
            }
        }
    }
    for adapter in &mut incoming.channel_adapters {
        if let Some(ex) = existing
            .channel_adapters
            .iter()
            .find(|a| a.kind == adapter.kind)
        {
            if adapter.bot_token.is_none() {
                adapter.bot_token = ex.bot_token.clone();
            }
            if adapter.phone_number.is_none() {
                adapter.phone_number = ex.phone_number.clone();
            }
        }
    }
}
