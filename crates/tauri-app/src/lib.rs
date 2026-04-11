use std::sync::Arc;

use futures_util::StreamExt;
use tauri::{Emitter, Manager};
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
use pares_agens_core::agent::{Agent, InMemory};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::memory::embed::MockEmbedder;
use pares_agens_core::memory::store::PluresDbStore;
use pares_agens_core::memory::store::{InMemoryStore, MemoryStore};
use pares_agens_core::memory::PluresLm;
use pares_agens_core::optimization::OptimizationSafetyGate;
use pares_agens_core::praxis::GuidanceService;
use pares_agens_core::secrets::InMemorySecretStore;
use pares_agens_core::Event;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role};
use pares_models::ModelRouter;

use crate::state::{build_router_config, rebuild_model_router, AppState, Settings};

mod commands;
mod mcp;
mod migration;
mod procedures;
mod settings;
mod state;
pub mod tray;
mod wizard;

/// Entry point called from `main.rs`.
///
/// Wires up:
/// - Tauri IPC adapter ↔ core agent event loop (background task)
/// - System tray with Show/Hide, Settings, and Quit menu items
/// - Window-state persistence (size and position restored on next launch)
/// - Auto-start at system login when [`Settings::auto_start`] is enabled
/// - Shared [`AppState`] exposed to every Tauri command
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            // ── Memory store ──────────────────────────────────────────────
            // Open a persistent PluresDB-backed memory store under the app data
            // directory.  Fall back to an ephemeral in-memory store if the data
            // directory is unavailable (e.g. in sandboxed CI environments).
            //
            // The resulting `Arc<dyn MemoryStore>` is shared between `AppState`
            // and the `PluresLm` inside the agent so that autorecall sees all
            // captured memories.
            let memory_store: Arc<dyn MemoryStore> = match app
                .path()
                .app_data_dir()
                .ok()
                .and_then(|dir| {
                    PluresDbStore::open(dir.join("memory.db"))
                        .map_err(|e| {
                            tracing::warn!(
                                "PluresDbStore::open failed ({}), falling back to in-memory",
                                e
                            );
                            e
                        })
                        .ok()
                }) {
                Some(store) => Arc::new(store),
                None => Arc::new(InMemoryStore::new()),
            };

            // ── Shared settings & model router ────────────────────────────
            let default_settings = Settings::default();
            let router_config = build_router_config(&default_settings);
            let settings: Arc<Mutex<Settings>> = Arc::new(Mutex::new(default_settings));
            let model_router: Arc<RwLock<ModelRouter>> =
                Arc::new(RwLock::new(ModelRouter::new(router_config)));

            // Clones captured by the adapter callback.
            let settings_for_cb = Arc::clone(&settings);
            let router_for_cb = Arc::clone(&model_router);

            // MCP state shared between AppState and the adapter callback.
            let mcp_clients: Arc<Mutex<std::collections::HashMap<String, mcp_client::McpClient>>> =
                Arc::new(Mutex::new(std::collections::HashMap::new()));
            let mcp_tools: Arc<RwLock<Vec<(String, mcp_client::protocol::Tool)>>> =
                Arc::new(RwLock::new(Vec::new()));
            let mcp_tools_for_cb = Arc::clone(&mcp_tools);
            let mcp_clients_for_cb = Arc::clone(&mcp_clients);

            // ── IPC bridge ────────────────────────────────────────────────
            let (adapter, handle) = tauri_ipc_channel("user");

            // Build the PluresLm instance that shares the backing store with
            // AppState so that autorecall sees all captured memories.
            let plures_lm = Arc::new(PluresLm::new(
                Arc::clone(&memory_store),
                Box::new(MockEmbedder),
                128_000,
            ));

            // Build the Agent with a Cerebellum wired in so every message
            // flows through autorecall and routing before being handled.
            let agent = Arc::new(Agent::with_cerebellum(
                Arc::new(InMemory::new()),
                Cerebellum::new(CerebellumConfig::default()),
                plures_lm,
            ));

            // Spawn the adapter run-loop, routing all events through the agent
            // and then through the model router for real LLM responses.
            let app_handle_for_cb = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                info!("Tauri IPC adapter starting (cerebellum + model router enabled)");
                adapter
                    .run(move |event: Event| {
                        let agent = Arc::clone(&agent);
                        let router = Arc::clone(&router_for_cb);
                        let settings = Arc::clone(&settings_for_cb);
                        let mcp_tools = Arc::clone(&mcp_tools_for_cb);
                        let mcp_clients = Arc::clone(&mcp_clients_for_cb);
                        let app_handle = app_handle_for_cb.clone();
                        Box::pin(async move {
                            // Extract message fields before consuming the event — avoids
                            // cloning the entire Event payload.
                            let msg_fields = if let Event::Message { ref id, ref content, .. } = event {
                                Some((id.clone(), content.clone()))
                            } else {
                                None
                            };

                            // Let the agent run cerebellum preprocessing (autorecall,
                            // routing, drop filtering) and capture the message in memory.
                            let preprocessed = agent.handle_event(event).await;

                            // If the cerebellum dropped the event, respect that.
                            preprocessed.as_ref()?;

                            // For message events, replace the echo response with a real
                            // model call via the ModelRouter.
                            if let Some((id, content)) = msg_fields {
                                // Prefer routing.interactive.model when configured
                                // (set by the Settings routing tab); fall back to the
                                // legacy settings.model field.
                                let (model, system_prompt) = {
                                    let s = settings.lock().await;
                                    let model = s.routing.interactive
                                        .as_ref()
                                        .map(|r| r.model.clone())
                                        .unwrap_or_else(|| s.model.clone());
                                    (model, s.system_prompt.clone())
                                };

                                let mut messages = vec![
                                    ChatMessage::text(Role::System, &system_prompt),
                                    ChatMessage::text(Role::User, &content),
                                ];

                                // Inject MCP tools into the request if any are available.
                                let tools_json: Option<Vec<pares_models::types::Tool>> = {
                                    let tool_list = mcp_tools.read().await;
                                    if tool_list.is_empty() {
                                        None
                                    } else {
                                        let converted: Vec<pares_models::types::Tool> = tool_list
                                            .iter()
                                            .map(|(_, t)| pares_models::types::Tool {
                                                kind: "function".to_string(),
                                                function: pares_models::types::FunctionDefinition {
                                                    name: t.name.clone(),
                                                    description: t.description.clone(),
                                                    parameters: Some(serde_json::to_value(&t.input_schema)
                                                        .unwrap_or_default()),
                                                },
                                            })
                                            .collect();
                                        Some(converted)
                                    }
                                };

                                // ── Streaming path (no MCP tools) ─────────────────────────────
                                //
                                // When no tools are configured, stream tokens directly to the UI
                                // via `model-chunk` Tauri events.  The `request_id` in each event
                                // matches the placeholder message ID created by the frontend.
                                if tools_json.is_none() {
                                    let request = ChatCompletionRequest::new(&model, messages);
                                    let router_guard = router.read().await;
                                    match router_guard.chat_stream(&request).await {
                                        Ok(stream) => {
                                            drop(router_guard);
                                            let mut stream = std::pin::pin!(stream);
                                            while let Some(chunk) = stream.next().await {
                                                match chunk {
                                                    Ok(c) => {
                                                        if let Some(choice) = c.choices.first() {
                                                            if let Some(ref delta) = choice.delta.content {
                                                                if !delta.is_empty() {
                                                                    // Token chunks: silently drop on emit failure —
                                                                    // individual tokens are non-critical and logging
                                                                    // each would be noisy on a transient disconnect.
                                                                    let _ = app_handle.emit(
                                                                        "model-chunk",
                                                                        serde_json::json!({
                                                                            "request_id": &id,
                                                                            "content": delta,
                                                                            "done": false,
                                                                        }),
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                    Err(e) => {
                                                        error!(error = %e, model = %model, "streaming chunk error");
                                                        if let Err(emit_err) = app_handle.emit(
                                                            "model-error",
                                                            serde_json::json!({
                                                                "request_id": &id,
                                                                "error": format_model_error(&e.to_string(), &model),
                                                            }),
                                                        ) {
                                                            error!(error = %emit_err, "failed to emit model-error event");
                                                        }
                                                        return None;
                                                    }
                                                }
                                            }
                                            // Signal the frontend that streaming is complete.
                                            // Log if this critical done signal fails — a stuck
                                            // placeholder will result if it is lost.
                                            if let Err(emit_err) = app_handle.emit(
                                                "model-chunk",
                                                serde_json::json!({
                                                    "request_id": &id,
                                                    "content": "",
                                                    "done": true,
                                                }),
                                            ) {
                                                error!(error = %emit_err, "failed to emit final model-chunk done event");
                                            }
                                            // Return None — the streaming events carry the content.
                                            return None;
                                        }
                                        Err(e) => {
                                            drop(router_guard);
                                            error!(error = %e, model = %model, "model router stream failed");
                                            if let Err(emit_err) = app_handle.emit(
                                                "model-error",
                                                serde_json::json!({
                                                    "request_id": &id,
                                                    "error": format_model_error(&e.to_string(), &model),
                                                }),
                                            ) {
                                                error!(error = %emit_err, "failed to emit model-error event");
                                            }
                                            return None;
                                        }
                                    }
                                }

                                // ── Non-streaming path (MCP tools present) ─────────────────────
                                //
                                // When MCP tools are configured, use the tool-call loop with
                                // non-streaming requests so we can inspect tool_calls in each
                                // response before deciding whether to continue or return.
                                let mut request = ChatCompletionRequest::new(&model, messages.clone());
                                request.tools = tools_json;

                                // Tool call loop: model may return tool_calls instead of content.
                                // Execute tools, feed results back, repeat until we get content.
                                let max_tool_rounds = 5;
                                let mut final_reply = String::new();

                                for _round in 0..max_tool_rounds {
                                    let router_guard = router.read().await;
                                    match router_guard.chat(&request).await {
                                        Ok(response) => {
                                            drop(router_guard);
                                            let choice = match response.choices.first() {
                                                Some(c) => c,
                                                None => break,
                                            };

                                            // Check for tool calls
                                            if let Some(ref tool_calls) = choice.message.tool_calls {
                                                if !tool_calls.is_empty() {
                                                    // Add assistant message with tool calls to conversation
                                                    messages.push(ChatMessage {
                                                        role: Role::Assistant,
                                                        content: choice.message.content.clone(),
                                                        tool_calls: Some(tool_calls.clone()),
                                                        tool_call_id: None,
                                                        name: None,
                                                    });

                                                    // Execute each tool call
                                                    for tc in tool_calls {
                                                        let args: Option<serde_json::Value> =
                                                            serde_json::from_str(&tc.function.arguments).ok();

                                                        let result = {
                                                            let mut clients = mcp_clients.lock().await;
                                                            // Find which server owns this tool
                                                            let server_name = {
                                                                let tl = mcp_tools.read().await;
                                                                tl.iter()
                                                                    .find(|(_, t)| t.name == tc.function.name)
                                                                    .map(|(n, _)| n.clone())
                                                            };
                                                            if let Some(ref sn) = server_name {
                                                                if let Some(client) = clients.get_mut(sn) {
                                                                    match client.call_tool(&tc.function.name, args).await {
                                                                        Ok(r) => {
                                                                            let text = r.content.iter().filter_map(|c| {
                                                                                match c {
                                                                                    mcp_client::protocol::ToolContent::Text { text } => Some(text.clone()),
                                                                                    _ => None,
                                                                                }
                                                                            }).collect::<Vec<_>>().join("\n");
                                                                            text
                                                                        }
                                                                        Err(e) => format!("Error: {e}"),
                                                                    }
                                                                } else {
                                                                    format!("MCP server '{}' not connected", sn)
                                                                }
                                                            } else {
                                                                format!("No MCP server provides tool '{}'", tc.function.name)
                                                            }
                                                        };

                                                        // Add tool result to conversation
                                                        messages.push(ChatMessage {
                                                            role: Role::Tool,
                                                            content: Some(result),
                                                            tool_calls: None,
                                                            tool_call_id: Some(tc.id.clone()),
                                                            name: None,
                                                        });
                                                    }

                                                    // Update request for next round
                                                    request = ChatCompletionRequest::new(&model, messages.clone());
                                                    request.tools = {
                                                        let tl = mcp_tools.read().await;
                                                        if tl.is_empty() {
                                                            None
                                                        } else {
                                                            Some(tl.iter().map(|(_, t)| pares_models::types::Tool {
                                                                kind: "function".to_string(),
                                                                function: pares_models::types::FunctionDefinition {
                                                                    name: t.name.clone(),
                                                                    description: t.description.clone(),
                                                                    parameters: Some(serde_json::to_value(&t.input_schema)
                                                                        .unwrap_or_default()),
                                                                },
                                                            }).collect())
                                                        }
                                                    };
                                                    continue; // Next round
                                                }
                                            }

                                            // No tool calls — we have the final response
                                            final_reply = choice.message.content
                                                .as_ref()
                                                .cloned()
                                                .unwrap_or_default();
                                            break;
                                        }
                                        Err(e) => {
                                            drop(router_guard);
                                            error!(error = %e, model = %model, "model router call failed");
                                            if let Err(emit_err) = app_handle.emit(
                                                "model-error",
                                                serde_json::json!({
                                                    "request_id": &id,
                                                    "error": format_model_error(&e.to_string(), &model),
                                                }),
                                            ) {
                                                error!(error = %emit_err, "failed to emit model-error event");
                                            }
                                            return None;
                                        }
                                    }
                                }

                                Some(Event::ModelResponse {
                                    request_id: id,
                                    model,
                                    content: final_reply,
                                })
                            } else {
                                preprocessed
                            }
                        })
                    })
                    .await
                    .ok();
            });

            // ── AppState ──────────────────────────────────────────────────
            let guidance_service = GuidanceService::new();
            let optimization_safety_gate = OptimizationSafetyGate::new();
            // Initialise the secret store.  In production (with the `vault`
            // feature enabled) this would open the plures-vault encrypted
            // database from the app-data directory.  The in-memory store is
            // used for the default build so that no external dependencies or
            // vault unlocking are required on startup.
            let secret_store = Arc::new(InMemorySecretStore::new());
            app.manage(AppState {
                ipc_handle: handle,
                memory_store,
                secret_store,
                settings,
                model_router,
                wizard_completed: Mutex::new(false),
                procedures: Mutex::new(Vec::new()),
                procedure_log: Mutex::new(Vec::new()),
                guidance_service,
                optimization_safety_gate,
                mcp_clients: Arc::clone(&mcp_clients),
                mcp_tools: Arc::clone(&mcp_tools),
                license: Mutex::new(pares_agens_core::license::License::free()),
            });

            // ── Initial router rebuild ─────────────────────────────────────
            // The router was created from Settings::default() above.  Rebuild
            // it now that AppState (including the vault-backed SecretStore) is
            // managed so the initial router includes any persisted API keys.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();
                rebuild_model_router(&state).await;
                mcp::start_mcp_servers(&state).await;
            });

            // ── System tray ───────────────────────────────────────────────
            tray::setup_tray(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_message,
            commands::get_memories,
            commands::get_settings,
            commands::set_settings,
            commands::get_praxis_guidance,
            commands::get_all_praxis_guidance,
            commands::get_source_spans,
            commands::get_analysis_events,
            commands::trigger_praxis_analysis,
            commands::check_optimization_safety,
            commands::get_pending_evidence_requests,
            commands::get_optimization_telemetry,
            commands::update_optimization_outcome,
            commands::execute_with_safety,
            wizard::detect_docker_runner,
            wizard::validate_api_key,
            wizard::is_wizard_completed,
            wizard::complete_wizard,
            settings::list_providers,
            settings::add_provider,
            settings::update_provider,
            settings::remove_provider,
            settings::upsert_channel_adapter,
            settings::set_routing,
            migration::migration_detect,
            migration::migration_preview,
            migration::migration_run,
            procedures::list_procedures,
            procedures::get_procedure,
            procedures::save_procedure,
            procedures::toggle_procedure,
            procedures::get_procedure_log,
            procedures::create_from_template,
            commands::list_mcp_tools,
            commands::call_mcp_tool,
            commands::restart_mcp_servers,
            commands::get_mcp_openai_tools,
            commands::get_license_status,
            commands::activate_license,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pares Agens");
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Produce a user-friendly error message for model/stream failures.
///
/// Detects common connectivity and availability errors and provides
/// actionable guidance.  Includes Ollama-specific hints when relevant
/// (the default local provider), but remains useful for any endpoint.
fn format_model_error(raw: &str, model: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("connection refused")
        || lower.contains("failed to connect")
        || lower.contains("error sending request")
        || lower.contains("tcp connect")
        || lower.contains("no route to host")
        || lower.contains("os error 111")
    {
        format!(
            "Cannot reach the model endpoint. Is the model server running?\n\n\
             If you are using Ollama locally:\n\
             • ollama serve\n\
             • ollama pull {model}\n\n\
             Otherwise, verify the endpoint URL in Settings and try again."
        )
    } else if lower.contains("model") && (lower.contains("not found") || lower.contains("404")) {
        format!(
            "Model `{model}` was not found on the configured endpoint.\n\n\
             If you are using Ollama, pull it with: ollama pull {model}\n\
             Otherwise, check the model name in Settings."
        )
    } else {
        format!("Model request to `{model}` failed: {raw}")
    }
}
