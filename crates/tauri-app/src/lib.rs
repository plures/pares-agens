use std::sync::Arc;

use tauri::Manager;
use tokio::sync::Mutex;
use tracing::info;

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
use pares_agens_core::memory::store::InMemoryStore;
use pares_agens_core::Event;

use crate::state::{AppState, Settings};

mod commands;
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
            // ── IPC bridge ────────────────────────────────────────────────
            let (adapter, handle) = tauri_ipc_channel("user");

            // Spawn the adapter run-loop.  In production this would be wired
            // to the real OnMessage procedure via a ProcedureRegistry.  Here
            // we use an echo handler so the scaffold is fully functional out
            // of the box without requiring a running LLM endpoint.
            tauri::async_runtime::spawn(async move {
                info!("Tauri IPC adapter starting");
                adapter
                    .run(|event: Event| {
                        Box::pin(async move {
                            if let Event::Message { id, content, .. } = event {
                                Some(Event::ModelResponse {
                                    request_id: id,
                                    model: "echo".into(),
                                    // Real integration: call OnMessage procedure here.
                                    content: format!("Echo: {content}"),
                                })
                            } else {
                                None
                            }
                        })
                    })
                    .await
                    .ok();
            });

            // ── AppState ──────────────────────────────────────────────────
            let memory_store = Arc::new(InMemoryStore::new());
            app.manage(AppState {
                ipc_handle: handle,
                memory_store,
                settings: Mutex::new(Settings::default()),
                wizard_completed: Mutex::new(false),
                procedures: Mutex::new(Vec::new()),
                procedure_log: Mutex::new(Vec::new()),
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
            wizard::detect_docker_runner,
            wizard::validate_api_key,
            wizard::is_wizard_completed,
            wizard::complete_wizard,
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
