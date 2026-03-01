use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use pares_agens_channels::tauri_ipc::TauriIpcHandle;
use pares_agens_core::memory::store::InMemoryStore;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// A single model provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    /// Unique identifier for this provider (e.g. `"ollama"`, `"openai"`).
    pub name: String,
    /// OpenAI-compatible base URL (e.g. `"http://localhost:11434/v1"`).
    pub base_url: String,
    /// Bearer token / API key.
    ///
    /// Stored internally; never returned verbatim — masked before sending to
    /// the UI.  In a full PluresDB integration this value would be encrypted
    /// at rest.
    #[serde(skip_serializing, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model IDs known to be available through this provider.
    #[serde(default)]
    pub models: Vec<String>,
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// References a specific model on a named provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// Provider name (must match a key in [`Settings::providers`]).
    pub provider: String,
    /// Model identifier accepted by that provider's API.
    pub model: String,
}

/// Per–use-case model routing preferences.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RoutingPrefs {
    /// Model to use for real-time interactive conversations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interactive: Option<ModelRef>,
    /// Model to use for background / long-running tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<ModelRef>,
    /// Model to use for code generation and editing tasks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coding: Option<ModelRef>,
}

// ---------------------------------------------------------------------------
// Channel adapters
// ---------------------------------------------------------------------------

/// Configuration for a single channel adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelAdapterConfig {
    /// Adapter kind: `"telegram"`, `"signal"`, or `"local"`.
    pub kind: String,
    /// Whether this adapter is currently active.
    pub enabled: bool,
    /// Telegram bot token (Telegram adapters only).
    #[serde(skip_serializing)]
    pub bot_token: Option<String>,
    /// Phone number for Signal / SMS adapters.
    #[serde(skip_serializing)]
    pub phone_number: Option<String>,
}

// ---------------------------------------------------------------------------
// Agent preferences
// ---------------------------------------------------------------------------

/// General agent / UX preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPreferences {
    /// Display name shown in the UI header.
    pub agent_name: String,
    /// Optional personality notes appended to the system prompt.
    pub personality_notes: String,
    /// Whether the agent should auto-recall relevant memories each turn.
    pub auto_recall: bool,
    /// Memory categories the agent actively captures.
    #[serde(default)]
    pub capture_categories: Vec<String>,
    /// Whether desktop notifications are enabled.
    pub notifications_enabled: bool,
}

impl Default for AgentPreferences {
    fn default() -> Self {
        Self {
            agent_name: "Pares Agens".to_string(),
            personality_notes: String::new(),
            auto_recall: true,
            capture_categories: vec![
                "code-pattern".to_string(),
                "preference".to_string(),
                "decision".to_string(),
            ],
            notifications_enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level Settings
// ---------------------------------------------------------------------------

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
    /// Configured model providers (ordered list).
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    /// Per–use-case model routing preferences.
    #[serde(default)]
    pub routing: RoutingPrefs,
    /// Channel adapter configurations.
    #[serde(default)]
    pub channel_adapters: Vec<ChannelAdapterConfig>,
    /// General agent preferences.
    #[serde(default)]
    pub preferences: AgentPreferences,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "qwen3:235b".to_string(),
            endpoint: "http://localhost:11434/v1".to_string(),
            channel: "tauri".to_string(),
            system_prompt: "You are Pares Agens, a helpful desktop AI assistant.".to_string(),
            auto_start: false,
            providers: vec![ProviderEntry {
                name: "ollama".to_string(),
                base_url: "http://localhost:11434/v1".to_string(),
                api_key: None,
                models: vec!["qwen3:235b".to_string(), "llama3.1:8b".to_string()],
            }],
            routing: RoutingPrefs::default(),
            channel_adapters: vec![
                ChannelAdapterConfig {
                    kind: "local".to_string(),
                    enabled: true,
                    bot_token: None,
                    phone_number: None,
                },
                ChannelAdapterConfig {
                    kind: "telegram".to_string(),
                    enabled: false,
                    bot_token: None,
                    phone_number: None,
                },
            ],
            preferences: AgentPreferences::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

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
}
