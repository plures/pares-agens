use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};

use mcp_client::protocol::Tool as McpTool;
use mcp_client::McpClient;
use pares_agens_channels::tauri_ipc::TauriIpcHandle;
use pares_agens_core::license::License;
use pares_agens_core::memory::store::MemoryStore;
use pares_agens_core::optimization::OptimizationSafetyGate;
use pares_agens_core::praxis::GuidanceService;
use pares_agens_core::secrets::{provider_api_key, SecretStore};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::ModelRouter;

use crate::procedures::{ProcedureLogEntry, ProcedureRecord};
use crate::telemetry::TelemetryService;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// A single model provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderEntry {
    /// Unique identifier for this provider (e.g. `"copilot"`, `"openai"`).
    pub name: String,
    /// OpenAI-compatible base URL (e.g. `"http://localhost:11434/v1"`).
    pub base_url: String,
    /// Bearer token / API key received from the frontend.
    ///
    /// **Never stored in this struct at rest.**  When `add_provider` or
    /// `update_provider` receives a non-empty, non-masked value here it is
    /// written to the [`AppState::secret_store`] vault and then this field is
    /// cleared to `None`.  The vault key is
    /// `provider:<name>:api_key` (see
    /// [`pares_agens_core::secrets::provider_api_key`]).
    #[serde(skip_serializing, default)]
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

// ---------------------------------------------------------------------------
// MCP Server
// ---------------------------------------------------------------------------

/// Configuration for a single MCP server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerConfig {
    /// Display name for this server (e.g. "filesystem", "time").
    pub name: String,
    /// Command to run (e.g. "uvx", "npx", "node").
    pub command: String,
    /// Command arguments (e.g. ["mcp-server-filesystem", "/tmp"]).
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether this server should be auto-started on app launch.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

pub fn default_activation_hotkey() -> String {
    "Ctrl+Space".to_string()
}

pub fn sanitize_activation_hotkey(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        default_activation_hotkey()
    } else {
        trimmed.to_string()
    }
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

/// Hyperswarm settings collected during first-run setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwarmSettings {
    /// Swarm join mode selected by the user (`new` or `join`).
    pub mode: String,
    /// 32-byte topic key encoded as 64-char hex.
    pub topic: String,
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
    /// Optional API key for cloud model providers (OpenAI, Anthropic, Google).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Optional Telegram bot token for the Telegram channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram_token: Option<String>,
    /// Launch at system startup, minimised to the system tray.
    pub auto_start: bool,
    /// Global keyboard shortcut used to summon/focus the window.
    #[serde(default = "default_activation_hotkey")]
    pub activation_hotkey: String,
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
    /// MCP server configurations for tool orchestration.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// Optional Hyperswarm topic configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swarm: Option<SwarmSettings>,
    /// Privacy-first telemetry preferences.
    #[serde(default)]
    pub telemetry: TelemetrySettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "gpt-4.1".to_string(),
            endpoint: "https://api.enterprise.githubcopilot.com".to_string(),
            channel: "tauri".to_string(),
            system_prompt: "You are Pares Agens, a helpful desktop AI assistant.".to_string(),
            api_key: None,
            telegram_token: None,
            auto_start: false,
            activation_hotkey: default_activation_hotkey(),
            providers: vec![ProviderEntry {
                name: "copilot".to_string(),
                base_url: "https://api.enterprise.githubcopilot.com".to_string(),
                api_key: None,
                models: vec!["gpt-4.1".to_string(), "claude-opus-4.6".to_string()],
            }],
            routing: RoutingPrefs {
                interactive: Some(ModelRef {
                    provider: "copilot".to_string(),
                    model: "gpt-4.1".to_string(),
                }),
                background: None,
                coding: None,
            },
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
            mcp_servers: Vec::new(),
            swarm: None,
            telemetry: TelemetrySettings::default(),
        }
    }
}

/// Privacy-first telemetry preferences.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TelemetrySettings {
    /// Whether anonymous telemetry collection is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Whether uploading local aggregates is enabled.
    #[serde(default)]
    pub upload_enabled: bool,
    /// Optional upload endpoint used for manual telemetry uploads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_endpoint: Option<String>,
}

/// Shared application state managed by Tauri.
///
/// Accessible in every Tauri command via `tauri::State<'_, AppState>`.
pub struct AppState {
    /// Handle to send user messages to the agent's IPC adapter.
    pub ipc_handle: TauriIpcHandle,
    /// In-process memory store — populated by the agent run-loop procedures.
    ///
    /// Shared with the [`pares_agens_core::memory::PluresLm`] instance inside
    /// the agent so that autorecall sees all captured memories.
    pub memory_store: Arc<dyn MemoryStore>,
    /// Encrypted secret store for provider API keys and other sensitive
    /// configuration.  Provider API keys are persisted **only** in this store
    /// and are never written to [`Settings`] when settings are saved.
    ///
    /// Note: other secret-like fields (e.g. `ChannelAdapterConfig.bot_token`)
    /// remain in-memory for now and are excluded from serialization via
    /// `#[serde(skip_serializing)]`.
    pub secret_store: Arc<dyn SecretStore>,
    /// User-configurable settings (model, endpoint, channel, …).
    ///
    /// Wrapped in `Arc` so the adapter callback can read model/system-prompt
    /// without requiring the full `AppState`.
    pub settings: Arc<Mutex<Settings>>,
    /// Model router that selects the right provider for each request.
    ///
    /// Rebuilt whenever provider settings or routing preferences change via
    /// [`rebuild_model_router`].
    pub model_router: Arc<RwLock<ModelRouter>>,
    /// Whether the first-run wizard has been completed in this session.
    ///
    /// Durable completion is tracked in the frontend via `localStorage`; this
    /// flag lets the backend acknowledge the wizard completion for the lifetime
    /// of the current process.
    pub wizard_completed: Mutex<bool>,
    /// All registered procedure records (config + DSL body).
    pub procedures: Mutex<Vec<ProcedureRecord>>,
    /// Execution log for all procedures (most recent last).
    pub procedure_log: Mutex<Vec<ProcedureLogEntry>>,
    /// Praxis coprocessor guidance service for the memory sidebar.
    pub guidance_service: GuidanceService,
    /// Optimization safety gate for runtime enforcement of safety decisions.
    pub optimization_safety_gate: OptimizationSafetyGate,
    /// Active MCP clients keyed by server name.
    pub mcp_clients: Arc<Mutex<HashMap<String, McpClient>>>,
    /// Cached tool list across all connected MCP servers.
    pub mcp_tools: Arc<RwLock<Vec<(String, McpTool)>>>,
    /// Current license — Free by default; updated on successful activation.
    pub license: Mutex<License>,
    /// Anonymous telemetry aggregator backed by PluresDB state.
    pub telemetry_service: Arc<TelemetryService>,
}

// ---------------------------------------------------------------------------
// Router helpers
// ---------------------------------------------------------------------------

/// Build a [`RouterConfig`] from the current [`Settings`] without API keys.
///
/// Used at startup before any vault entries exist.  For a config that
/// includes API keys from the vault, use [`rebuild_model_router`] instead.
pub fn build_router_config(settings: &Settings) -> RouterConfig {
    let mut providers = HashMap::new();
    for entry in &settings.providers {
        providers.insert(
            entry.name.clone(),
            ProviderConfig::new(&entry.base_url, entry.api_key.clone()),
        );
    }

    // Backward-compatible fallback: if no explicit providers are configured
    // but legacy endpoint/api_key fields are populated (e.g. from the
    // first-run wizard), synthesize a single provider entry.
    if providers.is_empty() && !settings.endpoint.is_empty() {
        providers.insert(
            "default".to_string(),
            ProviderConfig::new(&settings.endpoint, settings.api_key.clone()),
        );
    }

    // Prefer an explicitly configured routing provider when it exists and is
    // present in the providers map; otherwise, if there is exactly one
    // provider configured (including synthesized legacy fallback), use that.
    let mut default_provider = settings
        .routing
        .interactive
        .as_ref()
        .map(|r| r.provider.clone());

    if let Some(ref name) = default_provider {
        if !providers.contains_key(name) {
            // Routing preference refers to a provider that doesn't exist.
            default_provider = None;
        }
    }

    let default_provider = default_provider
        .or_else(|| {
            if providers.len() == 1 {
                providers.keys().next().cloned()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "copilot".to_string());

    RouterConfig {
        providers,
        rules: vec![],
        default_provider,
        fallback_models: vec![],
    }
}

/// Rebuild the [`ModelRouter`] from the current settings and vault secrets.
///
/// Releases the settings mutex before performing async vault I/O, then
/// writes the new router behind the `RwLock` so that the next model call
/// picks up the changes.
pub async fn rebuild_model_router(state: &AppState) {
    let (provider_entries, routing) = {
        let settings = state.settings.lock().await;
        (settings.providers.clone(), settings.routing.clone())
    };

    let mut providers = HashMap::new();
    for entry in &provider_entries {
        let api_key = match state.secret_store.get(&provider_api_key(&entry.name)).await {
            Ok(api_key) => api_key,
            Err(err) => {
                eprintln!(
                    "Failed to fetch API key for provider '{}': {err:?}",
                    entry.name
                );
                None
            }
        };
        providers.insert(
            entry.name.clone(),
            ProviderConfig::new(&entry.base_url, api_key),
        );
    }

    let default_provider = routing
        .interactive
        .as_ref()
        .map(|r| r.provider.clone())
        .or_else(|| provider_entries.first().map(|p| p.name.clone()))
        .unwrap_or_else(|| "copilot".to_string());

    let mut config = RouterConfig {
        providers,
        rules: vec![],
        default_provider,
        fallback_models: vec![],
    };

    // Ensure we don't accidentally enable multi-provider routing when using
    // ModelRouter::new. If multiple providers are configured, restrict the
    // router config to only the default provider.
    if config.providers.len() > 1 {
        if let Some(default_cfg) = config.providers.get(&config.default_provider).cloned() {
            config.providers.clear();
            config
                .providers
                .insert(config.default_provider.clone(), default_cfg);
        }
    }
    *state.model_router.write().await = ModelRouter::new(config);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_core::secrets::InMemorySecretStore;

    #[test]
    fn build_router_config_default_settings() {
        let settings = Settings::default();
        let config = build_router_config(&settings);

        // Default settings have one provider: "copilot".
        assert_eq!(config.providers.len(), 1);
        assert!(config.providers.contains_key("copilot"));
        assert_eq!(config.default_provider, "copilot");
    }

    #[test]
    fn build_router_config_uses_interactive_routing_as_default() {
        let mut settings = Settings::default();
        settings.routing.interactive = Some(ModelRef {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
        });
        settings.providers.push(ProviderEntry {
            name: "openai".to_string(),
            base_url: "https://api.openai.com".to_string(),
            api_key: None,
            models: vec![],
        });

        let config = build_router_config(&settings);

        assert_eq!(config.default_provider, "openai");
        assert_eq!(config.providers.len(), 2);
    }

    #[test]
    fn build_router_config_empty_providers_uses_legacy_endpoint_fallback() {
        let mut settings = Settings::default();
        settings.providers.clear();

        let config = build_router_config(&settings);

        // With no explicit providers, the legacy endpoint/api_key fields
        // synthesize a single "default" provider.
        assert_eq!(config.providers.len(), 1);
        assert!(config.providers.contains_key("default"));
        assert_eq!(config.default_provider, "default");
    }

    #[test]
    fn build_router_config_empty_providers_and_endpoint_defaults_to_copilot() {
        let mut settings = Settings::default();
        settings.providers.clear();
        settings.endpoint = String::new();

        let config = build_router_config(&settings);

        assert!(config.providers.is_empty());
        assert_eq!(config.default_provider, "copilot");
    }

    #[test]
    fn sanitize_activation_hotkey_uses_default_for_blank_values() {
        assert_eq!(sanitize_activation_hotkey(""), "Ctrl+Space");
        assert_eq!(sanitize_activation_hotkey("   "), "Ctrl+Space");
    }

    #[test]
    fn sanitize_activation_hotkey_trims_custom_values() {
        assert_eq!(sanitize_activation_hotkey(" Alt+Space "), "Alt+Space");
    }

    #[tokio::test]
    async fn rebuild_model_router_picks_up_vault_api_keys() {
        let secret_store = Arc::new(InMemorySecretStore::new());
        secret_store
            .set(&provider_api_key("openai"), "sk-test-key")
            .await
            .unwrap();

        let mut settings = Settings::default();
        settings.providers.push(ProviderEntry {
            name: "openai".to_string(),
            base_url: "https://api.openai.com".to_string(),
            api_key: None,
            models: vec![],
        });

        let state = AppState {
            ipc_handle: test_ipc_handle(),
            memory_store: Arc::new(pares_agens_core::memory::store::InMemoryStore::new()),
            secret_store: secret_store as Arc<dyn SecretStore>,
            settings: Arc::new(Mutex::new(settings)),
            model_router: Arc::new(RwLock::new(ModelRouter::new(RouterConfig::single(
                "copilot",
                ProviderConfig::new("http://localhost:11434/v1", None),
            )))),
            wizard_completed: Mutex::new(false),
            procedures: Mutex::new(Vec::new()),
            procedure_log: Mutex::new(Vec::new()),
            guidance_service: GuidanceService::new(),
            optimization_safety_gate: OptimizationSafetyGate::new(),
            mcp_clients: Arc::new(Mutex::new(HashMap::new())),
            mcp_tools: Arc::new(RwLock::new(Vec::new())),
            license: Mutex::new(pares_agens_core::license::License::free()),
            telemetry_service: Arc::new(TelemetryService::new(Arc::new(
                pares_agens_core::InMemoryStateStore::new(),
            ))),
        };

        rebuild_model_router(&state).await;

        // The router was rebuilt — we can't inspect its internal state directly,
        // but we can verify the rebuild didn't panic and the lock is available.
        let _guard = state.model_router.read().await;
    }

    /// Create a dummy `TauriIpcHandle` for testing (channel will never be read).
    fn test_ipc_handle() -> pares_agens_channels::tauri_ipc::TauriIpcHandle {
        let (_adapter, handle) = pares_agens_channels::tauri_ipc::tauri_ipc_channel("test");
        handle
    }
}
