use tauri::State;

use pares_agens_channels::tauri_ipc::TauriIpcMessage;
use pares_agens_core::license::{
    FixedKeyValidator, LicenseStatus, LicenseValidator, PolarValidator,
};
use pares_agens_core::optimization::{EvidenceRequest, OptimizationSafety, OptimizationTelemetry};
use pares_agens_core::praxis::{AnalysisEvent, GuidanceCategory, GuidanceEntry, SourceSpan};
use pares_agens_core::telemetry::TelemetrySnapshot;

use crate::apply_activation_hotkey;
use crate::state::{rebuild_model_router, sanitize_activation_hotkey, AppState, Settings};

/// Send a user message through the core agent runtime and return the response.
///
/// The frontend calls this via `invoke("send_message", { content, requestId })`.
/// The adapter's run-loop processes the event and either:
/// - Emits `model-chunk` Tauri events (streaming path) and returns `""`, or
/// - Returns the full response string (non-streaming / tool-call path).
///
/// `request_id` must match the placeholder message ID the UI created so that
/// `model-chunk` / `model-error` events can be correlated on the frontend.
#[tauri::command]
pub async fn send_message(
    content: String,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let started_at = std::time::Instant::now();
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();

    state
        .ipc_handle
        .input_tx
        .send(TauriIpcMessage {
            content,
            request_id,
            response_tx,
        })
        .await
        .map_err(|e| format!("IPC send failed: {e}"))?;

    let response = response_rx
        .await
        .map_err(|e| format!("IPC receive failed: {e}"))?;

    let telemetry_enabled = {
        let settings = state.settings.lock().await;
        settings.telemetry.enabled
    };
    if telemetry_enabled {
        let elapsed_ms = started_at
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        state.telemetry_service.record_model_call(elapsed_ms).await;
    }

    match response {
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
pub async fn get_memories(state: State<'_, AppState>) -> Result<Vec<serde_json::Value>, String> {
    let entries = state.memory_store.all().await.map_err(|e| e.to_string())?;

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
/// Secrets that are never serialised to the frontend (`bot_token`,
/// `phone_number`) are re-merged from the current in-memory state so that
/// calling this command from the UI cannot accidentally clear a stored
/// credential.  API keys for model providers are stored in the vault (see
/// [`crate::settings::add_provider`]) and are never part of the Settings
/// struct.
#[tauri::command]
pub async fn set_settings(
    mut settings: Settings,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    settings.activation_hotkey = sanitize_activation_hotkey(&settings.activation_hotkey);

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
    #[cfg(desktop)]
    apply_activation_hotkey(&app, &settings.activation_hotkey)?;

    let mut current = state.settings.lock().await;
    // Re-attach secrets the frontend never received so they are not cleared.
    merge_secrets(&current, &mut settings);
    *current = settings;
    drop(current);

    // Rebuild the model router so changed model / system prompt / providers
    // take effect on the next message without a restart.
    rebuild_model_router(&state).await;

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
pub async fn trigger_praxis_analysis(state: State<'_, AppState>) -> Result<u32, String> {
    let memories = state.memory_store.all().await.map_err(|e| e.to_string())?;

    let mut analysis_count = 0;
    for memory in memories.iter().take(10) {
        // Limit to 10 recent memories
        state
            .guidance_service
            .generate_guidance_from_memory(&memory.content, &memory.id);
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
    Ok(state
        .optimization_safety_gate
        .check_optimization_safety(&action))
}

/// Get all pending evidence requests.
///
/// Returns evidence requests that were generated when actions were blocked
/// due to insufficient data.
#[tauri::command]
pub async fn get_pending_evidence_requests(
    state: State<'_, AppState>,
) -> Result<Vec<EvidenceRequest>, String> {
    Ok(state
        .optimization_safety_gate
        .get_pending_evidence_requests())
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
    state
        .optimization_safety_gate
        .update_telemetry_outcome(&telemetry_id, outcome)
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
    state
        .optimization_safety_gate
        .execute_with_safety_check(&action, || {
            Ok::<String, String>(format!("Executed: {}", action))
        })
        .await
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Re-merge secrets from `existing` into `incoming`.
///
/// Fields marked `#[serde(skip_serializing)]` are never sent to the
/// frontend, so `set_settings` would otherwise clear them on every save.
/// This helper copies:
/// - `ChannelAdapterConfig.bot_token` / `phone_number` — matched by kind
///
/// Note: `ProviderEntry.api_key` is no longer merged here because API keys
/// are stored exclusively in the [`crate::state::AppState::secret_store`]
/// vault — they are never held in the in-memory `Settings` struct.
fn merge_secrets(existing: &Settings, incoming: &mut Settings) {
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

// ---------------------------------------------------------------------------
// MCP Tool Commands
// ---------------------------------------------------------------------------

/// List all discovered MCP tools across connected servers.
#[tauri::command]
pub async fn list_mcp_tools(
    state: State<'_, AppState>,
) -> Result<Vec<crate::mcp::DiscoveredTool>, String> {
    Ok(crate::mcp::list_discovered_tools(&state).await)
}

/// Call an MCP tool by name with JSON arguments.
#[tauri::command]
pub async fn call_mcp_tool(
    tool_name: String,
    arguments: Option<serde_json::Value>,
    state: State<'_, AppState>,
) -> Result<crate::mcp::ToolCallResult, String> {
    Ok(crate::mcp::call_tool(&state, &tool_name, arguments).await)
}

/// Restart all MCP servers (re-reads settings, respawns enabled servers).
#[tauri::command]
pub async fn restart_mcp_servers(state: State<'_, AppState>) -> Result<(), String> {
    crate::mcp::restart_mcp_servers(&state).await;
    Ok(())
}

/// Get MCP tools in OpenAI function-calling format.
#[tauri::command]
pub async fn get_mcp_openai_tools(
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    Ok(crate::mcp::openai_tools(&state).await)
}

// ---------------------------------------------------------------------------
// License Commands
// ---------------------------------------------------------------------------

/// Return a serialisable snapshot of the current license status.
///
/// The frontend calls this via `invoke("get_license_status")` to show the
/// current tier (Free / Pro) and whether the license is still valid.
#[tauri::command]
pub async fn get_license_status(state: State<'_, AppState>) -> Result<LicenseStatus, String> {
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

/// Return a snapshot of local anonymous telemetry aggregates.
#[tauri::command]
pub async fn get_telemetry_snapshot(
    state: State<'_, AppState>,
) -> Result<TelemetrySnapshot, String> {
    Ok(state.telemetry_service.snapshot().await)
}

/// Upload the local anonymous telemetry snapshot to the configured endpoint.
#[tauri::command]
pub async fn upload_telemetry_snapshot(state: State<'_, AppState>) -> Result<(), String> {
    let (telemetry_enabled, upload_enabled, upload_endpoint) = {
        let settings = state.settings.lock().await;
        (
            settings.telemetry.enabled,
            settings.telemetry.upload_enabled,
            settings.telemetry.upload_endpoint.clone(),
        )
    };

    if !telemetry_enabled {
        return Err("Telemetry collection is disabled".to_string());
    }
    if !upload_enabled {
        return Err("Telemetry upload is disabled".to_string());
    }

    let endpoint = upload_endpoint
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .ok_or_else(|| "Telemetry upload endpoint is not configured".to_string())?;

    let payload = state.telemetry_service.snapshot().await;
    let response = reqwest::Client::new()
        .post(&endpoint)
        .json(&payload)
        .send()
        .await
        .map_err(|e| format!("Telemetry upload failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Telemetry upload returned status {}",
            response.status()
        ));
    }

    state.telemetry_service.mark_uploaded().await;
    Ok(())
}

/// Return recent conversation turns for the chat UI.
///
/// Reads persisted `ChatTurn` entries from PluresDB so the chat view can
/// hydrate conversation history on load. Returns up to `limit` most recent
/// turns (default 20), each containing the full message array.
#[tauri::command]
pub async fn get_conversation_history(
    channel: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let channel = channel.unwrap_or_else(|| "desktop".to_string());
    let limit = limit.unwrap_or(20);

    let turns = state
        .memory_store
        .recent_turns(&channel, limit)
        .await
        .map_err(|e| e.to_string())?;

    let result: Vec<serde_json::Value> = turns
        .into_iter()
        .flat_map(|t| {
            let ts = t.timestamp.clone();
            t.messages.into_iter().filter_map(move |m| {
                // Skip system messages and tool results from the UI view.
                if m.role == "system" || m.role == "tool" {
                    return None;
                }
                Some(serde_json::json!({
                    "role": if m.role == "assistant" { "agent" } else { &m.role },
                    "content": m.content,
                    "time": ts,
                }))
            })
        })
        .collect();

    Ok(result)
}

/// Handle a user action selected from an actionable desktop notification.
#[tauri::command]
pub async fn handle_notification_action(
    notification_id: String,
    action: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    crate::notifications::handle_action(&app, &notification_id, &action);
    Ok(())
}
