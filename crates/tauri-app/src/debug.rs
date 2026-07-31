//! Tauri boundary for the Chronos live-context debug render surface.

use tauri::State;

use crate::state::AppState;

/// Pause or resume the selected live-context session while its Chronos cards
/// are inspected. The operation is session-scoped and performs no routing or
/// policy selection; that behavior is defined by `live-context-debug.px`.
#[tauri::command]
pub async fn set_live_context_paused(
    session_id: String,
    paused: bool,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    if session_id.trim().is_empty() {
        return Err("session_id is required".to_string());
    }

    let result = if paused {
        state.live_context_handler.pause_live_context(&session_id).await
    } else {
        state.live_context_handler.resume_live_context(&session_id).await
    };
    result.map_err(|error| error.to_string())
}
