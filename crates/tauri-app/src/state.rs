use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use pares_agens_channels::tauri_ipc::TauriIpcHandle;
use pares_agens_core::memory::store::InMemoryStore;

use crate::procedures::{ProcedureLogEntry, ProcedureRecord};

/// User-configurable settings stored in PluresDB state.
///
/// Persisted across sessions via [`crate::commands::get_settings`] /
/// [`crate::commands::set_settings`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Model identifier (e.g. `"qwen3:235b"`, `"llama3.1:8b"`).
    pub model: String,
    /// OpenAI-compatible endpoint URL (e.g. `"http://localhost:11434/v1"`).
    pub endpoint: String,
    /// Active channel name displayed in the UI header.
    pub channel: String,
    /// System prompt prepended to every conversation.
    pub system_prompt: String,
    /// Launch at system startup, minimised to the system tray.
    pub auto_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "qwen3:235b".to_string(),
            endpoint: "http://localhost:11434/v1".to_string(),
            channel: "tauri".to_string(),
            system_prompt: "You are Pares Agens, a helpful desktop AI assistant.".to_string(),
            auto_start: false,
        }
    }
}

/// Shared application state managed by Tauri.
///
/// Accessible in every Tauri command via `tauri::State<'_, AppState>`.
pub struct AppState {
    /// Handle to send user messages to the agent's IPC adapter.
    pub ipc_handle: TauriIpcHandle,
    /// In-process memory store — populated by the agent run-loop procedures.
    pub memory_store: Arc<InMemoryStore>,
    /// User-configurable settings (model, endpoint, channel, …).
    pub settings: Mutex<Settings>,
    /// All registered procedure records (config + DSL body).
    pub procedures: Mutex<Vec<ProcedureRecord>>,
    /// Execution log for all procedures (most recent last).
    pub procedure_log: Mutex<Vec<ProcedureLogEntry>>,
}
