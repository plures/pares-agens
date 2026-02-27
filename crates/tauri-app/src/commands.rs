use tauri::State;

use pares_agens_channels::tauri_ipc::TauriIpcMessage;
use pares_agens_core::license::{
    FixedKeyValidator, LicenseStatus, LicenseValidator, PolarValidator,
};
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
#[tauri::command]
pub async fn set_settings(
    settings: Settings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    *state.settings.lock().await = settings;
    Ok(())
}

/// Return a serialisable snapshot of the current license status.
///
/// The frontend calls this via `invoke("get_license_status")` to show the
/// current tier (Free / Pro) and whether the license is still valid.
#[tauri::command]
pub async fn get_license_status(
    state: State<'_, AppState>,
) -> Result<LicenseStatus, String> {
    Ok(state.license.lock().await.status())
}

/// Activate a Pro license key.
///
/// * If the `POLAR_BENEFIT_ID` environment variable is set, validates the key
///   against the Polar.sh API (online) and writes the resulting Pro license to
///   the shared state.
/// * Otherwise, falls back to `FixedKeyValidator` using the `PRO_LICENSE_KEY`
///   environment variable.  This is suitable for self-hosted / offline setups.
///
/// Returns the updated [`LicenseStatus`] on success so the UI can refresh
/// immediately.
#[tauri::command]
pub async fn activate_license(
    key: String,
    state: State<'_, AppState>,
) -> Result<LicenseStatus, String> {
    let new_license = if let Ok(benefit_id) = std::env::var("POLAR_BENEFIT_ID") {
        let validator = PolarValidator::new(benefit_id);
        validator.validate(&key).await.map_err(|e| e.to_string())?
    } else {
        let expected = std::env::var("PRO_LICENSE_KEY").unwrap_or_default();
        let validator = FixedKeyValidator::new(expected);
        validator.validate(&key).await.map_err(|e| e.to_string())?
    };

    let status = new_license.status();
    *state.license.lock().await = new_license;
    Ok(status)
}
