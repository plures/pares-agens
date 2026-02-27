use std::sync::Arc;

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    App, Manager,
};
use tokio::sync::Mutex;
use tracing::info;

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::tauri_ipc::tauri_ipc_channel;
use pares_agens_core::memory::store::InMemoryStore;
use pares_agens_core::Event;

use crate::state::{AppState, Settings};

mod commands;
mod migration;
mod state;

/// Entry point called from `main.rs`.
///
/// Wires up:
/// - Tauri IPC adapter ↔ core agent event loop (background task)
/// - System tray with Show / Quit menu items
/// - Shared [`AppState`] exposed to every Tauri command
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            });

            // ── System tray ───────────────────────────────────────────────
            setup_tray(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::send_message,
            commands::get_memories,
            commands::get_settings,
            commands::set_settings,
            migration::migration_detect,
            migration::migration_preview,
            migration::migration_run,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Pares Agens");
}

/// Build and register the system tray icon with a minimal context menu.
fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::new()
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => {
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}
