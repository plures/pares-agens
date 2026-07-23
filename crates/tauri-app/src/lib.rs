use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, Manager};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, instrument};

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
use pares_agens_core::agent::{Agent, InMemory};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::memory::embed::MockEmbedder;
use pares_agens_core::memory::store::PluresDbStore;
use pares_agens_core::memory::store::{InMemoryStore, MemoryStore};
use pares_agens_core::memory::PluresLm;
use pares_radix_core::model::{
    ChatMessage, ChatOptions, ModelClient, ModelCompletion, StreamDelta, StreamSender,
    ToolDefinition, ToolDispatcher,
};
use pares_radix_core::optimization::OptimizationSafetyGate;
use pares_radix_core::plugins::PluginRuntime;
use pares_radix_core::praxis::GuidanceService;
use pares_radix_core::secrets::{provider_api_key, InMemorySecretStore, SecretStore};
use pares_radix_core::Event;
use pares_radix_core::{PluresDbStateStore, StateStore};
use pares_agens_models::types::{ChatCompletionRequest, Role, Tool};
use pares_agens_models::ModelRouter;

use crate::state::{
    build_router_config, rebuild_model_router, sanitize_activation_hotkey, AppState, Settings,
};
use crate::telemetry::TelemetryService;

mod commands;
mod mcp;
mod migration;
mod notifications;
mod plugins;
mod procedures;
mod settings;
mod state;
mod telemetry;
pub mod tray;
mod wizard;

#[derive(Serialize)]
struct ModelChunkPayload {
    request_id: String,
    content: String,
    done: bool,
}

#[derive(Serialize)]
struct ModelResponsePayload {
    request_id: String,
    content: String,
}

#[derive(Serialize)]
struct ModelErrorPayload {
    request_id: String,
    error: String,
}

#[cfg(test)]
fn split_stream_chunks(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    content
        .split_inclusive(char::is_whitespace)
        .map(str::to_string)
        .collect()
}

fn emit_with_warn<T: Serialize>(app_handle: &tauri::AppHandle, event: &str, payload: &T) {
    if let Err(err) = app_handle.emit(event, payload) {
        tracing::warn!(event, error = %err, "failed to emit tauri event");
    }
}

#[cfg(desktop)]
fn setup_global_shortcut(app: &tauri::AppHandle, activation_hotkey: &str) -> tauri::Result<()> {
    app.plugin(tauri_plugin_global_shortcut::Builder::new().build())?;
    if let Err(err) = apply_activation_hotkey(app, activation_hotkey) {
        tracing::warn!(hotkey = activation_hotkey, error = %err, "failed to register activation hotkey");
    }

    Ok(())
}

#[cfg(not(desktop))]
fn setup_global_shortcut(_: &tauri::AppHandle, _: &str) -> tauri::Result<()> {
    Ok(())
}

#[cfg(desktop)]
pub(crate) fn apply_activation_hotkey(
    app: &tauri::AppHandle,
    activation_hotkey: &str,
) -> Result<(), String> {
    use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

    let global_shortcut = app.global_shortcut();
    global_shortcut
        .unregister_all()
        .map_err(|e| e.to_string())?;
    global_shortcut
        .on_shortcut(activation_hotkey, |app, _, event| {
            if event.state == ShortcutState::Pressed {
                tray::show_and_focus_main_window(app, true);
            }
        })
        .map_err(|e| e.to_string())
}

#[cfg(not(desktop))]
pub(crate) fn apply_activation_hotkey(_: &tauri::AppHandle, _: &str) -> Result<(), String> {
    Ok(())
}

struct AppModelClient {
    router: Arc<RwLock<ModelRouter>>,
    settings: Arc<Mutex<Settings>>,
    telemetry_service: Arc<TelemetryService>,
}

#[async_trait::async_trait]
impl ModelClient for AppModelClient {
    #[instrument(skip_all, fields(model, provider = "router"))]
    async fn complete(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ModelCompletion, String> {
        let model = {
            let settings = self.settings.lock().await;
            settings
                .routing
                .interactive
                .as_ref()
                .map(|r| r.model.clone())
                .unwrap_or_else(|| settings.model.clone())
        };
        tracing::Span::current().record("model", model.as_str());

        let converted_messages = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                pares_agens_models::types::ChatMessage {
                    role,
                    content: Some(m.content.clone()),
                    tool_calls: m.tool_calls.clone().map(|calls| {
                        calls
                            .into_iter()
                            .map(|call| pares_agens_models::types::ToolCall {
                                id: call.id,
                                kind: "function".into(),
                                function: pares_agens_models::types::FunctionCall {
                                    name: call.name,
                                    arguments: call.arguments.to_string(),
                                },
                                index: None,
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.clone(),
                    name: None,
                }
            })
            .collect();

        let mut request = ChatCompletionRequest::new(&model, converted_messages);
        if !tools.is_empty() {
            request.tools = Some(
                tools
                    .iter()
                    .map(|tool| {
                        Tool::function(
                            tool.name.clone(),
                            tool.description.clone(),
                            tool.parameters.clone(),
                        )
                    })
                    .collect(),
            );
        }
        if let Some(temp) = options.temperature {
            request.temperature = Some(temp as f32);
        }
        if options.logprobs {
            request.logprobs = Some(true);
        }

        let start = std::time::Instant::now();
        let router_guard = self.router.read().await;
        let response = router_guard
            .chat(&request)
            .await
            .map_err(|e| e.to_string())?;
        drop(router_guard);
        let latency_ms = start.elapsed().as_millis();
        info!(latency_ms, model = %model, "model call completed");
        self.telemetry_service
            .record_model_call(latency_ms as u64)
            .await;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| "model returned no choices".to_string())?;

        let tool_calls = choice
            .message
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|call| {
                let args = call.function.arguments;
                pares_radix_core::model::ToolCall {
                    id: call.id,
                    name: call.function.name,
                    arguments: serde_json::from_str(&args)
                        .unwrap_or(serde_json::Value::String(args)),
                }
            })
            .collect();

        let logprobs = choice
            .logprobs
            .as_ref()
            .and_then(|lp| lp.content.as_ref())
            .map(|tokens| tokens.iter().filter_map(|t| t.logprob).collect::<Vec<_>>())
            .filter(|vals| !vals.is_empty());

        Ok(ModelCompletion {
            content: choice.message.content.clone(),
            tool_calls,
            logprobs,
            model: Some(response.model),
        })
    }

    #[instrument(skip_all, fields(model, provider = "router"))]
    async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
        tx: StreamSender,
    ) -> Result<ModelCompletion, String> {
        use futures_util::StreamExt;

        let model = {
            let settings = self.settings.lock().await;
            settings
                .routing
                .interactive
                .as_ref()
                .map(|r| r.model.clone())
                .unwrap_or_else(|| settings.model.clone())
        };
        tracing::Span::current().record("model", model.as_str());

        let converted_messages: Vec<pares_agens_models::types::ChatMessage> = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => Role::System,
                    "user" => Role::User,
                    "assistant" => Role::Assistant,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                pares_agens_models::types::ChatMessage {
                    role,
                    content: Some(m.content.clone()),
                    tool_calls: m.tool_calls.clone().map(|calls| {
                        calls
                            .into_iter()
                            .map(|call| pares_agens_models::types::ToolCall {
                                id: call.id,
                                kind: "function".into(),
                                function: pares_agens_models::types::FunctionCall {
                                    name: call.name,
                                    arguments: call.arguments.to_string(),
                                },
                                index: None,
                            })
                            .collect()
                    }),
                    tool_call_id: m.tool_call_id.clone(),
                    name: None,
                }
            })
            .collect();

        let mut request = ChatCompletionRequest::new(&model, converted_messages);
        if !tools.is_empty() {
            request.tools = Some(
                tools
                    .iter()
                    .map(|tool| {
                        Tool::function(
                            tool.name.clone(),
                            tool.description.clone(),
                            tool.parameters.clone(),
                        )
                    })
                    .collect(),
            );
        }
        if let Some(temp) = options.temperature {
            request.temperature = Some(temp as f32);
        }

        let start = std::time::Instant::now();
        let router_guard = self.router.read().await;
        let stream_result = router_guard.chat_stream(&request).await;
        drop(router_guard);

        let mut stream = match stream_result {
            Ok(s) => s,
            Err(e) => {
                // Fall back to non-streaming on stream error.
                let _ = tx.send(StreamDelta::Done);
                return Err(e.to_string());
            }
        };

        let mut full_content = String::new();
        let mut tool_calls: Vec<pares_radix_core::model::ToolCall> = Vec::new();
        // Buffer partial tool call arguments by index.
        let mut tc_args: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut response_model: Option<String> = None;

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    if response_model.is_none() {
                        response_model = Some(chunk.model.clone());
                    }
                    for choice in &chunk.choices {
                        // Text content delta.
                        if let Some(ref content) = choice.delta.content {
                            full_content.push_str(content);
                            let _ = tx.send(StreamDelta::Content(content.clone()));
                        }
                        // Tool call deltas.
                        if let Some(ref tc_deltas) = choice.delta.tool_calls {
                            for tc in tc_deltas {
                                let idx = tc.index.unwrap_or(0) as usize;
                                let func = &tc.function;
                                if !tc.id.is_empty() {
                                    // New tool call start.
                                    let name = func.name.clone();
                                    tc_args
                                        .insert(idx, (tc.id.clone(), name.clone(), String::new()));
                                    let _ = tx.send(StreamDelta::ToolCallStart {
                                        index: idx,
                                        id: tc.id.clone(),
                                        name,
                                    });
                                }
                                if !func.arguments.is_empty() {
                                    if let Some(entry) = tc_args.get_mut(&idx) {
                                        entry.2.push_str(&func.arguments);
                                    }
                                    let _ = tx.send(StreamDelta::ToolCallDelta {
                                        index: idx,
                                        arguments: func.arguments.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "stream chunk error");
                    break;
                }
            }
        }

        let _ = tx.send(StreamDelta::Done);

        let latency_ms = start.elapsed().as_millis();
        info!(latency_ms, model = %model, "streaming model call completed");
        self.telemetry_service
            .record_model_call(latency_ms as u64)
            .await;

        // Assemble tool calls from buffered fragments.
        for (_idx, (id, name, args)) in tc_args {
            tool_calls.push(pares_radix_core::model::ToolCall {
                id,
                name,
                arguments: serde_json::from_str(&args).unwrap_or(serde_json::Value::String(args)),
            });
        }

        Ok(ModelCompletion {
            content: if full_content.is_empty() {
                None
            } else {
                Some(full_content)
            },
            tool_calls,
            logprobs: None,
            model: response_model,
        })
    }
}

struct McpToolDispatcher {
    mcp_tools: Arc<RwLock<Vec<(String, mcp_client::protocol::Tool)>>>,
    mcp_clients: Arc<Mutex<std::collections::HashMap<String, mcp_client::McpClient>>>,
    settings: Arc<Mutex<Settings>>,
    telemetry_service: Arc<TelemetryService>,
}

#[async_trait::async_trait]
impl ToolDispatcher for McpToolDispatcher {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        let tool_list = self.mcp_tools.read().await;
        tool_list
            .iter()
            .map(|(_, tool)| ToolDefinition {
                name: tool.name.clone(),
                description: tool.description.clone().unwrap_or_default(),
                parameters: serde_json::to_value(&tool.input_schema).unwrap_or_default(),
            })
            .collect()
    }

    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        let telemetry_enabled = {
            let settings = self.settings.lock().await;
            settings.telemetry.enabled
        };
        if telemetry_enabled {
            self.telemetry_service.record_tool_usage(name).await;
        }

        let server_name = {
            let tool_list = self.mcp_tools.read().await;
            tool_list
                .iter()
                .find(|(_, tool)| tool.name == name)
                .map(|(server, _)| server.clone())
        };

        let mut clients = self.mcp_clients.lock().await;
        if let Some(server) = server_name {
            if let Some(client) = clients.get_mut(&server) {
                match client.call_tool(name, Some(arguments)).await {
                    Ok(result) => result
                        .content
                        .into_iter()
                        .filter_map(|c| match c {
                            mcp_client::protocol::ToolContent::Text { text } => {
                                Some(text)
                            }
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    Err(e) => format!("Error: {e}"),
                }
            } else {
                format!("MCP server '{server}' not connected")
            }
        } else {
            format!("No MCP server provides tool '{name}'")
        }
    }
}

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
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init());

    // Activate TUI mode when --tui flag is passed
    #[cfg(feature = "tui")]
    {
        let args: Vec<String> = std::env::args().collect();
        if args.contains(&"--tui".to_string()) {
            builder = builder.plugin(tauri_plugin_tui::init());
        }
    }

    builder
        .setup(|app| {
            // ── Memory store ──────────────────────────────────────────────
            // Open a persistent PluresDB-backed memory store under the app data
            // directory.  Fall back to an ephemeral in-memory store if the data
            // directory is unavailable (e.g. in sandboxed CI environments).
            //
            // The resulting `Arc<dyn MemoryStore>` is shared between `AppState`
            // and the `PluresLm` inside the agent so that autorecall sees all
            // captured memories.
            let memory_store: Arc<dyn MemoryStore> =
                match app.path().app_data_dir().ok().and_then(|dir| {
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
            let activation_hotkey = sanitize_activation_hotkey(&default_settings.activation_hotkey);
            let router_config = build_router_config(&default_settings);
            let system_prompt = default_settings.system_prompt.clone();
            let settings: Arc<Mutex<Settings>> = Arc::new(Mutex::new(default_settings));
            let model_router: Arc<RwLock<ModelRouter>> =
                Arc::new(RwLock::new(ModelRouter::new(router_config)));

            // MCP state shared between AppState and the adapter callback.
            let mcp_clients: Arc<Mutex<std::collections::HashMap<String, mcp_client::McpClient>>> =
                Arc::new(Mutex::new(std::collections::HashMap::new()));
            let mcp_tools: Arc<RwLock<Vec<(String, mcp_client::protocol::Tool)>>> =
                Arc::new(RwLock::new(Vec::new()));
            let telemetry_store: Arc<dyn StateStore> = match app
                .path()
                .app_data_dir()
                .ok()
                .and_then(|dir| PluresDbStateStore::open(dir.join("telemetry-state.db")).ok())
            {
                Some(store) => Arc::new(store),
                None => Arc::new(PluresDbStateStore::in_memory()),
            };
            let telemetry_service = Arc::new(TelemetryService::new(telemetry_store));

            // ── IPC bridge ────────────────────────────────────────────────
            let (adapter, handle) = tauri_ipc_channel("user");

            // Build the PluresLm instance that shares the backing store with
            // AppState so that autorecall sees all captured memories.
            let plures_lm = Arc::new(PluresLm::new(
                Arc::clone(&memory_store),
                Box::new(MockEmbedder),
                128_000,
            ));

            let model_client = Arc::new(AppModelClient {
                router: Arc::clone(&model_router),
                settings: Arc::clone(&settings),
                telemetry_service: Arc::clone(&telemetry_service),
            });
            let tool_dispatcher = Arc::new(McpToolDispatcher {
                mcp_tools: Arc::clone(&mcp_tools),
                mcp_clients: Arc::clone(&mcp_clients),
                settings: Arc::clone(&settings),
                telemetry_service: Arc::clone(&telemetry_service),
            });

            // Build the Agent with a Cerebellum wired in so every message
            // flows through autorecall and routing before being handled.
            let agent = Arc::new(
                Agent::with_cerebellum(
                    Arc::new(InMemory::new()),
                    Cerebellum::new(CerebellumConfig::default()),
                    plures_lm,
                )
                .with_model(model_client, tool_dispatcher, system_prompt),
            );

            // Spawn the adapter run-loop, routing all events through the agent
            let frontend_handle = app.handle().clone();
            let notification_settings = Arc::clone(&settings);
            tauri::async_runtime::spawn(async move {
                info!("Tauri IPC adapter starting (cerebellum + model client enabled)");
                adapter
                    .run(move |event: Event| {
                        let agent = Arc::clone(&agent);
                        let app_handle = frontend_handle.clone();
                        let notification_settings = Arc::clone(&notification_settings);
                        Box::pin(async move {
                            let request_id = match &event {
                                Event::Message { id, .. } => Some(id.clone()),
                                _ => None,
                            };

                            // Use streaming path for messages (real-time token emission).
                            let response = if let Some(ref req_id) = request_id {
                                let (tx, mut rx) =
                                    tokio::sync::mpsc::unbounded_channel::<StreamDelta>();
                                let stream_handle = app_handle.clone();
                                let stream_req_id = req_id.clone();

                                // Spawn a task to forward stream deltas to the frontend.
                                let forwarder = tokio::spawn(async move {
                                    while let Some(delta) = rx.recv().await {
                                        match delta {
                                            StreamDelta::Content(content) => {
                                                emit_with_warn(
                                                    &stream_handle,
                                                    "model-chunk",
                                                    &ModelChunkPayload {
                                                        request_id: stream_req_id.clone(),
                                                        content,
                                                        done: false,
                                                    },
                                                );
                                            }
                                            StreamDelta::Done => break,
                                            _ => {} // ToolCallStart/Delta handled internally
                                        }
                                    }
                                });

                                let resp = agent.handle_event_streaming(event, tx).await;
                                // Ensure forwarder completes.
                                let _ = forwarder.await;
                                resp
                            } else {
                                agent.handle_event(event).await
                            };

                            if let Some(content) =
                                response.as_ref().and_then(notifications::response_content)
                            {
                                let notifications_enabled = {
                                    let settings = notification_settings.lock().await;
                                    settings.preferences.notifications_enabled
                                };
                                if notifications_enabled {
                                    notifications::maybe_notify(
                                        &app_handle,
                                        content,
                                        request_id.is_none(),
                                    );
                                }
                            }

                            if let Some(request_id) = request_id {
                                match &response {
                                    Some(Event::ModelResponse { content, .. })
                                    | Some(Event::Message { content, .. }) => {
                                        // Emit the final done marker and full response.
                                        emit_with_warn(
                                            &app_handle,
                                            "model-chunk",
                                            &ModelChunkPayload {
                                                request_id: request_id.clone(),
                                                content: String::new(),
                                                done: true,
                                            },
                                        );

                                        emit_with_warn(
                                            &app_handle,
                                            "model-response",
                                            &ModelResponsePayload {
                                                request_id,
                                                content: content.clone(),
                                            },
                                        );
                                    }
                                    Some(other) => {
                                        emit_with_warn(
                                            &app_handle,
                                            "model-error",
                                            &ModelErrorPayload {
                                                request_id: request_id.clone(),
                                                error: format!(
                                                    "request {request_id} received unexpected event '{}'; expected model response content",
                                                    other.kind()
                                                ),
                                            },
                                        );
                                    }
                                    None => {
                                        emit_with_warn(
                                            &app_handle,
                                            "model-error",
                                            &ModelErrorPayload {
                                                request_id,
                                                error: "agent did not return a response"
                                                    .to_string(),
                                            },
                                        );
                                    }
                                }
                            }

                            response
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

            // Pre-seed Copilot provider API key from `gh auth token` if available.
            #[allow(clippy::let_underscore_future)]
            if let Ok(output) = std::process::Command::new("gh")
                .args(["auth", "token"])
                .output()
            {
                if output.status.success() {
                    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !token.is_empty() {
                        let _ = secret_store.set(&provider_api_key("copilot"), &token);
                        info!("Pre-seeded copilot provider from gh auth token");
                    }
                }
            }

            // Pre-seed Anthropic API key from ANTHROPIC_API_KEY env var.
            #[allow(clippy::let_underscore_future)]
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                if !key.is_empty() {
                    let _ = secret_store.set(&provider_api_key("anthropic"), &key);
                    info!("Pre-seeded anthropic provider from ANTHROPIC_API_KEY");
                }
            }

            // Pre-seed OpenAI API key from OPENAI_API_KEY env var.
            #[allow(clippy::let_underscore_future)]
            if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                if !key.is_empty() {
                    let _ = secret_store.set(&provider_api_key("openai"), &key);
                    info!("Pre-seeded openai provider from OPENAI_API_KEY");
                }
            }
            let plugin_runtime = Arc::new(PluginRuntime::new());
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
                license: Mutex::new(pares_radix_core::license::License::free()),
                telemetry_service: Arc::clone(&telemetry_service),
                plugin_runtime,
                plugin_executor: None, // TODO: wire CrdtStore when available
            });

            // ── Initial router rebuild ─────────────────────────────────────
            // The router was created from Settings::default() above.  Rebuild
            // it now that AppState (including the vault-backed SecretStore) is
            // managed so the initial router includes any persisted API keys.
            let startup_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = startup_handle.state::<AppState>();
                rebuild_model_router(&state).await;
                mcp::start_mcp_servers(&state).await;
            });

            // ── System tray ───────────────────────────────────────────────
            tray::setup_tray(app)?;
            setup_global_shortcut(app.handle(), &activation_hotkey)?;

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
            wizard::generate_swarm_invite,
            wizard::verify_swarm_join,
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
            commands::handle_notification_action,
            commands::get_license_status,
            commands::activate_license,
            commands::get_conversation_history,
            commands::get_telemetry_snapshot,
            commands::upload_telemetry_snapshot,
            plugins::plugin_install,
            plugins::plugin_list,
            plugins::plugin_uninstall,
            plugins::plugin_schema,
            plugins::plugin_crud_create,
            plugins::plugin_crud_list,
            plugins::plugin_crud_update,
            plugins::plugin_crud_delete,
            plugins::plugin_crud_search,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pares Radix");
}

// ── helpers ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::split_stream_chunks;

    #[test]
    fn split_stream_chunks_preserves_whitespace() {
        let chunks = split_stream_chunks("Hello world!\nNext");
        assert_eq!(chunks, vec!["Hello ", "world!\n", "Next"]);
    }

    #[test]
    fn split_stream_chunks_single_token() {
        let chunks = split_stream_chunks("token");
        assert_eq!(chunks, vec!["token"]);
    }
}
