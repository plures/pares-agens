use tauri::State;

use pares_agens_channels::tauri_ipc::TauriIpcMessage;
use pares_agens_core::memory::store::MemoryStore;
use pares_agens_core::optimization::{EvidenceRequest, OptimizationSafety, OptimizationTelemetry};
use pares_agens_core::praxis::{GuidanceCategory, GuidanceEntry, SourceSpan, AnalysisEvent};

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

/// Get Praxis coprocessor guidance entries for a specific category.
///
/// Returns guidance entries sorted by priority and confidence.
/// The frontend can use this to populate the Facts, Rules, Decisions,
/// Risks, and Guidance sections in the memory sidebar.
#[tauri::command]
pub async fn get_praxis_guidance(
    category: String,
    state: State<'_, AppState>,
) -> Result<Vec<GuidanceEntry>, String> {
    let category = match category.as_str() {
        "facts" => GuidanceCategory::Facts,
        "rules" => GuidanceCategory::Rules,
        "constraints" => GuidanceCategory::Constraints,
        "decisions" => GuidanceCategory::Decisions,
        "risks" => GuidanceCategory::Risks,
        "guidance" => GuidanceCategory::Guidance,
        _ => return Err(format!("Unknown guidance category: {}", category)),
    };

    Ok(state.guidance_service.get_guidance(&category))
}

/// Get all Praxis guidance entries across all categories.
///
/// Returns all guidance entries for overview/search functionality.
#[tauri::command]
pub async fn get_all_praxis_guidance(
    state: State<'_, AppState>,
) -> Result<Vec<GuidanceEntry>, String> {
    Ok(state.guidance_service.get_all_guidance())
}

/// Get source spans for traceability from guidance to memory.
///
/// Takes a list of span IDs and returns the corresponding source spans
/// with memory references, positions, and relevance scores.
#[tauri::command]
pub async fn get_source_spans(
    span_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<SourceSpan>, String> {
    Ok(state.guidance_service.get_spans(&span_ids))
}

/// Get recent Praxis analysis events.
///
/// Returns recent analysis events that triggered guidance updates.
/// Used for showing live analysis activity in the sidebar.
#[tauri::command]
pub async fn get_analysis_events(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<AnalysisEvent>, String> {
    let limit = limit.unwrap_or(10);
    Ok(state.guidance_service.get_recent_events(limit))
}

/// Trigger manual analysis of current memories.
///
/// Forces the Praxis coprocessor to re-analyze existing memories
/// and update guidance entries. Useful for testing or when the user
/// wants to refresh guidance after significant memory updates.
#[tauri::command]
pub async fn trigger_praxis_analysis(
    state: State<'_, AppState>,
) -> Result<u32, String> {
    let memories = state
        .memory_store
        .all()
        .await
        .map_err(|e| e.to_string())?;

    let mut analysis_count = 0;
    for memory in memories.iter().take(10) { // Limit to 10 recent memories
        state.guidance_service.generate_guidance_from_memory(&memory.content, &memory.id);
        analysis_count += 1;
    }

    Ok(analysis_count)
}

/// Check optimization safety for a specific action.
///
/// Returns the safety assessment from the control plane.
#[tauri::command]
pub async fn check_optimization_safety(
    action: String,
    state: State<'_, AppState>,
) -> Result<OptimizationSafety, String> {
    Ok(state.optimization_safety_gate.check_optimization_safety(&action))
}

/// Get all pending evidence requests.
///
/// Returns evidence requests that were generated when actions were blocked
/// due to insufficient data.
#[tauri::command]
pub async fn get_pending_evidence_requests(
    state: State<'_, AppState>,
) -> Result<Vec<EvidenceRequest>, String> {
    Ok(state.optimization_safety_gate.get_pending_evidence_requests())
}

/// Get optimization telemetry records.
///
/// Returns telemetry data for blocked optimization executions with optional limit.
#[tauri::command]
pub async fn get_optimization_telemetry(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<OptimizationTelemetry>, String> {
    Ok(state.optimization_safety_gate.get_telemetry(limit))
}

/// Update the eventual outcome for a blocked optimization action.
///
/// Records the final result of what happened after an optimization was initially blocked.
#[tauri::command]
pub async fn update_optimization_outcome(
    telemetry_id: String,
    outcome: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.optimization_safety_gate.update_telemetry_outcome(&telemetry_id, outcome)
}

/// Execute an action with optimization safety enforcement.
///
/// This is a test/demonstration command that shows how safety gates work.
/// In production, safety enforcement happens automatically in the executor.
#[tauri::command]
pub async fn execute_with_safety(
    action: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state.optimization_safety_gate.execute_with_safety_check(
        &action,
        || Ok::<String, String>(format!("Executed: {}", action))
    ).await
}

// ── helpers ──────────────────────────────────────────────────────────────────

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
