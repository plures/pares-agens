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
#[tauri::command]
pub async fn set_settings(
    settings: Settings,
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
    *state.settings.lock().await = settings;
    Ok(())
}
