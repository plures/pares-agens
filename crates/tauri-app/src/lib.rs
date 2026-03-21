use std::sync::Arc;

use tauri::Manager;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info};

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
use pares_agens_core::agent::{Agent, InMemory};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::memory::embed::MockEmbedder;
use pares_agens_core::memory::store::{InMemoryStore, MemoryStore};
use pares_agens_core::memory::store::PluresDbStore;
use pares_agens_core::memory::PluresLm;
use pares_agens_core::optimization::OptimizationSafetyGate;
use pares_agens_core::praxis::GuidanceService;
use pares_agens_core::secrets::InMemorySecretStore;
use pares_agens_core::Event;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role};
use pares_models::ModelRouter;

use crate::state::{build_router_config, AppState, Settings};

mod commands;
mod settings;
mod migration;
mod procedures;
mod state;
mod wizard;
pub mod tray;

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
                    PluresDbStore::open(&dir.join("memory.db"))
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
            tauri::async_runtime::spawn(async move {
                info!("Tauri IPC adapter starting (cerebellum + model router enabled)");
                adapter
                    .run(move |event: Event| {
                        let agent = Arc::clone(&agent);
                        let router = Arc::clone(&router_for_cb);
                        let settings = Arc::clone(&settings_for_cb);
                        Box::pin(async move {
                            // Let the agent run cerebellum preprocessing (autorecall,
                            // routing, drop filtering) and capture the message in memory.
                            let preprocessed = agent.handle_event(event.clone()).await;

                            // If the cerebellum dropped the event, respect that.
                            if preprocessed.is_none() {
                                return None;
                            }

                            // For message events, replace the echo response with a real
                            // model call via the ModelRouter.
                            if let Event::Message { ref id, ref content, .. } = event {
                                let (model, system_prompt) = {
                                    let s = settings.lock().await;
                                    (s.model.clone(), s.system_prompt.clone())
                                };

                                let messages = vec![
                                    ChatMessage::text(Role::System, &system_prompt),
                                    ChatMessage::text(Role::User, content),
                                ];
                                let request = ChatCompletionRequest::new(&model, messages);

                                let router_guard = router.read().await;
                                match router_guard.chat(&request).await {
                                    Ok(response) => {
                                        let reply = response
                                            .choices
                                            .first()
                                            .and_then(|c| c.message.content.as_ref())
                                            .cloned()
                                            .unwrap_or_default();
                                        Some(Event::ModelResponse {
                                            request_id: id.clone(),
                                            model,
                                            content: reply,
                                        })
                                    }
                                    Err(e) => {
                                        error!(error = %e, "model router call failed");
                                        Some(Event::Message {
                                            id: format!("{id}-error"),
                                            channel: "system".into(),
                                            sender: "agent".into(),
                                            content: format!(
                                                "⚠️ Could not reach the model provider: {e}\n\n\
                                                 Please check your provider settings and ensure \
                                                 the model endpoint is accessible."
                                            ),
                                        })
                                    }
                                }
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pares Agens");
}
