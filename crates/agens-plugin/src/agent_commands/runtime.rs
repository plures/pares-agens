//! Agent runtime helpers carved from radix cli-runtime (Stage 4c).
//! Structs, impls, and free functions backing the agent commands
//! (serve-spine/serve/tui/ask/classify). Moved verbatim from
//! pares-radix-cli-runtime; the host no longer carries this surface.
#![allow(
    dead_code,
    unused_imports,
    clippy::all,
    clippy::pedantic,
    clippy::needless_return
)]

use super::px_config;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
#[allow(unused_imports)]
use tracing::{debug, error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;
use walkdir::WalkDir;

use reqwest::header::{HeaderMap, HeaderValue};

use pares_agens_bitnet::BitnetModelClient;
use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{
    TelegramAdapter, TelegramConfig, TelegramConfigControl, TelegramModelControl,
    TelegramPersonalityControl, TelegramRuntimeConfig, TelegramRuntimeControl,
};
use pares_agens_core::agent::{Agent, Memory};
use pares_agens_hostkit::{
    apply_runtime_log_level, build_env_filter, current_hostname, current_process_rss_kib,
    default_deep_escalation_enabled, detect_single_connection_conflicts,
    extract_verbose_tool_marker, format_verbose_tool_traces, manus_request_for_tool,
    parse_sync_topic_key, parse_tool_args, redact_connection_id, spawn_memory_monitor,
    spawn_systemd_watchdog, systemd_notify, value_to_tool_content, ToolCallTrace,
};
use pares_radix_core::auth::copilot::{CopilotAuth, CopilotModelClient};
use pares_agens_core::orchestrator::px_bridge::PxBridge;
use pares_agens_core::orchestrator::spine_contract::{
    autonomous_dispatch_catalog, AUTONOMOUS_DISPATCH_PROFILE,
};
use pares_agens_core::orchestrator::{Orchestrator, CerebellumConfig};
use pares_agens_core::delegation::{broker::DelegationBroker, registry::AgentRegistry};
use pares_agens_core::memory::{
    embed::{EmbeddingProvider, MockEmbedder, OpenAiEmbedder},
    entry::Exchange,
    store::{HostAdapterConfig, HostAdapterRecord, PluresDbStore},
    PluresLm,
};
use pares_radix_core::model::{
    ChatMessage as CoreChatMessage, ChatOptions, ModelClient, ModelClientError, ToolDefinition,
    ToolDispatcher, TransportFailure,
};
use pares_radix_core::task::{CompletionCondition, ConditionType};
use pares_radix_core::task_manager::TaskManager;
use pares_radix_core::plugins::{PluginCrudExecutor, PluginRuntime};
use pares_radix_core::procedure::{Procedure, ProcedureRegistry};
use pares_radix_core::shell_executor::{ExecRequest, ShellExecutor};
use pares_radix_core::tool_governance::{GovernanceVerdict, ToolGovernor};
use pares_radix_core::Event;
use pares_radix_core::{PluresDbStateStore, StateStore};
use pares_agens_models::config::{ProviderConfig, RouterConfig};
use pares_agens_models::router::ModelRouter;
use pares_agens_models::types::{ChatCompletionRequest, ChatMessage, Role, Tool};

struct RouterModelClient {
    router: Arc<RwLock<Arc<ModelRouter>>>,
    model: Arc<RwLock<String>>,
    endpoint: Arc<RwLock<String>>,
    api_key: Option<String>,
}

struct ToggleableModelClient {
    inner: Arc<dyn ModelClient>,
    enabled: Arc<RwLock<bool>>,
}

/// Channel-independent command gate for the live Spine model boundary.
///
/// Every channel ultimately produces a `ModelRequest`; handling operational
/// commands here therefore keeps Telegram, stdio/TUI, and HTTP consistent and
/// prevents them from being answered by a model.  This is deliberately a small
/// diagnostics I/O edge.  Conversational routing remains `.px`-owned.
struct SpineCommandGate {
    inner: pares_radix_core::spine::procedures::model_invoker::ModelInvoker,
    model: String,
    tool_count: usize,
}

impl SpineCommandGate {
    fn new(
        inner: pares_radix_core::spine::procedures::model_invoker::ModelInvoker,
        model: String,
        tool_count: usize,
    ) -> Self {
        Self {
            inner,
            model,
            tool_count,
        }
    }
}

fn spine_command_reply(content: &str, model: &str, tool_count: usize) -> Option<String> {
    let command = content
        .split_whitespace()
        .next()?
        .strip_prefix('/')?
        .split('@')
        .next()?
        .to_ascii_lowercase();

    match command.as_str() {
        "start" | "help" | "commands" => Some([
            "Pares Agens Spine commands",
            "/commands or /help — show this command list",
            "/status or /health — live runtime snapshot",
            "/version — build information",
            "",
            "Commands are handled before model invocation on every channel.",
        ].join("\n")),
        "status" | "health" => {
            let uptime = spine_process_uptime();
            let hostname = current_hostname();
            let rss = current_process_rss_kib()
                .map(|value| format!("{value} KiB"))
                .unwrap_or_else(|| "n/a".to_string());
            Some(format!(
                "🤖 Pares Agens v{} (Spine runtime)\n\
                 ⏱️ Uptime: {uptime} · PID: {} · RSS: {rss}\n\
                 🧠 Model: {model}\n\
                 ⚡ Event Spine: active\n\
                 🔧 Tools: {tool_count} registered\n\
                 🗄 PluresDB: ~/.pares-radix/runtime-state/\n\
                 🖥 Host: {hostname}",
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
            ))
        }
        "version" => Some(format!(
            "Pares Agens v{} (Spine runtime active)",
            env!("CARGO_PKG_VERSION")
        )),
        _ => Some(format!(
            "/{command} is not registered by the live Spine command surface. Use /commands."
        )),
    }
}

fn spine_process_uptime() -> String {
    let seconds = std::fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|contents| contents.split_whitespace().next()?.parse::<f64>().ok())
        .and_then(|system_uptime| {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id())).ok()?;
            let start_ticks = stat.split_whitespace().nth(21)?.parse::<u64>().ok()?;
            Some((system_uptime as u64).saturating_sub(start_ticks / 100))
        });
    seconds
        .map(|value| format!("{}h {}m", value / 3600, (value % 3600) / 60))
        .unwrap_or_else(|| "unavailable".to_string())
}

#[async_trait]
impl pares_radix_core::spine::pipeline::SpineProcedure for SpineCommandGate {
    fn name(&self) -> &str {
        "spine_command_gate"
    }

    fn handles(&self) -> Option<Vec<&'static str>> {
        Some(vec!["model_request"])
    }

    async fn handle(
        &self,
        event: &pares_radix_core::spine::event::SpineEvent,
        emitter: &pares_radix_core::spine::pipeline::PipelineEmitter,
    ) {
        use pares_radix_core::spine::event::SpineEvent;
        use pares_radix_core::spine::pipeline::SpineProcedure;

        let SpineEvent::ModelRequest {
            source,
            chat_id,
            content,
            metadata,
            ..
        } = event
        else {
            return;
        };

        if let Some(reply) = spine_command_reply(content, &self.model, self.tool_count) {
            emitter
                .emit(SpineEvent::DeliveryRequest {
                    id: SpineEvent::new_id(),
                    channel: source.clone(),
                    chat_id: chat_id.clone(),
                    content: reply,
                    metadata: metadata.clone(),
                })
                .await;
            return;
        }

        self.inner.handle(event, emitter).await;
    }
}

impl ToggleableModelClient {
    fn new(inner: Arc<dyn ModelClient>, enabled: Arc<RwLock<bool>>) -> Self {
        Self { inner, enabled }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CopilotAuthCache {
    oauth_token: String,
    /// Epoch seconds when this OAuth token was cached. OAuth tokens don't
    /// technically expire, but GitHub can revoke them. If the token is older
    /// than 30 days, we force re-auth to avoid stale credentials.
    #[serde(default)]
    cached_at: u64,
}

const MODEL_OVERRIDE_STATE_KEY: &str = "agent.runtime_model_override";
const RUNTIME_CONFIG_OVERRIDE_STATE_KEY: &str = "agent.runtime_config_override";

// Telegram request ID currently being processed on this task.
// Used to correlate tool calls executed during `agent.handle_event(...)` with
// the originating Telegram message so verbose tool details can be appended.
tokio::task_local! {
    static ACTIVE_TELEGRAM_REQUEST_ID: String;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeModelOverride {
    model: String,
    deep_model: String,
    #[serde(default = "default_deep_escalation_enabled")]
    deep_escalation_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RuntimeConfigOverride {
    model: String,
    endpoint: String,
    log_level: String,
}

struct RuntimeModelControl {
    primary_model: Arc<RwLock<String>>,
    deep_model: Arc<RwLock<String>>,
    fast_model: Arc<RwLock<String>>,
    deep_escalation_enabled: Arc<RwLock<bool>>,
    state_store: Arc<dyn StateStore>,
    /// Full list of models discovered at boot (Copilot API). Retained so the
    /// `/models` command can enumerate real data instead of a hardcoded list.
    /// Empty when discovery returned nothing or failed (honest absence).
    available_models:
        Arc<RwLock<Vec<pares_radix_core::auth::copilot::AvailableModel>>>,
    /// Handle to the live agent, populated after agent construction so the
    /// `/status` command can read the last-routed tier. `None` until wired.
    agent_ref: Arc<RwLock<Option<Arc<Agent>>>>,
}


struct RuntimeConfigControl {
    model_control: Arc<RuntimeModelControl>,
    primary_client: Arc<RouterModelClient>,
    state_store: Arc<dyn StateStore>,
    log_level: Arc<RwLock<String>>,
    log_filter_handle: tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>,
}

#[derive(Clone)]
struct RuntimeResetControl {
    agent: Arc<RwLock<Arc<Agent>>>,
    factory: Arc<RuntimeAgentFactory>,
}

#[allow(dead_code)]
struct RuntimePersonalityControl {
    state_store: Arc<dyn StateStore>,
    agent: Arc<RwLock<Arc<Agent>>>,
}

#[derive(Clone)]
struct RuntimeAgentFactory {
    store: Arc<PluresDbStore>,
    model_client: Arc<dyn ModelClient>,
    deep_model_client: Arc<dyn ModelClient>,
    fast_model_client: Option<Arc<dyn ModelClient>>,
    tool_dispatcher: Arc<dyn ToolDispatcher>,
    registry: Arc<AgentRegistry>,
    embed_url: Option<String>,
    embed_model: String,
    api_key: Option<String>,
    system_prompt_path: Option<PathBuf>,
    #[allow(dead_code)]
    cerebellum_model_path: Option<PathBuf>,
}


#[derive(Clone, Default)]
struct ToolTraceStore {
    traces: Arc<Mutex<HashMap<String, Vec<ToolCallTrace>>>>,
}

impl ToolTraceStore {
    async fn record_for_current_request(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        result: &str,
        is_error: bool,
    ) {
        let Ok(request_id) = ACTIVE_TELEGRAM_REQUEST_ID.try_with(|id| id.clone()) else {
            return;
        };
        let mut traces = self.traces.lock().await;
        traces.entry(request_id).or_default().push(ToolCallTrace {
            tool_name: tool_name.to_string(),
            arguments: arguments.clone(),
            result: result.to_string(),
            is_error,
        });
    }

    async fn take_for_request(&self, request_id: &str) -> Vec<ToolCallTrace> {
        let mut traces = self.traces.lock().await;
        traces.remove(request_id).unwrap_or_default()
    }
}

impl RuntimeModelControl {
    async fn persist_models(&self) {
        let model = self.primary_model.read().await.clone();
        let deep_model = self.deep_model.read().await.clone();
        let deep_escalation_enabled = *self.deep_escalation_enabled.read().await;
        self.state_store
            .set(
                MODEL_OVERRIDE_STATE_KEY,
                json!(RuntimeModelOverride {
                    model,
                    deep_model,
                    deep_escalation_enabled
                }),
            )
            .await;
    }
}

impl RouterModelClient {
    async fn current_endpoint(&self) -> String {
        self.endpoint.read().await.clone()
    }

    async fn set_endpoint(&self, endpoint: &str) -> Result<(), String> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err("endpoint cannot be empty".to_string());
        }
        if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
            return Err("endpoint must start with http:// or https://".to_string());
        }
        let provider_config = ProviderConfig::new(endpoint, self.api_key.clone());
        let router_config = RouterConfig::single("default", provider_config);
        let updated_router = Arc::new(ModelRouter::new(router_config));
        {
            let mut guard = self.router.write().await;
            *guard = updated_router;
        }
        {
            let mut guard = self.endpoint.write().await;
            *guard = endpoint.to_string();
        }
        Ok(())
    }
}


impl RuntimeAgentFactory {
    fn build_embedder(&self) -> Box<dyn EmbeddingProvider> {
        match &self.embed_url {
            Some(url) => Box::new(OpenAiEmbedder::new(
                url.clone(),
                self.embed_model.clone(),
                self.api_key.clone(),
            )),
            None => Box::new(MockEmbedder),
        }
    }

    fn build_plures_lm(&self) -> Arc<PluresLm> {
        Arc::new(PluresLm::new(
            Arc::clone(&self.store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
            self.build_embedder(),
            128_000,
        ))
    }

    fn build_agent_with_lm(&self, plures_lm: Arc<PluresLm>) -> Result<Arc<Agent>, String> {
        let memory = Arc::new(PluresMemory {
            plures_lm: Arc::clone(&plures_lm),
        });
        let orchestrator = Orchestrator::new(CerebellumConfig::default());

        // Attach BitNet classifier if a orchestrator model path is configured
        #[cfg(feature = "bitnet-native")]
        let orchestrator = if let Some(ref path) = self.cerebellum_model_path {
            match super::bitnet_classifier::BitNetClassifier::new(path) {
                Ok(backend) => {
                    let classifier = pares_agens_core::orchestrator::classifier::CerebellumClassifier::with_backend(
                        std::sync::Arc::new(backend),
                        vec![],
                    );
                    tracing::info!("orchestrator classifier enabled (BitNet)");
                    orchestrator.with_classifier(classifier)
                }
                Err(e) => {
                    tracing::warn!(
                        "BitNet classifier failed to load: {e}, falling back to heuristic"
                    );
                    let classifier = pares_agens_core::orchestrator::classifier::CerebellumClassifier::heuristic_only(vec![]);
                    orchestrator.with_classifier(classifier)
                }
            }
        } else {
            orchestrator
        };

        // Load .px procedures for orchestrator routing/classification
        let orchestrator = {
            // Try ~/.pares-radix/praxis/procedures/ first (production)
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            let px_dir = std::path::PathBuf::from(&home)
                .join(".pares-radix")
                .join("praxis")
                .join("procedures");
            let bridge = Arc::new(PxBridge::new(Arc::new(
                pares_agens_core::orchestrator::actions::CerebellumActionHandler::new_minimal(),
            )));
            let loaded = bridge.load_from_directory_sync(&px_dir);
            if loaded > 0 {
                tracing::info!(count = loaded, dir = %px_dir.display(), "px_bridge: loaded orchestrator procedures");
                orchestrator.with_px_bridge(bridge)
            } else {
                // Also try the repo-local praxis/procedures/ directory
                let local_dir = std::path::PathBuf::from("praxis/procedures");
                let loaded_local = bridge.load_from_directory_sync(&local_dir);
                if loaded_local > 0 {
                    tracing::info!(count = loaded_local, dir = %local_dir.display(), "px_bridge: loaded orchestrator procedures (local)");
                    orchestrator.with_px_bridge(bridge)
                } else {
                    tracing::debug!("px_bridge: no .px procedures found, using Rust fallback");
                    orchestrator
                }
            }
        };

        // Load dataflow procedures (queue-driven, no triggers)
        let orchestrator = {
            use pares_agens_core::orchestrator::dataflow_bridge::DataflowBridge;
            use pares_radix_praxis::dataflow::{ast_to_node, parse_px};

            let action_handler_for_df = Arc::new(
                pares_agens_core::orchestrator::actions::CerebellumActionHandler::new_minimal(),
            );

            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_default();
            let px_dir = std::path::PathBuf::from(&home)
                .join(".pares-radix")
                .join("praxis")
                .join("procedures");
            let local_dir = std::path::PathBuf::from("praxis/procedures");

            let mut df_bridge = DataflowBridge::new(Arc::new(
                pares_agens_core::orchestrator::dataflow_bridge::DataflowActionAdapter::new(
                    Arc::clone(&action_handler_for_df) as Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>,
                ),
            ));
            let mut df_count = 0usize;
            let mut px_parse_failures: Vec<(std::path::PathBuf, String)> = Vec::new();

            for dir in [&px_dir, &local_dir] {
                if !dir.exists() {
                    continue;
                }
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("px") {
                            continue;
                        }
                        if let Ok(source) = std::fs::read_to_string(&path) {
                            match parse_px(&source) {
                                Ok(doc) => {
                                for proc in doc.statements.iter().filter_map(|s| match s {
                                    pares_radix_praxis::px::Statement::DataflowProcedure(p) => {
                                        Some(p)
                                    }
                                    _ => None,
                                }) {
                                    let node = ast_to_node(proc);
                                    let name = node.name.clone();
                                    let rt = tokio::runtime::Handle::current();
                                    let result = tokio::task::block_in_place(|| {
                                        rt.block_on(df_bridge.register(node))
                                    });
                                    if let Err(e) = result {
                                        tracing::warn!(name = %name, error = %e, "dataflow: failed to register procedure");
                                    } else {
                                        df_count += 1;
                                    }
                                }
                                }
                                Err(e) => {
                                    tracing::error!(
                                        file = %path.display(),
                                        error = %e,
                                        "px_loader: FAILED to parse procedure file - this policy file is NOT active"
                                    );
                                    px_parse_failures.push((path.clone(), e.to_string()));
                                }
                            }
                        } else {
                            tracing::error!(
                                file = %path.display(),
                                "px_loader: FAILED to read procedure file - this policy file is NOT active"
                            );
                        }
                    }
                }
            }

            if !px_parse_failures.is_empty() {
                tracing::error!(
                    count = px_parse_failures.len(),
                    files = ?px_parse_failures.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>(),
                    "px_loader: {} .px procedure file(s) failed to parse and are NOT active",
                    px_parse_failures.len()
                );
            }

            if df_count > 0 {
                tracing::info!(count = df_count, "dataflow_bridge: loaded procedures");
                orchestrator.with_dataflow_bridge(Arc::new(df_bridge))
            } else {
                tracing::debug!("dataflow_bridge: no dataflow procedures found");
                orchestrator
            }
        };

        let system_prompt = build_system_prompt(self.system_prompt_path.clone())?;

        // Create default personality contract. Runtime seeding into PluresDB
        // happens in the async serve path.
        let personality =
            pares_agens_core::personality::PersonalityContract::default_contract(None);
        let delegation_broker = DelegationBroker::new(
            Arc::clone(&self.registry),
            Arc::clone(&self.model_client),
            Arc::clone(&self.tool_dispatcher),
        );
        let turn_store: Arc<dyn pares_agens_core::memory::store::MemoryStore> = self.store.clone();

        // Shared Chronos timeline: attached to BOTH the Orchestrator (so
        // autorecall emits real `recall_query` operations, ADR-0019 4.3)
        // and the Agent (tool execution auditing), so recall + tool events
        // land on the same timeline instance.
        let chronos = Arc::new(pares_radix_core::chronos::ChronosTimeline::with_jsonl_from_env(
            self.store.crdt_store_arc(),
        ));
        let orchestrator = orchestrator.with_chronos(Arc::clone(&chronos));

        let agent = Agent::with_cerebellum(memory, orchestrator, plures_lm)
                .with_model(
                    Arc::clone(&self.model_client),
                    Arc::clone(&self.tool_dispatcher),
                    system_prompt,
                )
                .with_deep_model(Arc::clone(&self.deep_model_client))
                .with_delegation(delegation_broker)
                .with_turn_store(turn_store)
                .with_personality(personality)
                .with_chronos(chronos);
        // Attach fast model if available
        let agent = if let Some(ref fast_client) = self.fast_model_client {
            agent.with_fast_model(Arc::clone(fast_client))
        } else {
            agent
        };
        Ok(Arc::new(agent))
    }

    fn build_agent(&self) -> Result<Arc<Agent>, String> {
        let plures_lm = self.build_plures_lm();
        self.build_agent_with_lm(plures_lm)
    }
}

#[async_trait]
impl TelegramModelControl for RuntimeModelControl {
    async fn current_models(&self) -> (String, String) {
        (
            self.primary_model.read().await.clone(),
            self.deep_model.read().await.clone(),
        )
    }

    async fn fast_model(&self) -> Option<String> {
        let f = self.fast_model.read().await.clone();
        if f.trim().is_empty() {
            None
        } else {
            Some(f)
        }
    }

    async fn last_route_tier(&self) -> Option<String> {
        let guard = self.agent_ref.read().await;
        guard
            .as_ref()
            .and_then(|a| a.last_route_tier())
            .map(|t| t.label().to_string())
    }

    async fn routing_mode(&self) -> String {
        "complexity-gated (context-size-gated)".to_string()
    }

    async fn available_models(&self) -> Vec<pares_agens_channels::telegram::DiscoveredModelInfo> {
        use pares_radix_core::auth::copilot::{classify_model_tier, ModelTier};
        let primary = self.primary_model.read().await.clone();
        let deep = self.deep_model.read().await.clone();
        let fast = self.fast_model.read().await.clone();
        let models = self.available_models.read().await;
        models
            .iter()
            .map(|m| {
                let selected_slot = if m.id == fast && !fast.is_empty() {
                    Some("fast")
                } else if m.id == primary {
                    Some("standard")
                } else if m.id == deep {
                    Some("deep")
                } else {
                    None
                };
                let tier = match classify_model_tier(&m.id) {
                    ModelTier::Fast => "Fast",
                    ModelTier::Standard => "Standard",
                    ModelTier::Premium => "Premium",
                }
                .to_string();
                pares_agens_channels::telegram::DiscoveredModelInfo {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    context_window: m.context_window(),
                    tier,
                    selected_slot,
                }
            })
            .collect()
    }

    async fn set_primary_model(&self, model: &str) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("model name cannot be empty".to_string());
        }
        let previous = {
            let mut guard = self.primary_model.write().await;
            let previous = guard.clone();
            *guard = model.to_string();
            previous
        };
        self.persist_models().await;
        tracing::info!(from_model = %previous, to_model = %model, "runtime primary model updated");
        Ok(())
    }

    async fn set_deep_model(&self, model: &str) -> Result<(), String> {
        let model = model.trim();
        if model.is_empty() {
            return Err("deep model name cannot be empty".to_string());
        }
        let previous = {
            let mut guard = self.deep_model.write().await;
            let previous = guard.clone();
            *guard = model.to_string();
            previous
        };
        self.persist_models().await;
        tracing::info!(from_model = %previous, to_model = %model, "runtime deep model updated");
        Ok(())
    }

    async fn deep_escalation_enabled(&self) -> bool {
        *self.deep_escalation_enabled.read().await
    }

    async fn set_deep_escalation_enabled(&self, enabled: bool) -> Result<(), String> {
        {
            let mut guard = self.deep_escalation_enabled.write().await;
            *guard = enabled;
        }
        self.persist_models().await;
        tracing::info!(enabled, "runtime deep model escalation updated");
        Ok(())
    }
}

impl RuntimeConfigControl {
    async fn persist_config(&self) {
        let model = self.model_control.primary_model.read().await.clone();
        let endpoint = self.primary_client.current_endpoint().await;
        let log_level = self.log_level.read().await.clone();
        self.state_store
            .set(
                RUNTIME_CONFIG_OVERRIDE_STATE_KEY,
                json!(RuntimeConfigOverride {
                    model,
                    endpoint,
                    log_level
                }),
            )
            .await;
    }
}

#[async_trait]
impl TelegramConfigControl for RuntimeConfigControl {
    async fn current_config(&self) -> TelegramRuntimeConfig {
        TelegramRuntimeConfig {
            model: self.model_control.primary_model.read().await.clone(),
            endpoint: self.primary_client.current_endpoint().await,
            log_level: self.log_level.read().await.clone(),
        }
    }

    async fn set_model(&self, model: &str) -> Result<(), String> {
        self.model_control.set_primary_model(model).await?;
        self.persist_config().await;
        Ok(())
    }

    async fn set_endpoint(&self, endpoint: &str) -> Result<(), String> {
        self.primary_client.set_endpoint(endpoint).await?;
        self.persist_config().await;
        Ok(())
    }

    async fn set_log_level(&self, log_level: &str) -> Result<(), String> {
        let normalized = apply_runtime_log_level(&self.log_filter_handle, log_level)?;
        {
            let mut guard = self.log_level.write().await;
            *guard = normalized.clone();
        }
        self.persist_config().await;
        tracing::info!(log_level = %normalized, "runtime log level updated");
        Ok(())
    }
}

#[async_trait]
impl TelegramRuntimeControl for RuntimeResetControl {
    async fn reset_runtime(&self) -> Result<(), String> {
        tracing::info!("telegram /reset requested; rebuilding runtime state");
        let new_agent = self.factory.build_agent()?;
        {
            let mut guard = self.agent.write().await;
            *guard = new_agent;
        }
        tracing::info!("telegram /reset completed successfully");
        Ok(())
    }
}

#[async_trait]
impl TelegramPersonalityControl for RuntimePersonalityControl {
    async fn show(&self, channel: Option<&str>) -> String {
        use pares_agens_core::personality::{PersonalityContract, PERSONALITY_STATE_KEY};
        match self.state_store.get(PERSONALITY_STATE_KEY).await {
            Some(v) => match serde_json::from_value::<PersonalityContract>(v) {
                Ok(p) => p.display_summary(channel),
                Err(e) => format!("Failed to parse personality: {e}"),
            },
            None => "No personality contract configured.".to_string(),
        }
    }

    async fn set_tone(&self, tone: &str) -> Result<(), String> {
        use pares_agens_core::personality::{PersonalityContract, PERSONALITY_STATE_KEY};
        let mut contract = match self.state_store.get(PERSONALITY_STATE_KEY).await {
            Some(v) => serde_json::from_value::<PersonalityContract>(v)
                .map_err(|e| format!("parse error: {e}"))?,
            None => PersonalityContract::default_contract(None),
        };
        contract.tone = tone.to_string();
        let value = serde_json::to_value(&contract).map_err(|e| format!("serialize: {e}"))?;
        self.state_store.set(PERSONALITY_STATE_KEY, value).await;
        // TODO: rebuild agent system prompt live
        Ok(())
    }

    async fn add_rule(&self, rule_text: &str) -> Result<String, String> {
        use pares_agens_core::personality::{
            BehaviorRule, PersonalityContract, PERSONALITY_STATE_KEY,
        };
        let mut contract = match self.state_store.get(PERSONALITY_STATE_KEY).await {
            Some(v) => serde_json::from_value::<PersonalityContract>(v)
                .map_err(|e| format!("parse error: {e}"))?,
            None => PersonalityContract::default_contract(None),
        };
        let id = format!("custom-{}", uuid::Uuid::new_v4().as_simple());
        contract.upsert_rule(BehaviorRule {
            id: id.clone(),
            category: "communication".into(),
            rule: rule_text.to_string(),
            priority: 5,
            enforced: false,
        });
        let value = serde_json::to_value(&contract).map_err(|e| format!("serialize: {e}"))?;
        self.state_store.set(PERSONALITY_STATE_KEY, value).await;
        Ok(id)
    }

    async fn remove_rule(&self, id: &str) -> Result<(), String> {
        use pares_agens_core::personality::{PersonalityContract, PERSONALITY_STATE_KEY};
        let mut contract = match self.state_store.get(PERSONALITY_STATE_KEY).await {
            Some(v) => serde_json::from_value::<PersonalityContract>(v)
                .map_err(|e| format!("parse error: {e}"))?,
            None => return Err("No personality contract configured.".to_string()),
        };
        if !contract.remove_rule(id) {
            return Err(format!("Rule '{id}' not found."));
        }
        let value = serde_json::to_value(&contract).map_err(|e| format!("serialize: {e}"))?;
        self.state_store.set(PERSONALITY_STATE_KEY, value).await;
        Ok(())
    }

    async fn list_documents(&self) -> String {
        use pares_agens_core::personality::{get_all_documents, PERSONALITY_DOC_TYPES};
        let docs = get_all_documents(self.state_store.as_ref()).await;
        if docs.is_empty() {
            return "No personality documents stored.".to_string();
        }
        let mut lines = vec!["Personality documents:".to_string()];
        for doc_type in PERSONALITY_DOC_TYPES {
            if let Some(doc) = docs.iter().find(|d| d.doc_type == *doc_type) {
                lines.push(format!("• {} — {} chars", doc.doc_type, doc.content.len()));
            } else {
                lines.push(format!("• {} — (not set)", doc_type));
            }
        }
        lines.join("\n")
    }

    async fn get_document(&self, doc_type: &str) -> String {
        use pares_agens_core::personality::get_document;
        match get_document(self.state_store.as_ref(), doc_type).await {
            Some(doc) => format!(
                "## {} (updated: {})\n{}",
                doc.doc_type, doc.updated_at, doc.content
            ),
            None => format!("No '{doc_type}' document found."),
        }
    }

    async fn set_document(&self, doc_type: &str, content: &str) -> Result<(), String> {
        use pares_agens_core::personality::{
            format_documents_for_prompt, get_all_documents, store_document, PERSONALITY_DOC_TYPES,
        };
        if !PERSONALITY_DOC_TYPES.contains(&doc_type) {
            return Err(format!(
                "Unknown document type '{}'. Valid types: {:?}",
                doc_type, PERSONALITY_DOC_TYPES
            ));
        }
        store_document(self.state_store.as_ref(), doc_type, content).await;
        // Update agent cache
        let docs = get_all_documents(self.state_store.as_ref()).await;
        let formatted = format_documents_for_prompt(&docs);
        self.agent
            .read()
            .await
            .set_personality_documents(Some(formatted));
        Ok(())
    }
}

#[async_trait]
impl ModelClient for RouterModelClient {
    async fn complete(
        &self,
        messages: &[CoreChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<pares_radix_core::model::ModelCompletion, ModelClientError> {
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
                ChatMessage {
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

        let model = self.model.read().await.clone();
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

        let router = self.router.read().await.clone();
        let response = router
            .chat(&request)
            .await
            .map_err(|error| {
                ModelClientError::Transport(TransportFailure::message(error.to_string()))
            })?;

        let choice = response
            .choices
            .first()
            .ok_or_else(|| {
                ModelClientError::Transport(TransportFailure::message("model returned no choices"))
            })?;

        let tool_calls = choice
            .message
            .tool_calls
            .clone()
            .unwrap_or_default()
            .into_iter()
            .map(|call| pares_radix_core::model::ToolCall {
                id: call.id,
                name: call.function.name,
                arguments: serde_json::from_str(&call.function.arguments)
                    .unwrap_or(serde_json::Value::String(call.function.arguments)),
            })
            .collect();

        let logprobs = choice
            .logprobs
            .as_ref()
            .and_then(|lp| lp.content.as_ref())
            .map(|tokens| tokens.iter().filter_map(|t| t.logprob).collect::<Vec<_>>())
            .filter(|vals| !vals.is_empty());

        Ok(pares_radix_core::model::ModelCompletion {
            content: choice.message.content.clone(),
            tool_calls,
            logprobs,
            model: Some(response.model),
        })
    }

    async fn complete_stream(
        &self,
        messages: &[CoreChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
        tx: pares_radix_core::model::StreamSender,
    ) -> Result<pares_radix_core::model::ModelCompletion, ModelClientError> {
        use futures_util::StreamExt as _;
        use pares_radix_core::model::StreamDelta;

        let converted_messages: Vec<pares_agens_models::types::ChatMessage> = messages
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => pares_agens_models::types::Role::System,
                    "user" => pares_agens_models::types::Role::User,
                    "assistant" => pares_agens_models::types::Role::Assistant,
                    "tool" => pares_agens_models::types::Role::Tool,
                    _ => pares_agens_models::types::Role::User,
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

        let model = self.model.read().await.clone();
        let mut request =
            pares_agens_models::types::ChatCompletionRequest::new(&model, converted_messages);
        if !tools.is_empty() {
            request.tools = Some(
                tools
                    .iter()
                    .map(|tool| {
                        pares_agens_models::types::Tool::function(
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

        let router = self.router.read().await.clone();
        let mut stream = router
            .chat_stream(&request)
            .await
            .map_err(|error| {
                ModelClientError::Transport(TransportFailure::message(error.to_string()))
            })?;

        let mut full_content = String::new();
        let mut tool_calls_map: std::collections::HashMap<usize, (String, String, String)> =
            std::collections::HashMap::new();
        let mut response_model = String::new();

        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(chunk) => {
                    if response_model.is_empty() {
                        response_model = chunk.model.clone();
                    }
                    for choice in &chunk.choices {
                        if let Some(ref content) = choice.delta.content {
                            if !content.is_empty() {
                                full_content.push_str(content);
                                let _ = tx.send(StreamDelta::Content(content.clone()));
                            }
                        }
                        if let Some(ref tc_deltas) = choice.delta.tool_calls {
                            for tc in tc_deltas {
                                let idx = tc.index.unwrap_or(0) as usize;
                                let entry = tool_calls_map
                                    .entry(idx)
                                    .or_insert_with(|| (String::new(), String::new(), String::new()));
                                if !tc.id.is_empty() {
                                    entry.0 = tc.id.clone();
                                    entry.1 = tc.function.name.clone();
                                    let _ = tx.send(StreamDelta::ToolCallStart {
                                        index: idx,
                                        id: tc.id.clone(),
                                        name: tc.function.name.clone(),
                                    });
                                }
                                if !tc.function.arguments.is_empty() {
                                    entry.2.push_str(&tc.function.arguments);
                                    let _ = tx.send(StreamDelta::ToolCallDelta {
                                        index: idx,
                                        arguments: tc.function.arguments.clone(),
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

        let tool_calls: Vec<pares_radix_core::model::ToolCall> = {
            let mut calls: Vec<(usize, pares_radix_core::model::ToolCall)> = tool_calls_map
                .into_iter()
                .map(|(idx, (id, name, args))| {
                    (
                        idx,
                        pares_radix_core::model::ToolCall {
                            id,
                            name,
                            arguments: serde_json::from_str(&args)
                                .unwrap_or(serde_json::Value::String(args)),
                        },
                    )
                })
                .collect();
            calls.sort_by_key(|(idx, _)| *idx);
            calls.into_iter().map(|(_, tc)| tc).collect()
        };

        let content = if full_content.is_empty() {
            None
        } else {
            Some(full_content)
        };

        Ok(pares_radix_core::model::ModelCompletion {
            content,
            tool_calls,
            logprobs: None,
            model: Some(response_model),
        })
    }
}

#[async_trait]
impl ModelClient for ToggleableModelClient {
    async fn complete(
        &self,
        messages: &[CoreChatMessage],
        tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<pares_radix_core::model::ModelCompletion, ModelClientError> {
        if !*self.enabled.read().await {
            return Err(ModelClientError::Transport(TransportFailure::message(
                "deep model escalation is disabled",
            )));
        }
        self.inner.complete(messages, tools, options).await
    }

    fn context_window(&self) -> Option<u64> {
        self.inner.context_window()
    }

    fn model_id(&self) -> Option<String> {
        self.inner.model_id()
    }
}

/// Thin I/O boundary for task-graph operations that the platform task registry
/// does not yet expose. PX owns scheduling; this type only reads or writes the
/// durable `TaskManager` graph and protects its completion invariant.
struct TaskGraphToolDispatcher {
    inner: Arc<dyn ToolDispatcher>,
    task_manager: Arc<TaskManager>,
}

impl TaskGraphToolDispatcher {
    const CREATE_SUBTASK: &'static str = "task_create_subtask";
    const LIST_EVALUABLE_GRAPH: &'static str = "task_list_evaluable_graph";

    fn new(inner: Arc<dyn ToolDispatcher>, task_manager: Arc<TaskManager>) -> Self {
        Self {
            inner,
            task_manager,
        }
    }

    fn definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: Self::CREATE_SUBTASK.into(),
                description: "Create a durable child task beneath an active parent task. Use this to decompose independently verifiable work; the parent cannot complete until every child is terminal.".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "parent_task_id": {"type": "string", "description": "Parent task ID; an unambiguous leading ID prefix is accepted."},
                        "description": {"type": "string", "description": "Concrete child outcome to achieve."},
                        "completion_conditions": {"type": "array", "items": {"type": "string"}, "description": "Optional conditions required to complete the child."}
                    },
                    "required": ["parent_task_id", "description"]
                }),
            },
            ToolDefinition {
                name: Self::LIST_EVALUABLE_GRAPH.into(),
                description: "Return durable evaluable task records with their parent and child edges. Used by PX autonomous scheduling.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            },
        ]
    }

    fn resolve_active_task_id(&self, task_id: &str) -> Option<String> {
        if self
            .task_manager
            .get_task(task_id)
            .is_some_and(|task| !task.is_terminal())
        {
            return Some(task_id.to_string());
        }

        let matches: Vec<_> = self
            .task_manager
            .evaluable_tasks()
            .into_iter()
            .filter(|task| task.id.starts_with(task_id))
            .collect();
        (matches.len() == 1).then(|| matches[0].id.clone())
    }

    fn conditions(arguments: &serde_json::Value) -> Vec<CompletionCondition> {
        arguments
            .get("completion_conditions")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(|description| CompletionCondition {
                        description: description.to_string(),
                        condition_type: ConditionType::ModelEvaluation(description.to_string()),
                        satisfied: false,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn create_subtask(&self, arguments: serde_json::Value) -> String {
        let parent_task_id = match arguments.get("parent_task_id").and_then(serde_json::Value::as_str) {
            Some(task_id) => task_id,
            None => return serde_json::json!({"status": "error", "message": "'parent_task_id' is required"}).to_string(),
        };
        let description = match arguments.get("description").and_then(serde_json::Value::as_str) {
            Some(description) if !description.trim().is_empty() => description,
            _ => return serde_json::json!({"status": "error", "message": "a non-empty 'description' is required"}).to_string(),
        };
        let parent_task_id = match self.resolve_active_task_id(parent_task_id) {
            Some(task_id) => task_id,
            None => return serde_json::json!({"status": "error", "message": "parent task not found or terminal"}).to_string(),
        };

        match self
            .task_manager
            .create_subtask(&parent_task_id, description, Self::conditions(&arguments))
        {
            Some(task) => serde_json::json!({
                "status": "created",
                "task_id": task.id,
                "parent_task_id": parent_task_id,
                "description": task.description,
                "priority": task.priority,
            })
            .to_string(),
            None => serde_json::json!({"status": "error", "message": "parent task not found"}).to_string(),
        }
    }

    fn evaluable_graph(&self) -> String {
        let tasks = self
            .task_manager
            .evaluable_tasks()
            .into_iter()
            .map(|task| {
                serde_json::json!({
                    "id": task.id,
                    "description": task.description,
                    "priority": task.priority,
                    "created_at": task.created_at,
                    "last_evaluated_at": task.last_evaluated_at,
                    "attempts": task.attempts,
                    "subtasks": task.subtasks,
                    "parent_task": task.parent_task,
                    "conditions": task.completion_conditions.into_iter().map(|condition| serde_json::json!({
                        "description": condition.description,
                        "satisfied": condition.satisfied,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>();
        serde_json::Value::Array(tasks).to_string()
    }

    fn completion_is_blocked(&self, arguments: &serde_json::Value) -> Option<String> {
        let requested_id = arguments.get("task_id").and_then(serde_json::Value::as_str)?;
        let task_id = self.resolve_active_task_id(requested_id)?;
        let task = self.task_manager.get_task(&task_id)?;
        let outstanding = task
            .subtasks
            .iter()
            .filter(|child_id| {
                self.task_manager
                    .get_task(child_id)
                    .is_some_and(|child| !child.is_terminal())
            })
            .cloned()
            .collect::<Vec<_>>();
        (!outstanding.is_empty()).then(|| {
            serde_json::json!({
                "status": "error",
                "message": "parent task cannot complete while child tasks remain non-terminal",
                "task_id": task_id,
                "outstanding_subtask_ids": outstanding,
            })
            .to_string()
        })
    }
}

#[async_trait]
impl ToolDispatcher for TaskGraphToolDispatcher {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        let mut tools = self.inner.available_tools().await;
        tools.extend(Self::definitions());
        tools
    }

    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        match name {
            Self::CREATE_SUBTASK => self.create_subtask(arguments).await,
            Self::LIST_EVALUABLE_GRAPH => self.evaluable_graph(),
            "task_complete" => match self.completion_is_blocked(&arguments) {
                Some(result) => result,
                None => self.inner.call_tool(name, arguments).await,
            },
            _ => self.inner.call_tool(name, arguments).await,
        }
    }
}

// SpineToolDispatcher removed — ServeSpine now uses SpineProcedureDispatcher
// backed by a full ProcedureRegistry (see Commands::ServeSpine handler).

struct ProcedureToolDispatcher {
    registry: Arc<ProcedureRegistry>,
    trace_store: ToolTraceStore,
    governor: Arc<ToolGovernor>,
    plugin_runtime: Option<Arc<PluginRuntime>>,
    /// Shared block-and-await approval registry (#472). Cloned into the Telegram
    /// adapter so an Allow/Deny press can resolve a pending tool approval by token.
    approval_registry: Arc<pares_radix_core::approval::ApprovalRegistry>,
}

#[async_trait]
impl ToolDispatcher for ProcedureToolDispatcher {
    async fn available_tools(&self) -> Vec<ToolDefinition> {
        let mut tools = tool_definitions();
        if let Some(ref runtime) = self.plugin_runtime {
            tools.extend(runtime.tool_definitions().await);
        }
        tools
    }

    async fn call_tool(&self, name: &str, arguments: serde_json::Value) -> String {
        let args_for_trace = arguments.clone();
        let args_str = arguments.to_string();

        // --- Governance pre-execution check ---
        match self.governor.check(name, &args_str) {
            GovernanceVerdict::Blocked { pattern } => {
                let result = format!(
                    "Command blocked by policy: matched blocked pattern \"{}\".",
                    pattern
                );
                self.trace_store
                    .record_for_current_request(name, &args_for_trace, &result, true)
                    .await;
                return result;
            }
            GovernanceVerdict::AllowWithApprovalWarning => {
                // #472 block-and-await seam. Register a pending approval so the
                // token exists in the shared registry that the Telegram adapter
                // resolves against; this closes the resolve half of the loop
                // (adapter -> ApprovalRegistry::resolve -> woken waiter).
                //
                // NOTE (honest scope): full mid-tool-call blocking (awaiting
                // `pending.wait()` here to gate execution) requires an
                // out-of-band path to surface the Allow/Deny card to the user
                // while this call is suspended. The current adapter has no
                // dispatcher->channel outbound handle for a mid-turn card
                // (see runtime event-spine "stack-local for now" note), so we do
                // NOT block here yet — blocking without a visible card would
                // deadlock the turn. The registry + resolve routing are wired
                // and unit-tested end-to-end; enabling the await is a follow-up
                // once the outbound-card seam lands.
                let (req, _pending) = self
                    .approval_registry
                    .register(name, &args_str)
                    .await;
                let pending_count = self.approval_registry.pending_count().await;
                tracing::info!(
                    tool = name,
                    approval_token = %req.token,
                    pending = pending_count,
                    "registered pending tool approval (resolve seam live; block-and-await gated on outbound-card seam)"
                );
                // Do not leak the waiter: resolve it Allow immediately so the
                // map stays clean until real blocking is enabled. This preserves
                // today's log-and-proceed behavior with zero regression.
                let _ = self
                    .approval_registry
                    .resolve(&req.token, pares_radix_core::approval::ApprovalDecision::Allow)
                    .await;
            }
            GovernanceVerdict::Allow => {}
        }

        let handler = match self.registry.matching(name).next() {
            Some(h) => h,
            None => {
                let result = format!("no procedure registered for {name}");
                self.trace_store
                    .record_for_current_request(name, &args_for_trace, &result, true)
                    .await;
                return result;
            }
        };

        let event = Event::Message {
            id: Uuid::new_v4().to_string(),
            channel: "tool".into(),
            sender: "model".into(),
            content: arguments.to_string(),
        };

        // --- Governance timeout wrapper ---
        let policy = self.governor.policy_for(name);
        let timeout_duration = policy.timeout();
        let start = Instant::now();

        let execution = handler.execute(&event);
        let results = match tokio::time::timeout(timeout_duration, execution).await {
            Ok(results) => results,
            Err(_) => {
                let output = format!(
                    "Tool '{}' timed out after {:.1}s (limit: {:.1}s)",
                    name,
                    start.elapsed().as_secs_f64(),
                    timeout_duration.as_secs_f64(),
                );
                tracing::warn!(tool = name, "{}", output);
                self.trace_store
                    .record_for_current_request(name, &args_for_trace, &output, true)
                    .await;
                return output;
            }
        };

        let elapsed = start.elapsed();
        tracing::debug!(
            tool = name,
            elapsed_ms = elapsed.as_millis() as u64,
            "tool execution completed"
        );
        for result in results {
            if let Event::ToolResult {
                content, is_error, ..
            } = result
            {
                if is_error {
                    let output = format!("tool error: {content}");
                    self.trace_store
                        .record_for_current_request(name, &args_for_trace, &output, true)
                        .await;
                    return output;
                }
                self.trace_store
                    .record_for_current_request(name, &args_for_trace, &content, false)
                    .await;
                return content;
            }
        }

        let output = format!("procedure {name} returned no tool result");
        self.trace_store
            .record_for_current_request(name, &args_for_trace, &output, true)
            .await;
        output
    }
}


struct PluresMemory {
    plures_lm: Arc<PluresLm>,
}

#[async_trait]
impl Memory for PluresMemory {
    async fn capture(&self, content: &str) -> Result<(), String> {
        let exchange = Exchange {
            user: content.to_string(),
            assistant: String::new(),
        };
        self.plures_lm
            .capture(&exchange)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, String> {
        let entries = self
            .plures_lm
            .recall(query, 5, &[])
            .await
            .map_err(|e| e.to_string())?;
        Ok(entries.into_iter().map(|e| e.content).collect())
    }
}

struct ReadFileProcedure;
struct WriteFileProcedure;
struct RunCommandProcedure {
    executor: Arc<ShellExecutor>,
}

struct ProcessManageProcedure {
    executor: Arc<ShellExecutor>,
}
struct EditFileProcedure;
struct ListDirectoryProcedure;
struct WebFetchProcedure;
struct WebSearchProcedure {
    brave_api_key: Option<String>,
    base_url: String,
}

impl WebSearchProcedure {
    const DEFAULT_BASE_URL: &'static str = "https://api.search.brave.com/res/v1/web/search";

    fn new(brave_api_key: Option<String>) -> Self {
        Self {
            brave_api_key,
            base_url: Self::DEFAULT_BASE_URL.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base_url(brave_api_key: Option<String>, base_url: String) -> Self {
        Self {
            brave_api_key,
            base_url,
        }
    }
}
struct MemorySearchProcedure {
    plures_lm: Arc<PluresLm>,
}
struct MemoryStoreProcedure {
    plures_lm: Arc<PluresLm>,
}

struct CronListProcedure {
    scheduler: Arc<pares_agens_agenda::scheduler::Scheduler>,
}
struct CronAddProcedure {
    scheduler: Arc<pares_agens_agenda::scheduler::Scheduler>,
}
struct CronRemoveProcedure {
    scheduler: Arc<pares_agens_agenda::scheduler::Scheduler>,
}
struct CronToggleProcedure {
    scheduler: Arc<pares_agens_agenda::scheduler::Scheduler>,
}

/// Exposes the status of loaded umbra-evolved shadow candidates via the
/// procedure registry (`shadow_status` tool). This replaces the former inert
/// load-and-discard pattern, retaining the loaded `ShadowProcedures` in shared
/// state so that operators can inspect loaded candidates and their readiness
/// for eventual arena evaluation.
///
/// Phase A of the pares-umbra integration (issue #677): the shadow holder is
/// now retained and queryable. Full arena wiring (fitness scoring, promotion)
/// requires adding the `umbra-shadow` crate as a dependency once license
/// compatibility is confirmed.
struct ShadowStatusProcedure {
    shadow: Arc<pares_radix_core::spine::shadow::ShadowProcedures>,
}

struct ParesManusToolProcedure {
    tool_name: &'static str,
    manus_ws_url: Arc<String>,
}

impl ParesManusToolProcedure {
    fn new(tool_name: &'static str, manus_ws_url: Arc<String>) -> Self {
        Self {
            tool_name,
            manus_ws_url,
        }
    }
}

#[async_trait]
impl Procedure for ReadFileProcedure {
    fn name(&self) -> &str {
        "read_file"
    }

    fn handles(&self) -> &str {
        "read_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("path").and_then(|v| v.as_str()) {
                        Some(path) => tokio::fs::read_to_string(path)
                            .await
                            .map_err(|e| e.to_string()),
                        None => Err("missing 'path'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "read_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WriteFileProcedure {
    fn name(&self) -> &str {
        "write_file"
    }

    fn handles(&self) -> &str {
        "write_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let path = args.get("path").and_then(|v| v.as_str());
                        let body = args.get("content").and_then(|v| v.as_str());
                        match (path, body) {
                            (Some(path), Some(body)) => tokio::fs::write(path, body)
                                .await
                                .map_err(|e| e.to_string())
                                .map(|_| "ok".to_string()),
                            _ => Err("missing 'path' or 'content'".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "write_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for RunCommandProcedure {
    fn name(&self) -> &str {
        "run_command"
    }

    fn handles(&self) -> &str {
        "run_command"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let command = match args.get("command").and_then(|v| v.as_str()) {
                            Some(c) => c.to_string(),
                            None => {
                                return vec![Event::ToolResult {
                                    tool_call_id: id.clone(),
                                    tool_name: "run_command".into(),
                                    content: "missing 'command' argument".into(),
                                    is_error: true,
                                }]
                            }
                        };

                        let req = ExecRequest {
                            command,
                            workdir: args
                                .get("workdir")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                            env: args
                                .get("env")
                                .and_then(|v| {
                                    serde_json::from_value::<HashMap<String, String>>(v.clone())
                                        .ok()
                                })
                                .unwrap_or_default(),
                            timeout_secs: args.get("timeout").and_then(|v| v.as_u64()),
                            background: args
                                .get("background")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false),
                            pty: args.get("pty").and_then(|v| v.as_bool()).unwrap_or(false),
                            yield_ms: args.get("yieldMs").and_then(|v| v.as_u64()),
                        };

                        let exec_result = self.executor.exec(req).await;

                        // Format output similar to OpenClaw's exec tool
                        let output = if let Some(session_id) = &exec_result.session_id {
                            if exec_result.still_running {
                                format!(
                                    "Command still running (session {session_id}, pid {}).\nInitial output:\n{}{}\nUse process tool to poll/write/kill.",
                                    exec_result.exit_code.map(|c| c.to_string()).unwrap_or("?".into()),
                                    exec_result.stdout,
                                    if exec_result.stderr.is_empty() { String::new() } else { format!("\nstderr:\n{}", exec_result.stderr) }
                                )
                            } else {
                                format!(
                                    "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                                    exec_result
                                        .exit_code
                                        .map(|c| c.to_string())
                                        .unwrap_or("signal".into()),
                                    exec_result.stdout,
                                    exec_result.stderr
                                )
                            }
                        } else if exec_result.timed_out {
                            format!("Command timed out and was killed.\nPartial stdout:\n{}\nPartial stderr:\n{}",
                                exec_result.stdout, exec_result.stderr)
                        } else {
                            format!(
                                "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                                exec_result
                                    .exit_code
                                    .map(|c| c.to_string())
                                    .unwrap_or("signal".into()),
                                exec_result.stdout,
                                exec_result.stderr
                            )
                        };

                        Ok(output)
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "run_command".into(),
                    content: result.unwrap_or_else(|e| e),
                    is_error: false,
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for ProcessManageProcedure {
    fn name(&self) -> &str {
        "process"
    }

    fn handles(&self) -> &str {
        "process"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let args = match parse_tool_args(content) {
                    Ok(a) => a,
                    Err(e) => {
                        return vec![Event::ToolResult {
                            tool_call_id: id.clone(),
                            tool_name: "process".into(),
                            content: e,
                            is_error: true,
                        }]
                    }
                };

                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
                let session_id = args.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");

                let output = match action {
                    "list" => {
                        let sessions = self.executor.list().await;
                        if sessions.is_empty() {
                            "No active sessions.".to_string()
                        } else {
                            sessions
                                .iter()
                                .map(|s| {
                                    format!(
                                        "{} | {} | {} | exit={:?} | {}s",
                                        s.id,
                                        if s.running { "running" } else { "exited" },
                                        s.command.chars().take(60).collect::<String>(),
                                        s.exit_code,
                                        s.elapsed_secs
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    "poll" => {
                        let timeout_ms = args.get("timeout").and_then(|v| v.as_u64());
                        match self.executor.poll(session_id, timeout_ms).await {
                            Some(pr) => {
                                let status = if pr.running { "running" } else { "exited" };
                                format!(
                                    "Session {}: {}\nexit_code: {:?}\nnew output:\n{}",
                                    pr.session_id, status, pr.exit_code, pr.new_output
                                )
                            }
                            None => format!("Session '{session_id}' not found."),
                        }
                    }
                    "log" => {
                        let offset = args
                            .get("offset")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize);
                        let limit = args
                            .get("limit")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as usize);
                        match self.executor.log(session_id, offset, limit).await {
                            Some(log) => log,
                            None => format!("Session '{session_id}' not found."),
                        }
                    }
                    "write" => {
                        let data = args.get("data").and_then(|v| v.as_str()).unwrap_or("");
                        match self.executor.write_stdin(session_id, data).await {
                            Ok(()) => "Written successfully.".to_string(),
                            Err(e) => format!("Write failed: {e}"),
                        }
                    }
                    "kill" => match self.executor.kill(session_id).await {
                        Ok(()) => format!("Session '{session_id}' killed."),
                        Err(e) => format!("Kill failed: {e}"),
                    },
                    other => {
                        format!("Unknown process action: '{other}'. Use list/poll/log/write/kill.")
                    }
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "process".into(),
                    content: output,
                    is_error: false,
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for EditFileProcedure {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn handles(&self) -> &str {
        "edit_file"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let path = args.get("path").and_then(|v| v.as_str());
                        let old_text = args.get("old_text").and_then(|v| v.as_str());
                        let new_text = args.get("new_text").and_then(|v| v.as_str());
                        match (path, old_text, new_text) {
                            (Some(path), Some(old_text), Some(new_text)) => {
                                let body = tokio::fs::read_to_string(path)
                                    .await
                                    .map_err(|e| e.to_string());
                                match body {
                                    Ok(mut body) => {
                                        if let Some(idx) = body.find(old_text) {
                                            body.replace_range(idx..idx + old_text.len(), new_text);
                                            tokio::fs::write(path, body)
                                                .await
                                                .map_err(|e| e.to_string())
                                                .map(|_| "ok".to_string())
                                        } else {
                                            Err("old_text not found".into())
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            _ => Err("missing 'path', 'old_text', or 'new_text'".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "edit_file".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for ListDirectoryProcedure {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn handles(&self) -> &str {
        "list_directory"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("path").and_then(|v| v.as_str()) {
                        Some(path) => {
                            let entries =
                                tokio::fs::read_dir(path).await.map_err(|e| e.to_string());
                            match entries {
                                Ok(mut entries) => {
                                    let mut names = Vec::new();
                                    let mut error: Option<String> = None;
                                    loop {
                                        match entries.next_entry().await {
                                            Ok(Some(entry)) => {
                                                if let Some(name) = entry.file_name().to_str() {
                                                    names.push(name.to_string());
                                                }
                                            }
                                            Ok(None) => break,
                                            Err(e) => {
                                                error = Some(e.to_string());
                                                break;
                                            }
                                        }
                                    }
                                    if let Some(error) = error {
                                        Err(error)
                                    } else {
                                        Ok(names.join("\n"))
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'path'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "list_directory".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WebFetchProcedure {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn handles(&self) -> &str {
        "web_fetch"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match args.get("url").and_then(|v| v.as_str()) {
                        Some(url) => {
                            let max_chars =
                                args.get("max_chars")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(30_000) as usize;
                            let extract_mode = args
                                .get("extract_mode")
                                .and_then(|v| v.as_str())
                                .unwrap_or("markdown");
                            let client = reqwest::Client::builder()
                                .user_agent("Mozilla/5.0 (compatible; pares-radix/0.1; +https://github.com/plures/pares-radix)")
                                .timeout(std::time::Duration::from_secs(15))
                                .build()
                                .unwrap_or_else(|_| reqwest::Client::new());
                            let response = client.get(url).send().await.map_err(|e| e.to_string());
                            match response {
                                Ok(response) => {
                                    let content_type = response
                                        .headers()
                                        .get("content-type")
                                        .and_then(|v| v.to_str().ok())
                                        .unwrap_or("")
                                        .to_string();
                                    match response.text().await.map_err(|e| e.to_string()) {
                                        Ok(body) => {
                                            let extracted = if content_type.contains("text/html")
                                                || body.trim_start().starts_with('<')
                                            {
                                                // HTML → readable text extraction
                                                let width = match extract_mode {
                                                    "text" => 80,
                                                    _ => 120,
                                                };
                                                html2text::from_read(body.as_bytes(), width)
                                                    .unwrap_or(body)
                                            } else {
                                                body
                                            };
                                            let truncated = if extracted.len() > max_chars {
                                                let mut s: String =
                                                    extracted.chars().take(max_chars).collect();
                                                s.push_str("\n\n[...truncated]");
                                                s
                                            } else {
                                                extracted
                                            };
                                            Ok(truncated)
                                        }
                                        Err(e) => Err(e),
                                    }
                                }
                                Err(e) => Err(e),
                            }
                        }
                        None => Err("missing 'url'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "web_fetch".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for WebSearchProcedure {
    fn name(&self) -> &str {
        "web_search"
    }

    fn handles(&self) -> &str {
        "web_search"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let query = args.get("query").and_then(|v| v.as_str());
                        let count = args.get("count").and_then(|v| v.as_u64()).unwrap_or(5);
                        let api_key = self.brave_api_key.clone();
                        match (query, api_key) {
                            (Some(query), Some(api_key)) => {
                                let mut headers = HeaderMap::new();
                                let token =
                                    HeaderValue::from_str(&api_key).map_err(|e| e.to_string());
                                match token {
                                    Ok(token) => {
                                        headers.insert("X-Subscription-Token", token);
                                        let client = reqwest::Client::new();
                                        let response = client
                                            .get(&self.base_url)
                                            .headers(headers)
                                            .query(&[("q", query), ("count", &count.to_string())])
                                            .send()
                                            .await
                                            .map_err(|e| e.to_string());
                                        match response {
                                            Ok(response) => {
                                                let value: Result<serde_json::Value, String> =
                                                    response
                                                        .json()
                                                        .await
                                                        .map_err(|e| e.to_string());
                                                match value {
                                                    Ok(value) => {
                                                        let results = value
                                                            .get("web")
                                                            .and_then(|v| v.get("results"))
                                                            .and_then(|v| v.as_array())
                                                            .map(|items| {
                                                                items
                                                                    .iter()
                                                                    .filter_map(|item| {
                                                                        Some(serde_json::json!({
                                                                            "title": item.get("title")?.as_str()?,
                                                                            "url": item.get("url")?.as_str()?,
                                                                            "description": item
                                                                                .get("description")
                                                                                .and_then(|d| d.as_str())
                                                                                .unwrap_or("")
                                                                        }))
                                                                    })
                                                                    .collect::<Vec<_>>()
                                                            })
                                                            .unwrap_or_default();
                                                        Ok(serde_json::json!(results).to_string())
                                                    }
                                                    Err(e) => Err(e),
                                                }
                                            }
                                            Err(e) => Err(e),
                                        }
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            (None, _) => Err("missing 'query'".into()),
                            (_, None) => Err("missing BRAVE_API_KEY".into()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "web_search".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for MemorySearchProcedure {
    fn name(&self) -> &str {
        "memory_search"
    }

    fn handles(&self) -> &str {
        "memory_search"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                        let limit =
                            args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

                        if query.is_empty() {
                            Err("missing 'query' parameter".to_string())
                        } else {
                            match self.plures_lm.recall(query, limit, &[]).await {
                                Ok(entries) => {
                                    let results: Vec<serde_json::Value> = entries
                                        .into_iter()
                                        .map(|e| {
                                            json!({
                                                "id": e.id,
                                                "content": e.content,
                                                "category": format!("{:?}", e.category),
                                                "tags": e.tags,
                                                "created_at": e.created_at
                                            })
                                        })
                                        .collect();
                                    Ok(serde_json::to_string_pretty(&results)
                                        .unwrap_or_else(|_| "[]".to_string()))
                                }
                                Err(e) => Err(format!("recall failed: {e}")),
                            }
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "memory_search".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for MemoryStoreProcedure {
    fn name(&self) -> &str {
        "memory_store"
    }

    fn handles(&self) -> &str {
        "memory_store"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let text = args.get("content").and_then(|v| v.as_str());
                        let tags: Vec<String> = args
                            .get("tags")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();

                        match text {
                            Some(fact) if !fact.trim().is_empty() => {
                                match self.plures_lm.capture_fact(fact, tags).await {
                                    Ok(Some(id)) => Ok(format!("Stored memory: {id}")),
                                    Ok(None) => Ok("Memory rejected by quality gate".to_string()),
                                    Err(e) => Err(format!("store failed: {e}")),
                                }
                            }
                            _ => Err("missing or empty 'content' parameter".to_string()),
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "memory_store".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for CronListProcedure {
    fn name(&self) -> &str {
        "cron_list"
    }

    fn handles(&self) -> &str {
        "cron_list"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, .. } => {
                let tasks = self.scheduler.list().await;
                let output = if tasks.is_empty() {
                    "No scheduled tasks.".to_string()
                } else {
                    let items: Vec<serde_json::Value> = tasks
                        .iter()
                        .map(|t| {
                            serde_json::json!({
                                "id": t.id,
                                "name": t.name,
                                "schedule": t.schedule,
                                "enabled": t.enabled,
                                "last_run": t.last_run.map(|d| d.to_rfc3339()),
                                "last_result": t.last_result,
                            })
                        })
                        .collect();
                    serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into())
                };
                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "cron_list".into(),
                    content: output,
                    is_error: false,
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for CronAddProcedure {
    fn name(&self) -> &str {
        "cron_add"
    }

    fn handles(&self) -> &str {
        "cron_add"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let schedule_kind = args
                            .get("schedule_kind")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let command = args.get("command").and_then(|v| v.as_str()).unwrap_or("");

                        if command.is_empty() {
                            Err("missing 'command' parameter".to_string())
                        } else {
                            let schedule = match schedule_kind {
                                "interval" => {
                                    let secs = args
                                        .get("every_secs")
                                        .and_then(|v| v.as_u64())
                                        .unwrap_or(3600);
                                    pares_agens_agenda::scheduler::Schedule::Interval {
                                        every_secs: secs,
                                    }
                                }
                                "cron" => {
                                    let expr = args
                                        .get("cron_expr")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("0 * * * *");
                                    pares_agens_agenda::scheduler::Schedule::Cron {
                                        expr: expr.to_string(),
                                    }
                                }
                                "once" => {
                                    let at_str =
                                        args.get("at").and_then(|v| v.as_str()).unwrap_or("");
                                    match chrono::DateTime::parse_from_rfc3339(at_str) {
                                        Ok(dt) => pares_agens_agenda::scheduler::Schedule::Once {
                                            at: dt.with_timezone(&chrono::Utc),
                                        },
                                        Err(e) => {
                                            return vec![Event::ToolResult {
                                                tool_call_id: id.clone(),
                                                tool_name: "cron_add".into(),
                                                content: format!("invalid 'at' timestamp: {e}"),
                                                is_error: true,
                                            }]
                                        }
                                    }
                                }
                                _ => {
                                    return vec![Event::ToolResult {
                                        tool_call_id: id.clone(),
                                        tool_name: "cron_add".into(),
                                        content:
                                            "schedule_kind must be 'interval', 'cron', or 'once'"
                                                .into(),
                                        is_error: true,
                                    }];
                                }
                            };

                            let task_id = format!("cron.{}", uuid::Uuid::new_v4());
                            let task = pares_agens_agenda::scheduler::Task {
                                id: task_id.clone(),
                                name: if name.is_empty() {
                                    command.to_string()
                                } else {
                                    name.to_string()
                                },
                                schedule,
                                command: command.to_string(),
                                enabled: true,
                                last_run: None,
                                last_result: None,
                                ..Default::default()
                            };
                            self.scheduler.add(task).await;
                            Ok(format!("Scheduled task created: {task_id}"))
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "cron_add".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for CronRemoveProcedure {
    fn name(&self) -> &str {
        "cron_remove"
    }

    fn handles(&self) -> &str {
        "cron_remove"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let task_id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        if task_id.is_empty() {
                            Err("missing 'id' parameter".to_string())
                        } else if self.scheduler.remove(task_id).await {
                            Ok(format!("Removed task: {task_id}"))
                        } else {
                            Err(format!("Task not found: {task_id}"))
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "cron_remove".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for CronToggleProcedure {
    fn name(&self) -> &str {
        "cron_toggle"
    }

    fn handles(&self) -> &str {
        "cron_toggle"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => {
                        let task_id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                        let enabled = args
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        if task_id.is_empty() {
                            Err("missing 'id' parameter".to_string())
                        } else if self.scheduler.set_enabled(task_id, enabled).await {
                            Ok(format!("Task {task_id} enabled={enabled}"))
                        } else {
                            Err(format!("Task not found: {task_id}"))
                        }
                    }
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "cron_toggle".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for ShadowStatusProcedure {
    fn name(&self) -> &str {
        "shadow_status"
    }

    fn handles(&self) -> &str {
        "shadow_status"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, .. } => {
                let candidates = self.shadow.candidates();
                let output = if candidates.is_empty() {
                    serde_json::json!({
                        "status": "no_candidates",
                        "message": "No umbra-evolved shadow candidates loaded. Place .px files with `trigger: manual` in praxis/shadow/ to enroll candidates for evaluation.",
                        "candidates": [],
                        "arena_active": false,
                    })
                } else {
                    let items: Vec<serde_json::Value> = candidates
                        .iter()
                        .map(|c| {
                            serde_json::json!({
                                "name": c.name,
                                "trigger_kind": c.trigger_kind,
                                "arena_status": "pending_arena_wiring",
                            })
                        })
                        .collect();
                    serde_json::json!({
                        "status": "candidates_loaded",
                        "message": format!(
                            "{} shadow candidate(s) loaded. Arena evaluation pending umbra-shadow integration (Phase A, issue #677).",
                            candidates.len()
                        ),
                        "candidates": items,
                        "arena_active": false,
                    })
                };
                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "shadow_status".into(),
                    content: serde_json::to_string_pretty(&output)
                        .unwrap_or_else(|_| "{}".into()),
                    is_error: false,
                }]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl Procedure for ParesManusToolProcedure {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn handles(&self) -> &str {
        self.tool_name
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => match manus_request_for_tool(self.tool_name, args) {
                        Ok((method, params)) => {
                            call_pares_manus(self.manus_ws_url.as_str(), method, params).await
                        }
                        Err(e) => Err(e),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: self.tool_name.to_string(),
                    content: result
                        .as_ref()
                        .map(value_to_tool_content)
                        .unwrap_or_else(|e| e.clone()),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}


async fn call_pares_manus(
    ws_url: &str,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let request_id = Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": params
    })
    .to_string();

    let (mut socket, _) = tokio::time::timeout(MANUS_CONNECT_TIMEOUT, connect_async(ws_url))
        .await
        .map_err(|_| format!("timed out connecting to pares-manus at {ws_url}"))?
        .map_err(|e| format!("failed to connect to pares-manus at {ws_url}: {e}"))?;

    socket
        .send(Message::Text(payload.into()))
        .await
        .map_err(|e| format!("failed to send request to pares-manus: {e}"))?;

    let deadline = tokio::time::Instant::now() + MANUS_RESPONSE_TIMEOUT;
    loop {
        let now = tokio::time::Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            return Err(format!(
                "timed out waiting for pares-manus response for method {method}"
            ));
        }

        let message = tokio::time::timeout(remaining, socket.next())
            .await
            .map_err(|_| format!("timed out waiting for pares-manus response for method {method}"))?
            .ok_or_else(|| "pares-manus closed websocket connection".to_string())?
            .map_err(|e| format!("failed to read pares-manus response: {e}"))?;

        let maybe_value = match message {
            Message::Text(text) => serde_json::from_str::<serde_json::Value>(&text)
                .map(Some)
                .map_err(|e| format!("invalid JSON from pares-manus: {e}"))?,
            Message::Binary(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                .map(Some)
                .map_err(|e| format!("invalid binary JSON from pares-manus: {e}"))?,
            Message::Ping(_) | Message::Pong(_) => None,
            Message::Close(_) => {
                return Err("pares-manus websocket closed before returning a response".to_string())
            }
            Message::Frame(_) => None,
        };

        if let Some(value) = maybe_value {
            let id_matches = value
                .get("id")
                .and_then(|id| id.as_str())
                .map(|id| id == request_id)
                .unwrap_or(false);
            if !id_matches {
                continue;
            }

            if let Some(error) = value.get("error") {
                return Err(format!("pares-manus error: {error}"));
            }

            return value
                .get("result")
                .cloned()
                .ok_or_else(|| "pares-manus response missing 'result'".to_string());
        }
    }
}

// ── Plugin CRUD Procedures ──────────────────────────────────────────────────

struct PluginCrudProcedure {
    tool_name: &'static str,
    executor: Arc<PluginCrudExecutor>,
    runtime: Arc<PluginRuntime>,
}

impl PluginCrudProcedure {
    fn new(
        tool_name: &'static str,
        executor: Arc<PluginCrudExecutor>,
        runtime: Arc<PluginRuntime>,
    ) -> Self {
        Self {
            tool_name,
            executor,
            runtime,
        }
    }
}

#[async_trait]
impl Procedure for PluginCrudProcedure {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn handles(&self) -> &str {
        self.tool_name
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Message { id, content, .. } => {
                let result = match parse_tool_args(content) {
                    Ok(args) => self.dispatch_crud(self.tool_name, args).await,
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: self.tool_name.into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
                }]
            }
            _ => vec![],
        }
    }
}

impl PluginCrudProcedure {
    async fn dispatch_crud(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<String, String> {
        match tool_name {
            "plugin_create" => {
                let entity_type_full = args
                    .get("entity_type")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'entity_type'")?;
                let (plugin_name, entity_type) = entity_type_full
                    .split_once('/')
                    .ok_or("entity_type must be 'plugin/entity' format")?;
                let fields = args.get("fields").cloned().unwrap_or(serde_json::json!({}));
                let id = self
                    .executor
                    .create(entity_type, plugin_name, fields)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::json!({"id": id, "entity_type": entity_type_full}).to_string())
            }
            "plugin_list" => {
                let entity_type_full = args
                    .get("entity_type")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'entity_type'")?;
                let (plugin_name, entity_type) = entity_type_full
                    .split_once('/')
                    .ok_or("entity_type must be 'plugin/entity' format")?;
                let filters = args.get("filters");
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
                let items = self
                    .executor
                    .list(entity_type, plugin_name, filters, limit)
                    .map_err(|e| e.to_string())?;
                Ok(serde_json::to_string(&items).unwrap_or_else(|_| "[]".into()))
            }
            "plugin_update" => {
                let entity_id = args
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'entity_id'")?;
                let fields = args.get("fields").cloned().unwrap_or(serde_json::json!({}));
                self.executor
                    .update(entity_id, fields)
                    .map_err(|e| e.to_string())?;
                Ok("updated".into())
            }
            "plugin_delete" => {
                let entity_id = args
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'entity_id'")?;
                self.executor.delete(entity_id).map_err(|e| e.to_string())?;
                Ok("deleted".into())
            }
            "plugin_move" => {
                let entity_id = args
                    .get("entity_id")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'entity_id'")?;
                let new_parent_id = args
                    .get("new_parent_id")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'new_parent_id'")?;
                // Infer relationship from entity type or use a default
                let relationship = args
                    .get("relationship")
                    .and_then(|v| v.as_str())
                    .unwrap_or("parent");
                self.executor
                    .move_entity(entity_id, new_parent_id, relationship)
                    .map_err(|e| e.to_string())?;
                Ok("moved".into())
            }
            "plugin_search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'query'")?;
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                // Extract plugin name from entity_types if available, otherwise search all
                let entity_types = args
                    .get("entity_types")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect::<Vec<_>>()
                    });
                // Get all installed plugin names
                let plugins = self.runtime.list().await;
                let mut all_results = Vec::new();
                for plugin in &plugins {
                    let types_for_plugin = entity_types.as_ref().map(|types| {
                        types
                            .iter()
                            .filter_map(|t| {
                                t.split_once('/')
                                    .filter(|(p, _)| *p == plugin.name)
                                    .map(|(_, e)| e.to_string())
                            })
                            .collect::<Vec<_>>()
                    });
                    let results = self
                        .executor
                        .search(
                            query,
                            &plugin.name,
                            types_for_plugin.as_deref(),
                            limit.saturating_sub(all_results.len()),
                        )
                        .map_err(|e| e.to_string())?;
                    all_results.extend(results);
                    if all_results.len() >= limit {
                        break;
                    }
                }
                Ok(serde_json::to_string(&all_results).unwrap_or_else(|_| "[]".into()))
            }
            _ => Err(format!("unknown plugin tool: {tool_name}")),
        }
    }
}


fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a UTF-8 text file from disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "write_file".into(),
            description: "Write a UTF-8 text file to disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["path", "content"]
            }),
        },
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace the first occurrence of old_text with new_text in a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_text": {"type": "string"},
                    "new_text": {"type": "string"}
                },
                "required": ["path", "old_text", "new_text"]
            }),
        },
        ToolDefinition {
            name: "list_directory".into(),
            description: "List files in a directory, one per line".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "web_fetch".into(),
            description: "Fetch a URL and return readable content. HTML is automatically converted to text. Supports extract_mode (markdown/text) and max_chars.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "HTTP or HTTPS URL to fetch"},
                    "extract_mode": {"type": "string", "enum": ["markdown", "text"], "description": "Extraction mode for HTML content"},
                    "max_chars": {"type": "integer", "description": "Maximum characters to return (default 30000)"}
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "web_search".into(),
            description: "Search the web via Brave Search API".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "count": {"type": "integer"}
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "browser_open".into(),
            description: "Open a URL via pares-manus browser control".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
                },
                "required": ["url"]
            }),
        },
        ToolDefinition {
            name: "browser_screenshot".into(),
            description: "Capture a screenshot of the active browser via pares-manus".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "browser_click".into(),
            description: "Click browser coordinates via pares-manus GUI automation".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"}
                },
                "required": ["x", "y"]
            }),
        },
        ToolDefinition {
            name: "browser_type".into(),
            description: "Type text into the active browser via pares-manus GUI automation".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"}
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "screen_capture".into(),
            description: "Capture the full screen or a window via pares-manus".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "monitor": {"type": "integer"},
                    "window": {"type": "string"}
                }
            }),
        },
        ToolDefinition {
            name: "cdp_execute".into(),
            description: "Execute a Chrome DevTools Protocol script via pares-manus".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "script": {"type": "string"}
                },
                "required": ["script"]
            }),
        },
        ToolDefinition {
            name: "run_command".into(),
            description: "Run a shell command. Supports background, pty, yield_ms, workdir, env, timeout.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to execute"},
                    "workdir": {"type": "string", "description": "Working directory"},
                    "background": {"type": "boolean", "description": "Run in background"},
                    "pty": {"type": "boolean", "description": "Use pseudo-terminal"},
                    "timeout": {"type": "integer", "description": "Timeout in seconds"},
                    "yieldMs": {"type": "integer", "description": "Wait ms before backgrounding"},
                    "env": {"type": "object", "description": "Additional environment variables"}
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "process".into(),
            description: "Manage background shell sessions: list, poll, log, write, kill.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["list", "poll", "log", "write", "kill"], "description": "Action to perform"},
                    "sessionId": {"type": "string", "description": "Session ID (required for poll/log/write/kill)"},
                    "timeout": {"type": "integer", "description": "Poll timeout in ms"},
                    "data": {"type": "string", "description": "Data to write to stdin"},
                    "offset": {"type": "integer", "description": "Log offset"},
                    "limit": {"type": "integer", "description": "Log limit"}
                },
                "required": ["action"]
            }),
        },
        ToolDefinition {
            name: "memory_search".into(),
            description: "Search long-term memory semantically. Returns the most relevant stored memories matching the query.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Semantic search query"},
                    "limit": {"type": "integer", "description": "Max results to return (default 5)"},
                    "min_score": {"type": "number", "description": "Minimum similarity score (0.0-1.0)"}
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "memory_store".into(),
            description: "Store a fact, decision, or important information in long-term memory with optional tags.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "content": {"type": "string", "description": "The content to store in memory"},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Optional tags for categorization"}
                },
                "required": ["content"]
            }),
        },
        ToolDefinition {
            name: "cron_list".into(),
            description: "List all scheduled cron/interval tasks with their status and last run info.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "cron_add".into(),
            description: "Add a scheduled task. Supports interval (every N seconds), cron (5-field expression), or once (ISO timestamp).".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Human-readable name for the task"},
                    "schedule_kind": {"type": "string", "enum": ["interval", "cron", "once"], "description": "Type of schedule"},
                    "every_secs": {"type": "integer", "description": "Interval in seconds (for schedule_kind=interval)"},
                    "cron_expr": {"type": "string", "description": "Cron expression: min hour dom month dow (for schedule_kind=cron)"},
                    "at": {"type": "string", "description": "ISO 8601 timestamp (for schedule_kind=once)"},
                    "command": {"type": "string", "description": "Shell command to execute"}
                },
                "required": ["schedule_kind", "command"]
            }),
        },
        ToolDefinition {
            name: "cron_remove".into(),
            description: "Remove a scheduled task by ID.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Task ID to remove"}
                },
                "required": ["id"]
            }),
        },
        ToolDefinition {
            name: "cron_toggle".into(),
            description: "Enable or disable a scheduled task by ID.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "Task ID to toggle"},
                    "enabled": {"type": "boolean", "description": "Whether the task should be enabled"}
                },
                "required": ["id", "enabled"]
            }),
        },
    ]
}

fn build_system_prompt(path: Option<PathBuf>) -> Result<String, String> {
    // Explicit path takes priority.
    if let Some(path) = path {
        return std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read system prompt {}: {e}", path.display()));
    }

    // Auto-discover: check $HOME/.pares-radix/SYSTEM-PROMPT.md
    if let Ok(home) = std::env::var("HOME") {
        let home_prompt = PathBuf::from(&home).join(".pares-radix/SYSTEM-PROMPT.md");
        if home_prompt.exists() {
            tracing::info!("Loading system prompt from {}", home_prompt.display());
            return std::fs::read_to_string(&home_prompt)
                .map_err(|e| format!("failed to read {}: {e}", home_prompt.display()));
        }
    }

    // Check ~/.config/pares-radix/system-prompt.md
    if let Ok(home) = std::env::var("HOME") {
        let config_prompt = PathBuf::from(&home).join(".config/pares-radix/system-prompt.md");
        if config_prompt.exists() {
            tracing::info!("Loading system prompt from {}", config_prompt.display());
            return std::fs::read_to_string(&config_prompt)
                .map_err(|e| format!("failed to read {}: {e}", config_prompt.display()));
        }
    }

    // Built-in fallback
    Ok("You are Pares Radix, an AI agent built on the plures technology stack. Be direct, use tools proactively, and push commits without asking.".to_string())
}


const ADAPTER_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1200);
const ADAPTER_DISCOVERY_INTERVAL: Duration = Duration::from_millis(200);
const TELEGRAM_RECONNECT_MAX_ATTEMPTS: u32 = 8;
const TELEGRAM_RECONNECT_BASE_DELAY_SECS: u64 = 2;
const TELEGRAM_RECONNECT_MAX_DELAY_SECS: u64 = 30;
const MANUS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MANUS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);


// NOTE (B1 Option A, Stage R2): the self-update command/task builders that used
// to live inline here have been RELOCATED into `crate::self_update` (the host now
// owns its self-update behavior, single source of truth — ADR-0010). Callers in
// this module use `crate::self_update::*`.


async fn run_adapter_with_recovery(
    adapter: &TelegramAdapter,
    agent: Arc<RwLock<Arc<Agent>>>,
    trace_store: ToolTraceStore,
    stream_broadcast_tx: Option<tokio::sync::broadcast::Sender<pares_radix_core::model::StreamDelta>>,
) -> Result<(), String> {
    let mut attempts = 0u32;
    loop {
        let agent_clone = Arc::clone(&agent);
        let trace_store = trace_store.clone();
        let stream_broadcast_tx = stream_broadcast_tx.clone();
        match adapter
            .run(move |mut event: Event| {
                let agent = Arc::clone(&agent_clone);
                let trace_store = trace_store.clone();
                let stream_broadcast_tx = stream_broadcast_tx.clone();
                Box::pin(async move {
                    let mut trace_request_id: Option<String> = None;
                    let mut verbose_tool_details = false;
                    if let Event::Message {
                        id,
                        channel,
                        content,
                        ..
                    } = &mut event
                    {
                        trace_request_id = Some(id.clone());
                        if channel == "telegram" {
                            let (verbose, stripped) = extract_verbose_tool_marker(content);
                            if verbose {
                                *content = stripped;
                                verbose_tool_details = true;
                            }
                        }
                    }

                    let agent = agent.read().await.clone();
                    let mut response = if let Some(request_id) = trace_request_id.clone() {
                        // Use streaming path for real-time token delivery
                        let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
                        let agent_for_stream = agent.clone();
                        let event_for_stream = event.clone();
                        let handle = tokio::spawn(async move {
                            ACTIVE_TELEGRAM_REQUEST_ID
                                .scope(request_id.clone(), async {
                                    agent_for_stream.handle_event_streaming(event_for_stream, stream_tx).await
                                })
                                .await
                        });

                        // Bridge mpsc → broadcast: forward streaming deltas to the
                        // TelegramAdapter's progressive delivery subscriber.
                        let broadcast_tx_for_bridge = stream_broadcast_tx.clone();
                        tokio::spawn(async move {
                            while let Some(delta) = stream_rx.recv().await {
                                if let Some(ref btx) = broadcast_tx_for_bridge {
                                    let _ = btx.send(delta);
                                }
                            }
                        });

                        handle.await.unwrap_or(None)
                    } else {
                        agent.handle_event(event).await
                    };

                    if let Some(request_id) = trace_request_id {
                        let traces = trace_store.take_for_request(&request_id).await;
                        if verbose_tool_details {
                            if let Some(Event::ModelResponse { content, .. }) = &mut response {
                                content.push_str("\n\n");
                                content.push_str(&format_verbose_tool_traces(&traces));
                            }
                        }
                    }

                    response
                })
            })
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => {
                attempts += 1;
                if attempts > TELEGRAM_RECONNECT_MAX_ATTEMPTS {
                    return Err(format!(
                        "telegram adapter failed after {TELEGRAM_RECONNECT_MAX_ATTEMPTS} retries: {e}"
                    ));
                }
                let delay = std::cmp::min(
                    TELEGRAM_RECONNECT_BASE_DELAY_SECS.saturating_mul(2u64.pow(attempts - 1)),
                    TELEGRAM_RECONNECT_MAX_DELAY_SECS,
                );
                tracing::warn!(
                    attempt = attempts,
                    retry_in_secs = delay,
                    "telegram adapter error; restarting"
                );
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
        }
    }
}

/// Wire and spawn the spine heartbeat runner for a channel.
///
/// All spine-driven channels (stdio, telegram, http) share the same wiring:
/// pipeline emitter + task manager, config load, optional quiet-hours override.
/// The returned shutdown sender must be kept alive for the runner to keep going.
async fn spawn_spine_heartbeat(
    state: Arc<dyn pares_radix_core::state::StateStore>,
    task_manager: Arc<pares_radix_core::task_manager::TaskManager>,
    emitter: pares_radix_core::spine::pipeline::PipelineEmitter,
    started_log: &str,
) -> tokio::sync::watch::Sender<bool> {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut heartbeat =
        pares_agens_core::heartbeat::HeartbeatRunner::new(Arc::clone(&state))
            .with_pipeline_emitter(emitter)
            .with_task_manager(task_manager, state);
    heartbeat.load_config().await;
    if std::env::var("PARES_HEARTBEAT_NO_QUIET").is_ok() {
        let mut cfg = heartbeat.config().clone();
        cfg.quiet_hours_enabled = false;
        heartbeat.set_config(cfg).await;
    }
    tokio::spawn(async move {
        heartbeat.run(shutdown_rx).await;
    });
    info!("{started_log}");
    shutdown_tx
}

async fn flush_pluresdb_on_shutdown(
    store: &PluresDbStore,
    hostname: &str,
    telegram_token: &str,
) -> Result<(), String> {
    store
        .set_host_adapters(
            hostname,
            vec![HostAdapterConfig {
                kind: "telegram".to_string(),
                connection_id: telegram_token.to_string(),
                single_connection: true,
            }],
        )
        .await
        .map_err(|e| format!("pluresdb flush failed: {e}"))
}

async fn read_host_adapter_configs(
    store: &PluresDbStore,
    local_host: &str,
    sync_enabled: bool,
) -> Result<Vec<HostAdapterRecord>, String> {
    let mut records = store
        .list_host_adapters()
        .await
        .map_err(|e| format!("failed to list host adapter configs: {e}"))?;
    if !sync_enabled {
        return Ok(records);
    }

    let deadline = tokio::time::Instant::now() + ADAPTER_DISCOVERY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if records.iter().any(|record| record.host != local_host) {
            break;
        }
        tokio::time::sleep(ADAPTER_DISCOVERY_INTERVAL).await;
        records = store
            .list_host_adapters()
            .await
            .map_err(|e| format!("failed to list host adapter configs: {e}"))?;
    }
    Ok(records)
}


// ===== Agent command handlers (former run_with_providers match arms) =====

pub(crate) async fn run_serve_spine(
    config: Option<String>,
    channel: String,
    telegram_token: String,
    http_port: u16,
    model_url: String,
    model: String,
    use_copilot: bool,
) {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

            use pares_agens_channels::stdio_spine::StdioSpineChannel;
            use pares_agens_channels::telegram_spine::{TelegramSpineChannel, TelegramSpineConfig};
            use pares_radix_core::spine::channel::SpineChannel;
            use pares_radix_core::spine::conversation::{
                ConversationStore, PluresConversationStore,
            };
            use pares_radix_core::spine::pipeline::Pipeline;
            use pares_radix_core::spine::procedures::history_recorder::HistoryRecorder;
            use pares_radix_core::spine::procedures::inbound_router::InboundRouter;
            use pares_radix_core::spine::procedures::model_invoker::ModelInvoker;
            use pares_radix_core::spine::procedures::response_router::ResponseRouter;
            use pares_radix_core::spine::procedures::tool_executor::ToolExecutor;
            use pares_radix_core::spine::reactive::ReactiveRegistry;
            use pares_radix_core::spine::bootstrap;

            // Load .px config (CLI flags override config file values)
            let px_cfg = px_config::load_config(config.as_deref()).unwrap_or_default();

            // Resolve effective values: CLI flag > env var > .px config > default
            let channel = if channel != "telegram" {
                channel
            } else {
                px_cfg
                    .get_str("radix.channel")
                    .unwrap_or("telegram")
                    .to_string()
            };
            let model = if model != "gpt-4o" {
                model
            } else {
                px_cfg
                    .get_str("radix.model")
                    .unwrap_or("gpt-4o")
                    .to_string()
            };
            let model_url = if model_url != "https://models.inference.ai.azure.com" {
                model_url
            } else {
                px_cfg
                    .get_str("model.url")
                    .unwrap_or("https://models.inference.ai.azure.com")
                    .to_string()
            };
            let use_copilot = use_copilot || px_cfg.get_bool("radix.use_copilot").unwrap_or(false);
            let telegram_token = if !telegram_token.is_empty() {
                telegram_token
            } else {
                px_cfg.get_resolved("telegram.token").unwrap_or_default()
            };

            info!("Starting pares-radix in spine-driven mode (ADR-0001)");

            // 1. Set up model client
            let model_client: Arc<dyn ModelClient> = if use_copilot {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                let auth_path = PathBuf::from(&home).join(".pares-radix/copilot-auth.json");
                let cached = std::fs::read_to_string(&auth_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<CopilotAuthCache>(&raw).ok());

                let oauth_token = if let Some(cache) = cached {
                    cache.oauth_token
                } else {
                    let (device_code, user_code, verification_uri) =
                        match CopilotAuth::device_flow_start().await {
                            Ok(response) => response,
                            Err(e) => {
                                error!("Copilot device flow failed: {e}");
                                std::process::exit(1);
                            }
                        };
                    println!(
                        "Authorize Copilot: visit {verification_uri} and enter code {user_code}"
                    );
                    let token = match CopilotAuth::device_flow_poll(&device_code).await {
                        Ok(token) => token,
                        Err(e) => {
                            error!("Copilot device flow polling failed: {e}");
                            std::process::exit(1);
                        }
                    };
                    if let Some(parent) = auth_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Ok(serialized) = serde_json::to_string_pretty(&CopilotAuthCache {
                        oauth_token: token.clone(),
                        cached_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    }) {
                        let _ = std::fs::write(&auth_path, serialized);
                    }
                    token
                };

                let auth = CopilotAuth::new(oauth_token);
                let model_name_arc = Arc::new(RwLock::new(model.clone()));
                Arc::new(CopilotModelClient::new_with_model_handle(auth, model_name_arc))
            } else {
                let provider_config = ProviderConfig::new(&model_url, None);
                let router_config = RouterConfig::single("spine", provider_config);
                let model_router = Arc::new(ModelRouter::new(router_config));
                let model_name_arc = Arc::new(RwLock::new(model.clone()));
                Arc::new(RouterModelClient {
                    router: Arc::new(RwLock::new(model_router)),
                    model: model_name_arc,
                    endpoint: Arc::new(RwLock::new(model_url.clone())),
                    api_key: None,
                })
            };
            info!(model = %model, copilot = use_copilot, "Model client initialized for spine mode");

            // 2. Set up tool dispatcher via SpineProcedureDispatcher (full procedure registry)
            use pares_radix_core::spine::dispatcher::SpineProcedureDispatcher;

            let shell_executor = Arc::new(ShellExecutor::new());
            let mut spine_registry = ProcedureRegistry::new();
            spine_registry.register(Box::new(ReadFileProcedure));
            spine_registry.register(Box::new(WriteFileProcedure));
            spine_registry.register(Box::new(EditFileProcedure));
            spine_registry.register(Box::new(ListDirectoryProcedure));
            spine_registry.register(Box::new(RunCommandProcedure {
                executor: Arc::clone(&shell_executor),
            }));
            spine_registry.register(Box::new(ProcessManageProcedure {
                executor: Arc::clone(&shell_executor),
            }));

            // Web tools
            spine_registry.register(Box::new(WebFetchProcedure));
            let brave_api_key = std::env::var("BRAVE_API_KEY").ok();
            spine_registry.register(Box::new(WebSearchProcedure::new(brave_api_key)));

            // Cron/scheduler tools
            let scheduler = Arc::new(pares_agens_agenda::scheduler::Scheduler::new());
            spine_registry.register(Box::new(CronListProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            spine_registry.register(Box::new(CronAddProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            spine_registry.register(Box::new(CronRemoveProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            spine_registry.register(Box::new(CronToggleProcedure {
                scheduler: Arc::clone(&scheduler),
            }));

            // Memory tools (PluresDB + fastembed)
            {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                let memory_path = PathBuf::from(&home).join(".pares-radix/memory");
                let fastembed_cache = std::env::var("FASTEMBED_CACHE_PATH")
                    .unwrap_or_else(|_| format!("{home}/.cache/fastembed"));
                std::fs::create_dir_all(&fastembed_cache).ok();
                #[allow(unused_unsafe)]
                unsafe {
                    std::env::set_var("FASTEMBED_CACHE_PATH", &fastembed_cache);
                }
                let memory_store: Arc<dyn pares_agens_core::memory::store::MemoryStore> =
                    match PluresDbStore::open_with_embeddings(&memory_path) {
                        Ok(store) => {
                            info!("PluresDB memory with native fastembed enabled for spine");
                            Arc::new(store)
                        }
                        Err(e) => {
                            warn!("fastembed unavailable ({e}), falling back to basic store");
                            match PluresDbStore::open(&memory_path) {
                                Ok(store) => Arc::new(store),
                                Err(e2) => {
                                    error!("failed to open memory store: {e2}");
                                    std::process::exit(1);
                                }
                            }
                        }
                    };
                use pares_agens_core::memory::embed::{EmbeddingProvider, MockEmbedder};
                let embedder: Box<dyn EmbeddingProvider> = Box::new(MockEmbedder);
                let plures_lm = Arc::new(PluresLm::new(memory_store, embedder, 128_000));
                spine_registry.register(Box::new(MemorySearchProcedure {
                    plures_lm: Arc::clone(&plures_lm),
                }));
                spine_registry.register(Box::new(MemoryStoreProcedure {
                    plures_lm: Arc::clone(&plures_lm),
                }));
            }

            let spine_registry = Arc::new(RwLock::new(spine_registry));
            let spine_tool_definitions = vec![
                ToolDefinition {
                    name: "read_file".into(),
                    description: "Read a UTF-8 text file from disk".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string", "description": "Path to the file to read"}
                        },
                        "required": ["path"]
                    }),
                },
                ToolDefinition {
                    name: "write_file".into(),
                    description: "Write content to a file, creating parent dirs if needed".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "content": {"type": "string"}
                        },
                        "required": ["path", "content"]
                    }),
                },
                ToolDefinition {
                    name: "edit_file".into(),
                    description: "Replace the first occurrence of old_text with new_text in a file".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"},
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"}
                        },
                        "required": ["path", "old_text", "new_text"]
                    }),
                },
                ToolDefinition {
                    name: "list_directory".into(),
                    description: "List files in a directory".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"}
                        },
                        "required": ["path"]
                    }),
                },
                ToolDefinition {
                    name: "run_command".into(),
                    description: "Run a shell command. Supports background, pty, timeout, workdir, env.".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": {"type": "string", "description": "Shell command to execute"},
                            "workdir": {"type": "string"},
                            "background": {"type": "boolean"},
                            "pty": {"type": "boolean"},
                            "timeout": {"type": "integer"},
                            "yieldMs": {"type": "integer"},
                            "env": {"type": "object"}
                        },
                        "required": ["command"]
                    }),
                },
                ToolDefinition {
                    name: "process".into(),
                    description: "Manage background shell sessions: list, poll, log, write, kill.".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "action": {"type": "string", "enum": ["list", "poll", "log", "write", "kill"]},
                            "sessionId": {"type": "string"},
                            "timeout": {"type": "integer"},
                            "data": {"type": "string"},
                            "offset": {"type": "integer"},
                            "limit": {"type": "integer"}
                        },
                        "required": ["action"]
                    }),
                },
                ToolDefinition {
                    name: "web_fetch".into(),
                    description: "Fetch and extract readable content from a URL (HTML → text)".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "url": {"type": "string", "description": "URL to fetch"},
                            "extractMode": {"type": "string", "enum": ["markdown", "text"], "description": "Extraction mode"},
                            "maxChars": {"type": "integer", "description": "Max characters to return"}
                        },
                        "required": ["url"]
                    }),
                },
                ToolDefinition {
                    name: "web_search".into(),
                    description: "Search the web using Brave Search API".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Search query"},
                            "count": {"type": "integer", "description": "Number of results (default 5)"}
                        },
                        "required": ["query"]
                    }),
                },
                ToolDefinition {
                    name: "cron_list".into(),
                    description: "List all scheduled cron jobs".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {},
                        "required": []
                    }),
                },
                ToolDefinition {
                    name: "cron_add".into(),
                    description: "Add a new cron job with name, schedule expression, and command".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "Job name"},
                            "schedule": {"type": "string", "description": "Cron expression (e.g. '0 */6 * * *')"},
                            "command": {"type": "string", "description": "Shell command to run"}
                        },
                        "required": ["name", "schedule", "command"]
                    }),
                },
                ToolDefinition {
                    name: "cron_remove".into(),
                    description: "Remove a cron job by name".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "Job name to remove"}
                        },
                        "required": ["name"]
                    }),
                },
                ToolDefinition {
                    name: "cron_toggle".into(),
                    description: "Enable or disable a cron job by name".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "Job name"},
                            "enabled": {"type": "boolean", "description": "Whether to enable (true) or disable (false)"}
                        },
                        "required": ["name", "enabled"]
                    }),
                },
                ToolDefinition {
                    name: "memory_search".into(),
                    description: "Search long-term memory semantically. Returns the most relevant stored memories matching the query.".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "Semantic search query"},
                            "limit": {"type": "integer", "description": "Max results to return (default 5)"},
                            "min_score": {"type": "number", "description": "Minimum similarity score (0.0-1.0)"}
                        },
                        "required": ["query"]
                    }),
                },
                ToolDefinition {
                    name: "memory_store".into(),
                    description: "Store a fact, decision, or important information in long-term memory with optional tags.".into(),
                    parameters: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "content": {"type": "string", "description": "The content to store in memory"},
                            "tags": {"type": "array", "items": {"type": "string"}, "description": "Optional tags for categorization"}
                        },
                        "required": ["content"]
                    }),
                },
            ];
            let spine_tool_dispatcher_builder =
                SpineProcedureDispatcher::with_tools(spine_registry, spine_tool_definitions);

            // 3. Create the reactive registry + pipeline
            let reactive_registry = Arc::new(ReactiveRegistry::new());
            let (pipeline, rx) = Pipeline::with_reactive(256, Arc::clone(&reactive_registry));

            // Set the emitter on the registry so .px procedures can emit back into the pipeline
            reactive_registry.set_emitter(pipeline.emitter()).await;

            // 3.5. Open THE shared PluresDB instance — all state goes here.
            // v1.55.13: the durable state store and the conversation store share
            // ONE CrdtStore. Build the PluresDbStateStore first (single owner of
            // the sled handle) and derive the shared CrdtStore from it via
            // `.crdt_store()`, mirroring the upstream canonical wiring
            // (radix-core spine::runtime `PluresDbStateStore::open` +
            // `PluresConversationStore::new(pdb.crdt_store())`).
            use pares_radix_core::state::{PluresDbStateStore, StateStore};
            use pares_radix_core::CrdtStore;
            let pluresdb_dir = PathBuf::from(&home).join(".pares-radix/runtime-state");
            std::fs::create_dir_all(&pluresdb_dir).ok();
            // Build the concrete PluresDbStateStore first so we can extract the
            // shared CrdtStore (via the inherent `.crdt_store()`) before erasing
            // it to `dyn StateStore` for the CompositeActionHandler.
            let pdb = match PluresDbStateStore::open(&pluresdb_dir) {
                Ok(pdb) => {
                    info!(path = %pluresdb_dir.display(), "PluresDB opened (shared instance)");
                    pdb
                }
                Err(e) => {
                    warn!(error = %e, "Failed to open PluresDB, using in-memory");
                    PluresDbStateStore::in_memory()
                }
            };
            // The shared CrdtStore backing conversation + task state is the SAME
            // store owned by `state_store` (co-location invariant preserved).
            let shared_store: Arc<CrdtStore> = pdb.crdt_store();
            let state_store: Arc<dyn StateStore> = Arc::new(pdb);

            // Conversation store writes to the shared PluresDB
            let conversation_store: Arc<dyn ConversationStore> =
                Arc::new(PluresConversationStore::new(Arc::clone(&shared_store)));

            // 3.7. Create TaskManager + StateStore for autonomous task execution
            // TaskManager uses the shared CrdtStore for task CRUD.
            // Heartbeat state (config, counters) uses a separate in-memory store.
            let spine_task_manager =
                Arc::new(pares_radix_core::task_manager::TaskManager::new(Arc::clone(&shared_store)));
            let spine_heartbeat_state: Arc<dyn pares_radix_core::state::StateStore> =
                Arc::new(pares_radix_core::state::InMemoryStateStore::default());
            info!("TaskManager + StateStore initialized for ServeSpine");

            // 3.8. Finalize tool dispatcher with task registry
            use pares_radix_core::tools::TaskRegistryTool;
            let task_registry = Arc::new(TaskRegistryTool::new(Arc::clone(&spine_task_manager)));
            let base_tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(
                spine_tool_dispatcher_builder.with_task_registry(Arc::clone(&task_registry)),
            );
            let spine_tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(
                TaskGraphToolDispatcher::new(
                    base_tool_dispatcher,
                    Arc::clone(&spine_task_manager),
                ),
            );

            // 3.6. Load system prompt — compose from context files like OpenClaw
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let workspace = std::env::var("PARES_WORKSPACE").unwrap_or_else(|_| {
                PathBuf::from(&home)
                    .join(".pares-radix/workspace")
                    .to_string_lossy()
                    .to_string()
            });
            let workspace_path = PathBuf::from(&workspace);

            // Load context files in priority order
            let mut context_parts: Vec<String> = Vec::new();

            // 1. SOUL.md — persona and tone
            let soul_path = workspace_path.join("SOUL.md");
            if soul_path.exists() {
                if let Ok(soul) = std::fs::read_to_string(&soul_path) {
                    context_parts.push(format!("## Persona\n{}", soul.trim()));
                    info!("Loaded SOUL.md");
                }
            }

            // 2. USER.md — who we're helping
            let user_path = workspace_path.join("USER.md");
            if user_path.exists() {
                if let Ok(user) = std::fs::read_to_string(&user_path) {
                    context_parts.push(format!("## User Context\n{}", user.trim()));
                    info!("Loaded USER.md");
                }
            }

            // 3. AGENTS.md — workspace conventions
            let agents_path = workspace_path.join("AGENTS.md");
            if agents_path.exists() {
                if let Ok(agents) = std::fs::read_to_string(&agents_path) {
                    // Truncate AGENTS.md if it's very long (keep first 4K)
                    let truncated = if agents.len() > 4096 {
                        format!("{}\n...(truncated)", &agents[..4096])
                    } else {
                        agents
                    };
                    context_parts.push(format!("## Workspace Conventions\n{}", truncated.trim()));
                    info!("Loaded AGENTS.md");
                }
            }

            // 4. SYSTEM-PROMPT.md — explicit override (highest priority)
            let system_prompt_path = PathBuf::from(&home).join(".pares-radix/SYSTEM-PROMPT.md");
            if system_prompt_path.exists() {
                if let Ok(prompt) = std::fs::read_to_string(&system_prompt_path) {
                    context_parts.insert(0, prompt);
                    info!("Loaded SYSTEM-PROMPT.md (override)");
                }
            }

            // 5. .px config personality (supplements workspace files)
            if let Some(name) = px_cfg.get_str("personality.name") {
                context_parts.push(format!("## Identity\nYour name is {}.", name));
                info!(name = %name, "Loaded personality name from .px config");
            }
            if let Some(prompt) = px_cfg.get_str("personality.system_prompt") {
                // Only use .px system_prompt as fallback if no workspace files loaded it
                if context_parts.is_empty() {
                    context_parts.push(prompt.to_string());
                    info!("Using personality.system_prompt from .px config as base prompt");
                } else {
                    // Append as supplementary instruction
                    context_parts.push(format!("## Additional Instructions\n{}", prompt));
                    info!("Appended personality.system_prompt from .px config");
                }
            }

            let system_prompt = if context_parts.is_empty() {
                "You are a software engineering assistant with access to shell commands, file operations, web search, and web fetch. You can execute code, read/write files, search the web, and help with development tasks. Be direct and concise. Use tools proactively.".to_string()
            } else {
                context_parts.join("\n\n")
            };

            // Inject pending task list into system prompt (auto-populated context)
            let task_context = task_registry.context_block();
            let system_prompt = if task_context.is_empty() {
                system_prompt
            } else {
                format!("{}\n{}", system_prompt, task_context)
            };
            info!(prompt_source = %if system_prompt_path.exists() { "SYSTEM-PROMPT.md" } else if !context_parts.is_empty() { "workspace context" } else { "default" }, prompt_len = system_prompt.len(), "System prompt loaded");

            // 4. Register procedures (full pipeline: inbound → history → model → tools → response)
            // Create the streaming broadcast channel FIRST — ModelInvoker sends deltas here,
            // channel handlers (Telegram) subscribe. Zero overhead if unused.
            let (stream_broadcast_tx, _) = tokio::sync::broadcast::channel::<pares_radix_core::model::StreamDelta>(256);

            pipeline.register(Arc::new(InboundRouter::with_reactive(Arc::clone(&reactive_registry)))).await;
            pipeline
                .register(Arc::new(HistoryRecorder::new(Arc::clone(
                    &conversation_store,
                ))))
                .await;
            let spine_tool_count = spine_tool_dispatcher.available_tools().await.len();
            let model_invoker = ModelInvoker::with_system_prompt(
                Arc::clone(&model_client),
                Arc::clone(&spine_tool_dispatcher),
                &system_prompt,
            )
            .with_conversation_store(Arc::clone(&conversation_store))
            .with_task_manager(Arc::clone(&spine_task_manager))
            .with_stream_sender(stream_broadcast_tx.clone());
            pipeline
                .register(Arc::new(SpineCommandGate::new(
                    model_invoker,
                    model.clone(),
                    spine_tool_count,
                )))
                .await;
            pipeline
                .register(Arc::new(ToolExecutor::new(Arc::clone(&spine_tool_dispatcher))))
                .await;
            pipeline.register(Arc::new(ResponseRouter)).await;
            // CommitmentDetector: fallback task creation for when the model commits to work
            // without explicitly calling task_create. Only fires on DeliveryRequest (text-only
            // responses — tool_call responses are handled by ToolExecutor and never reach here).
            pipeline
                .register(Arc::new(
                    pares_radix_core::spine::procedures::commitment_detector::CommitmentDetector::new(
                        Arc::clone(&spine_task_manager),
                    ),
                ))
                .await;
            info!("Pipeline procedures registered: inbound_router, history_recorder, model_invoker, tool_executor, response_router, commitment_detector");

            // 4.5. Assemble the PX action boundary against this LIVE pipeline.
            // The procedure graph owns task choice, ordering and lifecycle
            // transitions. Rust only exposes durable reads/writes plus the
            // final pipeline injection side effect.
            {
                use pares_radix_core::px_adapter::{
                    load_px_procedures, AsyncActionHandler, PxProcedureAdapter,
                    ToolDispatchActionHandler,
                };
                use pares_radix_core::spine::actions::CompositeActionHandler;
                use pares_radix_core::spine::task_dispatch_actions::TaskDispatchActionHandler;
                use pares_radix_core::task_executor::TaskDispatcher;
                use pares_agens_core::orchestrator::actions::{
                    CerebellumActionHandler, SpineActionRouter,
                };

                let tool_handler = Arc::new(ToolDispatchActionHandler::new(Arc::clone(&spine_tool_dispatcher)));
                let mut composite = CompositeActionHandler::new(
                    Arc::clone(&conversation_store),
                    Arc::clone(&state_store),
                    Arc::clone(&tool_handler),
                )
                .with_task_grounding(Arc::clone(&spine_task_manager));
                let task_dispatcher = Arc::new(
                    TaskDispatcher::new(Arc::clone(&state_store))
                        .with_pipeline_emitter(pipeline.emitter()),
                );
                composite.set_task_dispatch(Arc::new(TaskDispatchActionHandler::new(
                    task_dispatcher,
                    Some(Arc::clone(&spine_task_manager)),
                )));
                // The platform composite owns durable PluresDB, task and tool
                // boundaries. Cognition actions are a separate, explicit
                // registration: without this router they fell through to the
                // model tool registry and every `.px` classification/routing
                // step was reported as an unregistered tool.
                let cognition: Arc<dyn AsyncActionHandler> = Arc::new(
                    CerebellumActionHandler::new_minimal()
                        .with_model_client(Arc::clone(&model_client))
                        .with_tool_dispatcher(Arc::clone(&spine_tool_dispatcher)),
                );
                // Load from repo-local praxis/ (shipped with the binary)
                let praxis_dirs = [
                    PathBuf::from(&home).join(".pares-radix/praxis/procedures"),
                    PathBuf::from(&home).join(".pares-radix/praxis/spine"),
                ];

                // Assemble source into adapters before registering either named
                // or reactive procedures. The autonomous policy is contract-
                // checked first; a rejected source is absent from both routes.
                // Radix still owns canonical trigger mapping and registration.
                let spine_action_router = Arc::new(SpineActionRouter::new(
                    Arc::new(composite),
                    cognition,
                ));
                let procedure_bridge = Arc::new(PxBridge::new(
                    Arc::clone(&spine_action_router) as Arc<dyn AsyncActionHandler>,
                ));
                let mut bridge_registered = 0;
                let mut reactive_adapters: Vec<PxProcedureAdapter> = Vec::new();

                for praxis_dir in &praxis_dirs {
                    if !praxis_dir.is_dir() {
                        debug!(dir = %praxis_dir.display(), ".px directory not found, skipping");
                        continue;
                    }

                    for entry in WalkDir::new(praxis_dir)
                        .into_iter()
                        .filter_map(Result::ok)
                        .filter(|entry| {
                            entry.file_type().is_file()
                                && entry.path().extension().is_some_and(|extension| extension == "px")
                        })
                    {
                        let procedure_path = entry.into_path();
                        let source = match std::fs::read_to_string(&procedure_path) {
                            Ok(source) => source,
                            Err(error) => {
                                error!(file = %procedure_path.display(), %error, "Could not read .px source");
                                continue;
                            }
                        };
                        let autonomous_dispatch = procedure_path
                            .file_name()
                            .is_some_and(|name| name == "autonomous-dispatch.px");

                        if autonomous_dispatch {
                            if let Err(diagnostics) = PxBridge::validate_source_contract(
                                &source,
                                &autonomous_dispatch_catalog(),
                                AUTONOMOUS_DISPATCH_PROFILE,
                            ) {
                                error!(
                                    file = %procedure_path.display(),
                                    %diagnostics,
                                    "Rejected autonomous PX policy: complete spine contract diagnostics"
                                );
                                continue;
                            }
                        }

                        // Compile before bridge registration so a source can
                        // never be named-callable but missing its reactive form.
                        let adapters = match load_px_procedures(
                            &source,
                            Arc::clone(&spine_action_router) as Arc<dyn AsyncActionHandler>,
                        ) {
                            Ok(adapters) => adapters,
                            Err(error) => {
                                error!(file = %procedure_path.display(), %error, "Could not compile .px source");
                                continue;
                            }
                        };

                        // `PxProcedureAdapter` is immutable after compilation.
                        // Clone the adapters—not the source compilation—so the
                        // named bridge and reactive registry share one parse.
                        bridge_registered += procedure_bridge.load_adapters(adapters.clone()).await;
                        reactive_adapters.extend(adapters);
                    }
                }
                spine_action_router
                    .set_procedure_bridge(Arc::clone(&procedure_bridge))
                    .await;
                info!(registered = bridge_registered, "Named .px procedure bridge loaded");
                let total_registered = bootstrap::register_reactive_adapters(
                    reactive_adapters,
                    &reactive_registry,
                    None,
                )
                .await;

                if total_registered > 0 {
                    let trigger_count = reactive_registry.trigger_count().await;
                    info!(
                        registered = total_registered,
                        triggers = trigger_count,
                        "Reactive .px procedures loaded via bootstrap"
                    );
                } else {
                    warn!("No .px procedures found for reactive bootstrap");
                }
            }

            // 5. Start the pipeline event loop
            let pipeline_for_loop = Arc::clone(&pipeline);
            tokio::spawn(async move {
                pipeline_for_loop.run(rx).await;
            });
            info!("Pipeline event loop started");

            // 5.5. Periodic task evaluation timer (60s)
            {
                use pares_radix_core::spine::event::SpineEvent;
                let timer_emitter = pipeline.emitter();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(60));
                    loop {
                        interval.tick().await;
                        timer_emitter
                            .emit(SpineEvent::Timer {
                                id: SpineEvent::new_id(),
                                name: "task_eval".into(),
                            })
                            .await;
                    }
                });
                info!("Task evaluation timer started (60s interval)");
            }

            // 6. Start channel (delivery loop + receiver)
            let delivery_rx = pipeline.subscribe_deliveries();

            match channel.as_str() {
                "stdio" => {
                    let stdio_channel = StdioSpineChannel::new();
                    tokio::spawn(async move {
                        stdio_channel.run_delivery_loop(delivery_rx).await;
                    });
                    info!("Stdio delivery loop started");

                    // 6.5. Start heartbeat runner (proactive behavior)
                    let _heartbeat_shutdown_tx = spawn_spine_heartbeat(
                        Arc::clone(&spine_heartbeat_state),
                        Arc::clone(&spine_task_manager),
                        pipeline.emitter(),
                        "Heartbeat runner started (proactive behavior)",
                    )
                    .await;

                    // 7. Start receiving (blocks until /quit or EOF)
                    let emitter = pipeline.emitter();
                    let receiver = StdioSpineChannel::new();
                    info!("Starting stdio receiver — spine-driven mode active");
                    if let Err(e) = receiver.start_receiving(emitter).await {
                        error!(error = %e, "Stdio receiver failed");
                        std::process::exit(1);
                    }
                }
                "telegram" => {
                    if telegram_token.is_empty() {
                        error!("--telegram-token is required for --channel telegram");
                        std::process::exit(1);
                    }
                    // Use the shared stream broadcast created at pipeline level.
                    // ModelInvoker sends deltas here; TelegramSpineChannel subscribes.
                    let tg_channel = TelegramSpineChannel::with_stream(
                        TelegramSpineConfig { token: telegram_token.clone() },
                        stream_broadcast_tx.clone(),
                    );
                    tokio::spawn(async move {
                        tg_channel.run_delivery_loop(delivery_rx).await;
                    });
                    info!("Telegram delivery loop started (progressive streaming enabled)");

                    // 6.5. Start heartbeat runner (proactive behavior)
                    let _heartbeat_shutdown_tx = spawn_spine_heartbeat(
                        Arc::clone(&spine_heartbeat_state),
                        Arc::clone(&spine_task_manager),
                        pipeline.emitter(),
                        "Heartbeat runner started (proactive behavior + task dispatch)",
                    )
                    .await;

                    // 7. Start receiving (blocks)
                    let emitter = pipeline.emitter();
                    let receiver_channel = TelegramSpineChannel::with_stream(
                        TelegramSpineConfig { token: telegram_token },
                        stream_broadcast_tx,
                    );
                    info!("Starting Telegram receiver — spine-driven mode active");
                    if let Err(e) = receiver_channel.start_receiving(emitter).await {
                        error!(error = %e, "Telegram receiver failed");
                        std::process::exit(1);
                    }
                }
                "http" => {
                    use pares_agens_channels::http_spine::{
                        start_http_server, HttpSpineChannel, HttpSpineConfig, PendingResponses,
                    };

                    let http_config = HttpSpineConfig {
                        port: http_port,
                        bearer_token: None,
                        timeout_seconds: 120,
                    };
                    let pending = Arc::new(PendingResponses::default());

                    // Delivery loop routes responses to pending HTTP requests
                    let pending_for_delivery = Arc::clone(&pending);
                    tokio::spawn(async move {
                        let channel = HttpSpineChannel::new(HttpSpineConfig::default());
                        channel
                            .run_delivery_loop(delivery_rx, pending_for_delivery)
                            .await;
                    });

                    // HTTP has the same autonomous queue semantics as the
                    // interactive channels: heartbeat writes a tick; PX selects
                    // and claims work; TaskDispatcher performs the re-drive.
                    let _heartbeat_shutdown_tx = spawn_spine_heartbeat(
                        Arc::clone(&spine_heartbeat_state),
                        Arc::clone(&spine_task_manager),
                        pipeline.emitter(),
                        "Heartbeat runner started (HTTP + PX task dispatch)",
                    )
                    .await;

                    // Start HTTP server (blocks)
                    let emitter = pipeline.emitter();
                    info!(
                        port = http_port,
                        "Starting HTTP channel — POST /v1/chat to interact"
                    );
                    if let Err(e) = start_http_server(emitter, pending, http_config).await {
                        error!(error = %e, "HTTP server failed");
                        std::process::exit(1);
                    }
                }
                other => {
                    error!(channel = %other, "Unknown channel. Supported: stdio, telegram, http");
                    std::process::exit(1);
                }
            }
        }

pub(crate) async fn run_serve(
    telegram_token: String,
    model_url: String,
    model: String,
    copilot: bool,
    deep_model: String,
    fast_model: String,
    deep_model_url: Option<String>,
    api_key: Option<String>,
    embed_url: Option<String>,
    embed_model: String,
    system_prompt: Option<std::path::PathBuf>,
    brave_api_key: Option<String>,
    manus_ws_url: String,
    sync_topic_key: Option<String>,
    sync_shared_key: Option<String>,
    no_event_spine: bool,
    bitnet_model_path: Option<std::path::PathBuf>,
    cerebellum_model_path: Option<std::path::PathBuf>,
) {
    let radix_config = super::config::RadixConfig::load();
    let (_agens_filter_layer, log_filter_handle) = tracing_subscriber::reload::Layer::new(
        build_env_filter("info").unwrap_or_else(|_| EnvFilter::new("info")),
    );

            tracing::info!(
                commit = env!("GIT_COMMIT_HASH"),
                "Starting Pares Radix daemon"
            );
            let started_at = Instant::now();
            let sync_enabled = sync_topic_key.is_some();

            let system_prompt_path = system_prompt;

            let mut model_url = model_url;
            let mut model = model;
            let mut deep_model = deep_model;
            let mut fast_model = fast_model;
            let mut deep_escalation_enabled = default_deep_escalation_enabled();
            let mut runtime_log_level = "info".to_string();

            // Apply config file overrides — only override CLI "auto" when config
            // specifies a concrete model (not "auto" or empty)
            if model == "auto" && radix_config.model.primary != "auto" && !radix_config.model.primary.is_empty() {
                model = radix_config.model.primary.clone();
            }
            if deep_model == "auto" && radix_config.model.deep != "auto" && !radix_config.model.deep.is_empty() {
                deep_model = radix_config.model.deep.clone();
            }
            if model_url == "https://models.inference.ai.azure.com" && !radix_config.model.endpoint.is_empty() {
                model_url = radix_config.model.endpoint.clone();
            }
            let copilot = copilot || radix_config.model.copilot;

            // For non-copilot mode, "auto" falls back to sensible defaults since
            // we can't discover models from arbitrary OpenAI-compatible endpoints.
            if !copilot {
                if model == "auto" {
                    model = "gpt-4o".to_string();
                    tracing::info!("non-copilot mode: defaulting primary model to gpt-4o");
                }
                if deep_model == "auto" {
                    deep_model = "gpt-4o".to_string();
                    tracing::info!("non-copilot mode: defaulting deep model to gpt-4o");
                }
            }

            if copilot {
                tracing::info!("Copilot auth enabled");
                tracing::info!("Model: {model} (copilot)");
            } else {
                tracing::info!("Model: {model} @ {model_url}");
            }

            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let runtime_state_dir = PathBuf::from(&home).join(".pares-radix/runtime-state");
            let runtime_state_store: Arc<dyn StateStore> =
                match PluresDbStateStore::open(&runtime_state_dir) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        tracing::warn!(
                            path = %runtime_state_dir.display(),
                            error = %e,
                            "failed to open runtime state store; model overrides will not persist"
                        );
                        Arc::new(PluresDbStateStore::in_memory())
                    }
                };

            if let Some(saved) = runtime_state_store
                .get(MODEL_OVERRIDE_STATE_KEY)
                .await
                .and_then(|value| serde_json::from_value::<RuntimeModelOverride>(value).ok())
            {
                tracing::info!(
                    primary_model = %saved.model,
                    deep_model = %saved.deep_model,
                    deep_escalation_enabled = saved.deep_escalation_enabled,
                    "loaded runtime model overrides from PluresDB state"
                );
                model = saved.model;
                deep_model = saved.deep_model;
                deep_escalation_enabled = saved.deep_escalation_enabled;
            }

            if let Some(saved) = runtime_state_store
                .get(RUNTIME_CONFIG_OVERRIDE_STATE_KEY)
                .await
                .and_then(|value| serde_json::from_value::<RuntimeConfigOverride>(value).ok())
            {
                tracing::info!(
                    model = %saved.model,
                    endpoint = %saved.endpoint,
                    log_level = %saved.log_level,
                    "loaded runtime config overrides from PluresDB state"
                );
                model = saved.model;
                model_url = saved.endpoint;
                runtime_log_level = saved.log_level;
            }

            if let Err(e) = apply_runtime_log_level(&log_filter_handle, &runtime_log_level) {
                tracing::warn!(
                    requested_log_level = %runtime_log_level,
                    error = %e,
                    "failed to apply persisted runtime log level; using info"
                );
                runtime_log_level = "info".to_string();
            }

            let model_name = Arc::new(RwLock::new(model.clone()));
            let deep_model_name = Arc::new(RwLock::new(deep_model.clone()));
            let fast_model_name = Arc::new(RwLock::new(fast_model.clone()));
            let available_models_state: Arc<
                RwLock<Vec<pares_radix_core::auth::copilot::AvailableModel>>,
            > = Arc::new(RwLock::new(Vec::new()));
            let agent_ref_state: Arc<RwLock<Option<Arc<Agent>>>> =
                Arc::new(RwLock::new(None));
            let deep_escalation_enabled_state = Arc::new(RwLock::new(deep_escalation_enabled));
            let runtime_log_level_state = Arc::new(RwLock::new(runtime_log_level.clone()));
            let runtime_model_control = Arc::new(RuntimeModelControl {
                primary_model: Arc::clone(&model_name),
                deep_model: Arc::clone(&deep_model_name),
                fast_model: Arc::clone(&fast_model_name),
                available_models: Arc::clone(&available_models_state),
                agent_ref: Arc::clone(&agent_ref_state),
                deep_escalation_enabled: Arc::clone(&deep_escalation_enabled_state),
                state_store: Arc::clone(&runtime_state_store),
            });
            let mut runtime_config_control: Option<Arc<dyn TelegramConfigControl>> = None;

            #[allow(clippy::type_complexity)]
            let (model_client, deep_model_client, fast_model_client_opt): (Arc<dyn ModelClient>, Arc<dyn ModelClient>, Option<Arc<dyn ModelClient>>) =
                if let Some(ref bitnet_path) = bitnet_model_path {
                    tracing::info!(path = %bitnet_path.display(), "using local BitNet model");
                    let client: Arc<dyn ModelClient> =
                        Arc::new(BitnetModelClient::new(bitnet_path));
                    (Arc::clone(&client), client, None)
                } else if copilot {
                    let auth_path = PathBuf::from(&home).join(".pares-radix/copilot-auth.json");
                    let cached = std::fs::read_to_string(&auth_path)
                        .ok()
                        .and_then(|raw| serde_json::from_str::<CopilotAuthCache>(&raw).ok())
                        .filter(|cache| {
                            // Invalidate tokens older than 30 days
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            if cache.cached_at > 0
                                && now.saturating_sub(cache.cached_at) > 30 * 86400
                            {
                                tracing::info!(
                                    "Copilot OAuth token is >30 days old, forcing re-auth"
                                );
                                let _ = std::fs::remove_file(&auth_path);
                                return false;
                            }
                            true
                        });

                    let oauth_token = if let Some(cache) = cached {
                        cache.oauth_token
                    } else {
                        let (device_code, user_code, verification_uri) =
                            match CopilotAuth::device_flow_start().await {
                                Ok(response) => response,
                                Err(e) => {
                                    tracing::error!("copilot device flow failed: {e}");
                                    std::process::exit(1);
                                }
                            };

                        println!(
                            "Authorize Copilot: visit {verification_uri} and enter code {user_code}"
                        );

                        let oauth_token = match CopilotAuth::device_flow_poll(&device_code).await {
                            Ok(token) => token,
                            Err(e) => {
                                tracing::error!("copilot device flow polling failed: {e}");
                                std::process::exit(1);
                            }
                        };

                        if let Some(parent) = auth_path.parent() {
                            if let Err(e) = std::fs::create_dir_all(parent) {
                                tracing::warn!("failed to create copilot auth dir: {e}");
                            }
                        }
                        if let Ok(serialized) = serde_json::to_string_pretty(&CopilotAuthCache {
                            oauth_token: oauth_token.clone(),
                            cached_at: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs(),
                        }) {
                            if let Err(e) = std::fs::write(&auth_path, serialized) {
                                tracing::warn!("failed to persist copilot auth: {e}");
                            }
                        }

                        oauth_token
                    };

                    let mut auth = CopilotAuth::new(oauth_token.clone());
                    let deep_auth = CopilotAuth::new(oauth_token.clone());
                    let fast_auth_token = oauth_token; // Save for fast client creation later

                    // Smart model discovery: if model or deep_model is "auto",
                    // probe the Copilot API for available models and select the best.
                    if model == "auto" || deep_model == "auto" || fast_model == "auto" {
                        tracing::info!("auto-detecting available models...");
                        match auth.list_models().await {
                            Ok(available) if !available.is_empty() => {
                                let selection = pares_radix_core::auth::copilot::select_models(&available);
                                // Retain the full discovered list so `/models`
                                // can enumerate real data (no hardcoded list).
                                *available_models_state.write().await = selection.available.clone();
                                if model == "auto" {
                                    tracing::info!(selected = %selection.primary, "auto-selected primary model");
                                    model = selection.primary;
                                    *model_name.write().await = model.clone();
                                }
                                if deep_model == "auto" {
                                    tracing::info!(selected = %selection.deep, "auto-selected deep model");
                                    deep_model = selection.deep;
                                    *deep_model_name.write().await = deep_model.clone();
                                }
                                if fast_model == "auto" {
                                    if let Some(fast_pick) = selection.fast {
                                        tracing::info!(selected = %fast_pick, "auto-selected fast model");
                                        fast_model = fast_pick;
                                    } else {
                                        tracing::info!("no fast-tier models discovered, fast model disabled");
                                        fast_model = String::new();
                                    }
                                    *fast_model_name.write().await = fast_model.clone();
                                }
                                tracing::info!(
                                    available_count = available.len(),
                                    models = %available.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(", "),
                                    "model discovery complete"
                                );
                            }
                            Ok(_) => {
                                tracing::warn!("model discovery returned empty list, using defaults");
                                if model == "auto" {
                                    model = "claude-sonnet-4.5".to_string();
                                    *model_name.write().await = model.clone();
                                }
                                if deep_model == "auto" {
                                    deep_model = "claude-opus-4.6".to_string();
                                    *deep_model_name.write().await = deep_model.clone();
                                }
                                if fast_model == "auto" {
                                    fast_model = String::new();
                                    *fast_model_name.write().await = fast_model.clone();
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "model discovery failed, using defaults");
                                if model == "auto" {
                                    model = "claude-sonnet-4.5".to_string();
                                    *model_name.write().await = model.clone();
                                }
                                if deep_model == "auto" {
                                    deep_model = "claude-opus-4.6".to_string();
                                    *deep_model_name.write().await = deep_model.clone();
                                }
                                if fast_model == "auto" {
                                    fast_model = String::new();
                                    *fast_model_name.write().await = fast_model.clone();
                                }
                            }
                        }
                    }

                    (
                        Arc::new(
                            CopilotModelClient::new_with_model_handle(
                                auth,
                                Arc::clone(&model_name),
                            ),
                        ),
                        Arc::new(
                            CopilotModelClient::new_with_model_handle(
                                deep_auth,
                                Arc::clone(&deep_model_name),
                            ),
                        ),
                        // Fast model client: only created if a fast model was selected
                        if !fast_model.is_empty() {
                            let fast_auth = CopilotAuth::new(fast_auth_token);
                            Some(Arc::new(
                                CopilotModelClient::new_with_model_handle(
                                    fast_auth,
                                    Arc::clone(&fast_model_name),
                                ),
                            ) as Arc<dyn ModelClient>)
                        } else {
                            None
                        },
                    )
                } else {
                    // Set up model router
                    let provider_config = ProviderConfig::new(&model_url, api_key.clone());
                    let router_config = RouterConfig::single("default", provider_config);
                    let model_router = Arc::new(ModelRouter::new(router_config));

                    let deep_model_url = deep_model_url.unwrap_or_else(|| model_url.clone());
                    let deep_provider_config =
                        ProviderConfig::new(&deep_model_url, api_key.clone());
                    let deep_router_config = RouterConfig::single("deep", deep_provider_config);
                    let deep_model_router = Arc::new(ModelRouter::new(deep_router_config));

                    let primary_router_client = Arc::new(RouterModelClient {
                        router: Arc::new(RwLock::new(model_router)),
                        model: Arc::clone(&model_name),
                        endpoint: Arc::new(RwLock::new(model_url.clone())),
                        api_key: api_key.clone(),
                    });
                    let deep_router_client = Arc::new(RouterModelClient {
                        router: Arc::new(RwLock::new(deep_model_router)),
                        model: Arc::clone(&deep_model_name),
                        endpoint: Arc::new(RwLock::new(deep_model_url)),
                        api_key: api_key.clone(),
                    });

                    runtime_config_control = Some(Arc::new(RuntimeConfigControl {
                        model_control: Arc::clone(&runtime_model_control),
                        primary_client: Arc::clone(&primary_router_client),
                        state_store: Arc::clone(&runtime_state_store),
                        log_level: Arc::clone(&runtime_log_level_state),
                        log_filter_handle: log_filter_handle.clone(),
                    }));

                    (
                        primary_router_client as Arc<dyn ModelClient>,
                        deep_router_client as Arc<dyn ModelClient>,
                        None, // Router path doesn't support fast model yet
                    )
                };
            let deep_model_client: Arc<dyn ModelClient> = Arc::new(ToggleableModelClient::new(
                deep_model_client,
                Arc::clone(&deep_escalation_enabled_state),
            ));

            // Set up PluresDB memory store + PluresLM (native)
            let memory_path = PathBuf::from(home).join(".pares-radix/memory");
            let store = if let Some(topic_key_raw) = sync_topic_key {
                let shared_key = match sync_shared_key {
                    Some(key) => key,
                    None => {
                        tracing::error!(
                            "--sync-topic-key requires --sync-shared-key (or PARES_SYNC_SHARED_KEY)"
                        );
                        std::process::exit(1);
                    }
                };
                let topic_key = match parse_sync_topic_key(&topic_key_raw) {
                    Ok(key) => key,
                    Err(e) => {
                        tracing::error!("invalid --sync-topic-key: {e}");
                        std::process::exit(1);
                    }
                };
                tracing::info!("PluresDB Hyperswarm sync enabled");
                match PluresDbStore::open_with_sync(&memory_path, &topic_key, &shared_key) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        tracing::error!("failed to open sync-enabled memory store: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                // Ensure fastembed cache is in a writable location
                let fastembed_cache = std::env::var("FASTEMBED_CACHE_PATH").unwrap_or_else(|_| {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                    format!("{home}/.cache/fastembed")
                });
                std::fs::create_dir_all(&fastembed_cache).ok();
                #[allow(unused_unsafe)]
                unsafe {
                    std::env::set_var("FASTEMBED_CACHE_PATH", &fastembed_cache);
                }

                match PluresDbStore::open_with_embeddings(&memory_path) {
                    Ok(store) => {
                        tracing::info!(
                            "PluresDB with native fastembed (auto-embed on every write)"
                        );
                        Arc::new(store)
                    }
                    Err(e) => {
                        tracing::warn!("fastembed unavailable ({e}), falling back to basic store");
                        match PluresDbStore::open(&memory_path) {
                            Ok(store) => Arc::new(store),
                            Err(e2) => {
                                tracing::error!("failed to open memory store: {e2}");
                                std::process::exit(1);
                            }
                        }
                    }
                }
            };

            let hostname = current_hostname();
            if let Err(e) = store
                .set_host_adapters(
                    &hostname,
                    vec![HostAdapterConfig {
                        kind: "telegram".to_string(),
                        connection_id: telegram_token.clone(),
                        single_connection: true,
                    }],
                )
                .await
            {
                tracing::error!("failed to persist local adapter config for host {hostname}: {e}");
                std::process::exit(1);
            }

            let host_configs =
                match read_host_adapter_configs(&store, &hostname, sync_enabled).await {
                    Ok(configs) => configs,
                    Err(e) => {
                        tracing::error!("{e}");
                        std::process::exit(1);
                    }
                };

            let conflicts = detect_single_connection_conflicts(&hostname, &host_configs);
            for conflict in &conflicts {
                tracing::error!(
                    adapter = %conflict.kind,
                    connection = %redact_connection_id(&conflict.connection_id),
                    hosts = %conflict.hosts.join(", "),
                    "single-connection adapter conflict detected"
                );
            }
            if !conflicts.is_empty() {
                tracing::error!(
                    "headless mode: refusing to start adapter; keep this adapter enabled on only one host in the swarm (resolve ownership in setup wizard or by disabling Telegram on other hosts)"
                );
                std::process::exit(1);
            }

            let brave_api_key = brave_api_key.or_else(|| std::env::var("BRAVE_API_KEY").ok());
            let manus_ws_url = Arc::new(manus_ws_url);

            // Register native tool procedures
            let mut procedure_registry = ProcedureRegistry::new();
            procedure_registry.register(Box::new(ReadFileProcedure));
            procedure_registry.register(Box::new(WriteFileProcedure));
            procedure_registry.register(Box::new(EditFileProcedure));
            procedure_registry.register(Box::new(ListDirectoryProcedure));
            procedure_registry.register(Box::new(WebFetchProcedure));
            procedure_registry.register(Box::new(WebSearchProcedure::new(brave_api_key)));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "browser_open",
                Arc::clone(&manus_ws_url),
            )));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "browser_screenshot",
                Arc::clone(&manus_ws_url),
            )));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "browser_click",
                Arc::clone(&manus_ws_url),
            )));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "browser_type",
                Arc::clone(&manus_ws_url),
            )));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "screen_capture",
                Arc::clone(&manus_ws_url),
            )));
            procedure_registry.register(Box::new(ParesManusToolProcedure::new(
                "cdp_execute",
                Arc::clone(&manus_ws_url),
            )));
            let shell_executor = Arc::new(ShellExecutor::new());
            procedure_registry.register(Box::new(RunCommandProcedure {
                executor: Arc::clone(&shell_executor),
            }));
            procedure_registry.register(Box::new(ProcessManageProcedure {
                executor: Arc::clone(&shell_executor),
            }));

            // Create PluresLM for memory tools (shared with agent later)
            let embedder: Box<dyn EmbeddingProvider> = match &embed_url {
                Some(url) => Box::new(OpenAiEmbedder::new(
                    url.clone(),
                    embed_model.clone(),
                    api_key.clone(),
                )),
                None => Box::new(MockEmbedder),
            };
            let plures_lm = Arc::new(PluresLm::new(
                Arc::clone(&store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
                embedder,
                128_000,
            ));
            procedure_registry.register(Box::new(MemorySearchProcedure {
                plures_lm: Arc::clone(&plures_lm),
            }));
            procedure_registry.register(Box::new(MemoryStoreProcedure {
                plures_lm: Arc::clone(&plures_lm),
            }));

            // Initialize praxis write gate
            let write_gate = Arc::new(pares_radix_core::praxis::PraxisWriteGate::new());

            // Initialize plugin framework
            let plugin_runtime = Arc::new(PluginRuntime::new());
            let plugin_executor = Arc::new(PluginCrudExecutor::with_write_gate(
                store.crdt_store_arc(),
                Arc::clone(&write_gate),
            ));

            // Load persisted plugins from PluresDB
            {
                let manifests = plugin_executor.load_persisted_manifests();
                for manifest_json in manifests {
                    if let Ok(manifest) = serde_json::from_value::<
                        pares_radix_core::plugins::PluginManifest,
                    >(manifest_json)
                    {
                        let name = manifest.name.clone();
                        if let Err(e) = plugin_runtime.install(manifest).await {
                            tracing::warn!(plugin = %name, error = %e, "failed to restore persisted plugin");
                        } else {
                            tracing::info!(plugin = %name, "restored persisted plugin");
                        }
                    }
                }
            }

            // Auto-discover and load plugins from plugins/ directory
            {
                let plugins_dir = std::path::Path::new("plugins");
                if plugins_dir.is_dir() {
                    if let Ok(entries) = std::fs::read_dir(plugins_dir) {
                        for entry in entries.flatten() {
                            let manifest_path = entry.path().join("manifest.toml");
                            if manifest_path.is_file() {
                                match std::fs::read_to_string(&manifest_path) {
                                    Ok(toml_str) => {
                                        match plugin_runtime.install_from_toml(&toml_str).await {
                                            Ok(name) => {
                                                tracing::info!(plugin = %name, path = %manifest_path.display(), "auto-loaded plugin from directory");
                                                // Persist to PluresDB so it survives restarts even without the directory
                                                if let Some(manifest) = plugin_runtime.get(&name).await {
                                                    if let Ok(manifest_json) = serde_json::to_value(&manifest) {
                                                        let _ = plugin_executor.persist_manifest(&name, &manifest_json);
                                                    }
                                                }
                                            }
                                            Err(pares_radix_core::plugins::PluginError::AlreadyInstalled(_)) => {
                                                // Already loaded from PluresDB persistence — skip
                                            }
                                            Err(e) => {
                                                tracing::warn!(path = %manifest_path.display(), error = %e, "failed to auto-load plugin");
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(path = %manifest_path.display(), error = %e, "failed to read plugin manifest");
                                    }
                                }
                            }
                        }
                    }
                }
                let loaded = plugin_runtime.list().await;
                tracing::info!(count = loaded.len(), "plugin framework ready");
            }

            // Register plugin CRUD procedures
            for tool_name in &[
                "plugin_create",
                "plugin_list",
                "plugin_update",
                "plugin_delete",
                "plugin_move",
                "plugin_search",
            ] {
                procedure_registry.register(Box::new(PluginCrudProcedure::new(
                    tool_name,
                    Arc::clone(&plugin_executor),
                    Arc::clone(&plugin_runtime),
                )));
            }

            // Load .px procedures from praxis/ directory (live, reactive tree).
            // NOTE: excludes praxis/shadow/ — those are umbra-evolved candidates that
            // must NOT enter the live procedure registry. They are loaded separately,
            // inert, into the ShadowProcedures holder immediately below.
            let px_action_handler =
                Arc::new(pares_radix_core::px_adapter::ToolDispatchActionHandler::new_lazy());
            {
                let praxis_dir = std::path::Path::new("praxis");
                if praxis_dir.is_dir() {
                    let adapters = pares_radix_core::px_adapter::load_px_directory_excluding(
                        praxis_dir,
                        &["shadow"],
                        px_action_handler.clone()
                            as Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>,
                    );
                    if !adapters.is_empty() {
                        tracing::info!(
                            count = adapters.len(),
                            "loaded .px procedures from praxis/ (excluding shadow/)"
                        );
                        for adapter in adapters {
                            procedure_registry.register(Box::new(adapter));
                        }
                    }
                }
            }

            // Load umbra-evolved SHADOW candidates from praxis/shadow/ into the
            // shadow holder. CWD is the daemon WorkingDirectory (/home/kbristol
            // on praxisbot), so this resolves to ~/praxis/shadow — the same tree the
            // nixos service syncs from the package. These declare `trigger: manual`
            // and are loaded OUT-OF-BAND (never into procedure_registry above), so they
            // ship to praxisbot and accumulate fitness for promotion, but never serve
            // live output. See crates/core/src/spine/shadow.rs + praxis/shadow/README.md.
            //
            // Phase A (issue #677): the shadow holder is now RETAINED in shared state
            // (Arc) and exposed via the `shadow_status` procedure, replacing the former
            // load-and-discard dead-end. Full arena wiring (fitness scoring via
            // `umbra_shadow::ShadowArena`, promotion protocol) will land once the
            // `umbra-shadow` dependency's license compatibility is confirmed.
            let shadow_procedures = {
                use pares_radix_core::spine::shadow::ShadowProcedures;
                let shadow_dir = std::path::Path::new("praxis/shadow");
                let mut shadow = ShadowProcedures::new();
                let loaded = shadow.load_dir(
                    shadow_dir,
                    px_action_handler.clone()
                        as Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>,
                );
                if loaded > 0 {
                    tracing::info!(
                        shadow_candidates = loaded,
                        "loaded umbra-evolved shadow candidates from praxis/shadow/ (retained for arena evaluation)"
                    );
                }
                Arc::new(shadow)
            };

            // Create scheduler (shared via Arc for cron tools)
            let scheduler = Arc::new(
                pares_agens_agenda::scheduler::Scheduler::new().with_executor(std::sync::Arc::new(
                    |cmd: String| {
                        tokio::spawn(async move {
                            match tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(&cmd)
                                .output()
                                .await
                            {
                                Ok(output) => {
                                    let stdout = String::from_utf8_lossy(&output.stdout);
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    if output.status.success() {
                                        stdout.to_string()
                                    } else {
                                        format!("EXIT {}: {}\n{}", output.status, stdout, stderr)
                                    }
                                }
                                Err(e) => format!("EXEC ERROR: {e}"),
                            }
                        })
                    },
                )),
            );

            // Register cron tools
            procedure_registry.register(Box::new(CronListProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            procedure_registry.register(Box::new(CronAddProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            procedure_registry.register(Box::new(CronRemoveProcedure {
                scheduler: Arc::clone(&scheduler),
            }));
            procedure_registry.register(Box::new(CronToggleProcedure {
                scheduler: Arc::clone(&scheduler),
            }));

            // Register shadow-status tool (Phase A, issue #677)
            procedure_registry.register(Box::new(ShadowStatusProcedure {
                shadow: Arc::clone(&shadow_procedures),
            }));

            let procedure_registry = Arc::new(procedure_registry);

            let tool_trace_store = ToolTraceStore::default();
            let governor = Arc::new(ToolGovernor::with_defaults());
            // Shared approval registry: the resolve seam is threaded into the
            // Telegram adapter (below) so Allow/Deny presses reach radix-core.
            let approval_registry =
                Arc::new(pares_radix_core::approval::ApprovalRegistry::new());
            let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
                trace_store: tool_trace_store.clone(),
                governor: Arc::clone(&governor),
                plugin_runtime: Some(Arc::clone(&plugin_runtime)),
                approval_registry: Arc::clone(&approval_registry),
            });

            // Complete the lazy initialization of the .px action handler
            px_action_handler.set_dispatcher(Arc::clone(&tool_dispatcher));

            let mut registry = AgentRegistry::new();
            registry.register_builtins();
            let registry = Arc::new(registry);

            // Auto-download BitNet model for orchestrator if not explicitly provided
            #[cfg(feature = "bitnet-native")]
            let cerebellum_model_path = if cerebellum_model_path.is_some() {
                cerebellum_model_path
            } else {
                let model_manager = pares_radix_core::model_download::ModelManager::new();
                match model_manager.ensure_bitnet_model().await {
                    Ok(path) => {
                        tracing::info!(path = %path.display(), "Auto-downloaded BitNet model for orchestrator");
                        Some(path)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "BitNet auto-download failed (will use heuristic classifier): {e}"
                        );
                        None
                    }
                }
            };

            let agent_factory = Arc::new(RuntimeAgentFactory {
                store: Arc::clone(&store),
                model_client: Arc::clone(&model_client),
                deep_model_client: Arc::clone(&deep_model_client),
                fast_model_client: fast_model_client_opt.clone(),
                tool_dispatcher: Arc::clone(&tool_dispatcher),
                registry: Arc::clone(&registry),
                embed_url,
                embed_model: embed_model.clone(),
                api_key: api_key.clone(),
                system_prompt_path: system_prompt_path.clone(),
                cerebellum_model_path: cerebellum_model_path.clone(),
            });
            let agent = match agent_factory.build_agent_with_lm(Arc::clone(&plures_lm)) {
                Ok(agent) => agent,
                Err(e) => {
                    tracing::error!("failed to initialize runtime agent: {e}");
                    std::process::exit(1);
                }
            };
            let agent_handle = Arc::new(RwLock::new(agent));
            // Wire the live agent into the model control so `/status` can read
            // the last-routed tier from the real router.
            *agent_ref_state.write().await = Some(Arc::clone(&*agent_handle.read().await));

            // Inject plugin schema context into agent's system prompt
            {
                let schema_ctx = plugin_runtime.schema_context().await;
                if !schema_ctx.is_empty() {
                    let agent = agent_handle.read().await;
                    agent.set_plugin_context(Some(schema_ctx));
                    tracing::info!("Plugin schema context injected into system prompt");
                }
            }

            // Skip Telegram adapter when no token provided (desktop-only mode)
            if telegram_token.is_empty() {
                tracing::info!("No Telegram token — running in headless/desktop mode");

                if !no_event_spine {
                    let crdt = store.crdt_store();
                    let spine = pares_radix_core::event_spine::EventSpine::new(crdt, "pares-radix");
                    spine.seed_contracts();
                    spine.register_core_procedures();
                    tracing::info!("Event spine initialized");
                }

                if let Err(e) = systemd_notify("READY=1") {
                    tracing::warn!("systemd notify: {e}");
                }

                let memory_monitor = spawn_memory_monitor(env!("GIT_COMMIT_HASH"));
                let watchdog = spawn_systemd_watchdog();
                tokio::signal::ctrl_c().await.ok();
                tracing::info!("Shutdown signal received");
                let _ = systemd_notify("STOPPING=1");
                let hostname = current_hostname();
                let _ = flush_pluresdb_on_shutdown(&store, &hostname, "").await;
                memory_monitor.abort();
                if let Some(h) = watchdog {
                    h.abort();
                }
                return;
            }

            // Set up Telegram adapter
            let telegram_token_for_shutdown = telegram_token.clone();
            let mut config = TelegramConfig::new(telegram_token)
                .with_model_control(runtime_model_control as Arc<dyn TelegramModelControl>)
                .with_runtime_control(Arc::new(RuntimeResetControl {
                    agent: Arc::clone(&agent_handle),
                    factory: Arc::clone(&agent_factory),
                }));
            if let Some(control) = runtime_config_control {
                config = config.with_config_control(control);
            }
            config = config.with_personality_control(Arc::new(RuntimePersonalityControl {
                state_store: Arc::clone(&runtime_state_store),
                agent: Arc::clone(&agent_handle),
            }));
            config = config
                .with_plugin_runtime(Arc::clone(&plugin_runtime), Arc::clone(&plugin_executor));
            config.write_gate = Some(Arc::clone(&write_gate));

            // Task manager for /tasks and /task commands
            let task_manager = Arc::new(pares_radix_core::task_manager::TaskManager::new(
                store.crdt_store_arc(),
            ));
            config = config.with_task_manager(Arc::clone(&task_manager));
            config.tool_count = Some(procedure_registry.len());

            // Initialize ModelPool from config/models.toml
            // Search order covers NixOS deploy, dev, and manual install layouts
            let models_toml = {
                let h = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                let candidates = [
                    std::path::PathBuf::from(&h).join(".pares-radix/config/models.toml"),
                    std::path::PathBuf::from(&h).join(".pares-radix/models.toml"),
                    std::path::PathBuf::from(&h).join("config/models.toml"),
                    std::path::PathBuf::from(&h).join("models.toml"),
                ];
                candidates.into_iter().find(|p| p.exists())
            };
            // Auto-deploy bundled config if none found anywhere
            let models_toml = models_toml.unwrap_or_else(|| {
                let h = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                let target = std::path::PathBuf::from(&h).join(".pares-radix/config/models.toml");
                if let Ok(exe_dir) = std::env::current_exe().map(|p| p.parent().unwrap_or(std::path::Path::new(".")).to_path_buf()) {
                    let bundled = exe_dir.join("config").join("models.toml");
                    if bundled.exists() {
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).ok();
                        }
                        if let Err(e) = std::fs::copy(&bundled, &target) {
                            tracing::warn!(error = %e, "failed to deploy bundled models.toml");
                        } else {
                            tracing::info!(src = %bundled.display(), dst = %target.display(), "deployed bundled models.toml");
                        }
                    }
                }
                target
            });
            if models_toml.exists() {
                match pares_radix_core::model_pool::ModelPool::from_config(&models_toml) {
                    Ok(pool) => {
                        let pool = Arc::new(pool);
                        let pool_for_discovery = Arc::clone(&pool);
                        // Spawn background discovery + periodic refresh (every hour)
                        tokio::spawn(async move {
                            pool_for_discovery.discover_all().await;
                            // Re-discover every hour
                            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
                            interval.tick().await; // first tick fires immediately, skip it
                            loop {
                                interval.tick().await;
                                tracing::debug!("ModelPool: periodic rediscovery starting");
                                pool_for_discovery.discover_all().await;
                            }
                        });
                        let adapter_ctrl = Arc::new(
                            pares_radix_core::model_pool::PoolControlAdapter::new(Arc::clone(&pool)),
                        );
                        config = config.with_pool_control(adapter_ctrl as Arc<dyn pares_radix_core::model_pool::PoolControl>);
                        tracing::info!(config = %models_toml.display(), "ModelPool initialized (hourly refresh enabled)");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to load ModelPool config, falling back to legacy model control");
                    }
                }
            }

            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            // Create streaming broadcast channel — ModelInvoker sends deltas here,
            // TelegramAdapter subscribes for progressive editing. Zero overhead if unused.
            let (stream_broadcast_tx, _) = tokio::sync::broadcast::channel::<pares_radix_core::model::StreamDelta>(256);

            // Initialize the event spine if enabled
            let mut adapter = adapter;
            adapter.stream_tx = Some(stream_broadcast_tx.clone());
            // Share the approval registry so Allow/Deny presses resolve pending
            // tool approvals (#472 block-and-await resolve seam).
            adapter.approval_registry = Some(Arc::clone(&approval_registry));
            let mut heartbeat_spine_handle: Option<pares_radix_core::event_spine::EventSpineHandle> = None;
            if !no_event_spine {
                let crdt = store.crdt_store();
                let spine = pares_radix_core::event_spine::EventSpine::new(crdt, "pares-radix");
                spine.seed_contracts();
                spine.register_core_procedures();
                let handle = pares_radix_core::event_spine::EventSpineHandle::from_arc_store(
                    store.crdt_store_arc(),
                    "pares-radix",
                );
                // Create a second handle for the heartbeat
                heartbeat_spine_handle = Some(
                    pares_radix_core::event_spine::EventSpineHandle::from_arc_store(
                        store.crdt_store_arc(),
                        "pares-radix-heartbeat",
                    ),
                );
                adapter.event_spine = Some(handle);
                tracing::info!("AgensRuntime event spine initialized with core procedures");
                // The spine is stack-local for now — future work will make it
                // accessible from the adapter via Arc.  The important thing is
                // that contracts are seeded and procedures are registered in
                // PluresDB so the data is durable.
            }

            // Seed personality contract into PluresDB state if not present
            {
                use pares_agens_core::personality::{PersonalityContract, PERSONALITY_STATE_KEY};
                let existing = runtime_state_store.get(PERSONALITY_STATE_KEY).await;
                if existing
                    .and_then(|v| serde_json::from_value::<PersonalityContract>(v).ok())
                    .is_none()
                {
                    let default = PersonalityContract::default_contract(None);
                    if let Ok(value) = serde_json::to_value(&default) {
                        runtime_state_store.set(PERSONALITY_STATE_KEY, value).await;
                        tracing::info!("Seeded default personality contract into PluresDB state");
                    }
                }
            }

            // Seed personality documents from ~/.pares-radix/ directory
            {
                use pares_agens_core::personality::{
                    format_documents_for_prompt, get_all_documents, seed_from_directory,
                };
                if let Ok(home) = std::env::var("HOME") {
                    let config_dir = std::path::PathBuf::from(&home).join(".pares-radix");
                    if config_dir.exists() {
                        seed_from_directory(runtime_state_store.as_ref(), &config_dir).await;
                    }
                }
                // Load documents and cache in agent
                let docs = get_all_documents(runtime_state_store.as_ref()).await;
                if !docs.is_empty() {
                    let formatted = format_documents_for_prompt(&docs);
                    agent_handle
                        .read()
                        .await
                        .set_personality_documents(Some(formatted));
                    tracing::info!(
                        count = docs.len(),
                        "loaded personality documents into agent"
                    );
                    for doc in &docs {
                        tracing::info!("  {} ({} chars)", doc.doc_type, doc.content.len());
                    }
                }
            }

            scheduler.add(crate::self_update::self_update_task_from_env()).await;
            tracing::info!("Registered scheduled NixOS self-update task");

            // Spawn scheduler loop
            let scheduler_handle = Arc::clone(&scheduler);
            tokio::spawn(async move {
                scheduler_handle.start().await;
            });
            tracing::info!("Scheduler started");

            // Spawn heartbeat runner
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            {
                let heartbeat_store: Arc<dyn pares_radix_core::state::StateStore> =
                    Arc::new(pares_radix_core::state::InMemoryStateStore::default());
                let mut heartbeat =
                    pares_agens_core::heartbeat::HeartbeatRunner::new(Arc::clone(&heartbeat_store))
                        .with_task_manager(Arc::clone(&task_manager), Arc::clone(&heartbeat_store));
                if let Some(spine_handle) = heartbeat_spine_handle {
                    heartbeat = heartbeat.with_event_spine(spine_handle);
                }
                heartbeat.load_config().await;
                // Disable quiet hours if env var says so
                if std::env::var("PARES_HEARTBEAT_NO_QUIET").is_ok() {
                    let mut cfg = heartbeat.config().clone();
                    cfg.quiet_hours_enabled = false;
                    heartbeat.set_config(cfg).await;
                    tracing::info!("heartbeat quiet hours disabled");
                }
                tokio::spawn(async move {
                    heartbeat.run(shutdown_rx).await;
                });
                tracing::info!("Heartbeat runner started (with task manager + event spine)");
            }

            let memory_monitor = spawn_memory_monitor(env!("GIT_COMMIT_HASH"));
            let watchdog = spawn_systemd_watchdog();

            // Spawn autonomous task dispatch loop (IO boundary for autonomous-dispatch.px)
            //
            // Decision logic lives in praxis/procedures/autonomous-dispatch.px.
            // This Rust code is the IO boundary ONLY: it reads the dispatch decision
            // and performs the side-effect (inject event into agent).
            //
            // TODO: Route through PxBridge.call("evaluate_dispatch", {tick}) once
            // PxBridge is available in the serve path. Until then, this is a minimal
            // Rust fallback that mirrors the .px contracts (cooldown, max_attempts,
            // priority sort).
            //
            // Channel-independent: calls agent.handle_event() directly.
            // Works regardless of which channel adapter (Telegram, Discord, stdin)
            // is running alongside.
            let task_dispatch_shutdown = shutdown_tx.subscribe();
            {
                const DISPATCH_INTERVAL_SECS: u64 = 60;
                const MAX_ATTEMPTS: u32 = 5;
                const COOLDOWN_MS: u64 = 60_000;

                let agent_for_tasks = Arc::clone(&agent_handle);
                let tm_for_dispatch = Arc::clone(&task_manager);
                tokio::spawn(async move {
                    let mut shutdown = task_dispatch_shutdown;
                    let mut interval = tokio::time::interval(
                        std::time::Duration::from_secs(DISPATCH_INTERVAL_SECS),
                    );
                    interval.tick().await; // skip first immediate tick
                    // Track last dispatch time per task (mirrors .px cooldown contract)
                    let mut last_dispatched: std::collections::HashMap<String, std::time::Instant> =
                        std::collections::HashMap::new();

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                let mut tasks = tm_for_dispatch.evaluable_tasks();
                                if tasks.is_empty() {
                                    continue;
                                }

                                // Filter: cooldown (mirrors .px filter_ready_tasks)
                                let now = std::time::Instant::now();
                                tasks.retain(|t| {
                                    last_dispatched.get(&t.id).map_or(true, |last| {
                                        now.duration_since(*last).as_millis() as u64 > COOLDOWN_MS
                                    })
                                });

                                // Filter: max attempts (mirrors .px filter_retriable)
                                tasks.retain(|t| t.attempts < MAX_ATTEMPTS);

                                if tasks.is_empty() {
                                    continue;
                                }

                                // Select: highest priority (lowest number), then oldest
                                // (mirrors .px select_best_task)
                                tasks.sort_by(|a, b| {
                                    a.priority.cmp(&b.priority)
                                        .then(a.created_at.cmp(&b.created_at))
                                });

                                let task = &tasks[0];

                                // Build prompt (mirrors .px build_task_prompt)
                                let prompt = format!(
                                    "[autonomous-task] Execute this task:\n\
                                    Task: {}\n\
                                    ID: {}\n\
                                    Priority: {}\n\
                                    Attempts: {}\n\n\
                                    Work on this task using available tools. \
                                    When complete, call task_complete.",
                                    task.description, task.id, task.priority, task.attempts
                                );

                                // IO boundary: inject as internal event (channel-agnostic)
                                let event = pares_radix_core::event::Event::Message {
                                    id: format!("task-dispatch-{}", task.id),
                                    channel: "internal".into(),
                                    content: prompt,
                                    sender: "task_dispatcher".into(),
                                };

                                let task_id = task.id.clone();
                                let agent = agent_for_tasks.read().await.clone();
                                if let Some(response) = agent.handle_event(event).await {
                                    if let pares_radix_core::event::Event::Message { content, .. } = &response {
                                        tracing::info!(
                                            task_id = %task_id,
                                            response_len = content.len(),
                                            "autonomous task dispatched and processed"
                                        );
                                    }
                                } else {
                                    tracing::debug!(task_id = %task_id, "task dispatch produced no response");
                                }

                                // Record dispatch time (cooldown tracking)
                                last_dispatched.insert(task_id, std::time::Instant::now());
                            }
                            _ = shutdown.changed() => {
                                tracing::info!("task dispatch loop shutting down");
                                break;
                            }
                        }
                    }
                });
                tracing::info!("Autonomous task dispatch loop started (channel-independent, 60s interval)");
            }

            let adapter_result =
                run_adapter_with_recovery(&adapter, Arc::clone(&agent_handle), tool_trace_store, Some(stream_broadcast_tx))
                    .await;

            // Stop heartbeat
            let _ = shutdown_tx.send(true);
            if let Err(e) = systemd_notify("STOPPING=1") {
                tracing::warn!("failed to send systemd STOPPING=1: {e}");
            }

            if let Err(e) =
                flush_pluresdb_on_shutdown(&store, &hostname, &telegram_token_for_shutdown).await
            {
                tracing::warn!("{e}");
            }

            memory_monitor.abort();
            if let Some(handle) = watchdog {
                handle.abort();
            }

            let uptime_secs = started_at.elapsed().as_secs();
            if let Some(rss_kib) = current_process_rss_kib() {
                tracing::info!(
                    uptime_secs,
                    memory_rss_kib = rss_kib,
                    "daemon shutdown complete"
                );
            } else {
                tracing::info!(uptime_secs, "daemon shutdown complete");
            }

            if let Err(e) = adapter_result {
                tracing::error!("{e}");
                std::process::exit(1);
            }
        }

pub(crate) async fn run_tui(
    model_url: String,
    model: String,
    copilot: bool,
    api_key: Option<String>,
    system_prompt: Option<std::path::PathBuf>,
    bitnet_model_path: Option<std::path::PathBuf>,
    cerebellum_model_path: Option<std::path::PathBuf>,
) {
    let radix_config = super::config::RadixConfig::load();

            use crossterm::{
                event::{self as ct_event, Event as CtEvent, KeyCode, KeyEventKind, KeyModifiers},
                execute,
                terminal::{
                    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
                },
            };
            use pares_agens_tui::app::{App, AppEvent};
            use ratatui::backend::CrosstermBackend;
            use ratatui::Terminal;

            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let mut model = model;

            // Apply config file defaults
            if model == "claude-sonnet-4.5" {
                model = radix_config.model.primary.clone();
            }
            let copilot = copilot || radix_config.model.copilot;

            // Build model client
            let model_name_handle = Arc::new(RwLock::new(model.clone()));
            let model_client: Arc<dyn ModelClient> = if let Some(ref bitnet_path) =
                bitnet_model_path
            {
                tracing::info!(path = %bitnet_path.display(), "using local BitNet model (TUI)");
                Arc::new(BitnetModelClient::new(bitnet_path))
            } else if copilot {
                let auth_path = PathBuf::from(&home).join(".pares-radix/copilot-auth.json");
                let cached = std::fs::read_to_string(&auth_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<CopilotAuthCache>(&raw).ok())
                    .filter(|cache| {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs();
                        if cache.cached_at > 0 && now.saturating_sub(cache.cached_at) > 30 * 86400 {
                            tracing::info!("Copilot OAuth token is >30 days old, forcing re-auth");
                            let _ = std::fs::remove_file(&auth_path);
                            return false;
                        }
                        true
                    });

                let oauth_token = if let Some(cache) = cached {
                    cache.oauth_token
                } else {
                    let (device_code, user_code, verification_uri) =
                        match CopilotAuth::device_flow_start().await {
                            Ok(response) => response,
                            Err(e) => {
                                eprintln!("Copilot device flow failed: {e}");
                                std::process::exit(1);
                            }
                        };

                    println!(
                        "Authorize Copilot: visit {verification_uri} and enter code {user_code}"
                    );

                    let token = match CopilotAuth::device_flow_poll(&device_code).await {
                        Ok(token) => token,
                        Err(e) => {
                            eprintln!("Copilot polling failed: {e}");
                            std::process::exit(1);
                        }
                    };

                    if let Some(parent) = auth_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Ok(serialized) = serde_json::to_string_pretty(&CopilotAuthCache {
                        oauth_token: token.clone(),
                        cached_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    }) {
                        let _ = std::fs::write(&auth_path, serialized);
                    }
                    token
                };

                let auth = CopilotAuth::new(oauth_token);
                Arc::new(
                    CopilotModelClient::new_with_model_handle(auth, Arc::clone(&model_name_handle)),
                )
            } else {
                let provider_config = ProviderConfig::new(&model_url, api_key.clone());
                let router_config = RouterConfig::single("default", provider_config);
                let model_router = Arc::new(ModelRouter::new(router_config));
                Arc::new(RouterModelClient {
                    router: Arc::new(RwLock::new(model_router)),
                    model: Arc::clone(&model_name_handle),
                    endpoint: Arc::new(RwLock::new(model_url.clone())),
                    api_key: api_key.clone(),
                })
            };

            // Set up terminal FIRST to show loading screens
            enable_raw_mode().expect("failed to enable raw mode");
            let mut stdout = std::io::stdout();
            execute!(stdout, EnterAlternateScreen).expect("failed to enter alternate screen");
            let backend = CrosstermBackend::new(stdout);
            let mut terminal = Terminal::new(backend).expect("failed to create terminal");
            terminal.clear().expect("failed to clear terminal");

            // Show initial loading screen
            let _ = terminal.draw(|f| {
                use ratatui::layout::{Alignment, Constraint, Direction, Layout};
                use ratatui::style::{Color, Style};
                use ratatui::widgets::{Block, Borders, Paragraph};

                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Length(5),
                        Constraint::Percentage(40),
                    ])
                    .split(area);

                let block = Block::default()
                    .title(" pares-radix ")
                    .borders(Borders::ALL)
                    .style(Style::default().fg(Color::Cyan));
                let text = Paragraph::new(
                    "Initializing...\n\n(First launch downloads 127MB embedding model)",
                )
                .block(block)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White));
                f.render_widget(text, chunks[1]);
            });

            // Build memory + agent
            let memory_path = PathBuf::from(&home).join(".pares-radix/memory");
            // Update loading screen: opening memory store
            let _ = terminal.draw(|f| {
                use ratatui::layout::{Alignment, Constraint, Direction, Layout};
                use ratatui::style::{Color, Style};
                use ratatui::widgets::{Block, Borders, Paragraph};

                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Length(5),
                        Constraint::Percentage(40),
                    ])
                    .split(area);

                let block = Block::default()
                    .title(" pares-radix ")
                    .borders(Borders::ALL)
                    .style(Style::default().fg(Color::Cyan));
                let text = Paragraph::new(
                    "Loading memory store...\n\nBuilding vector index (this may take a moment)",
                )
                .block(block)
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::White));
                f.render_widget(text, chunks[1]);
            });

            let store: Arc<PluresDbStore> = match PluresDbStore::open_with_embeddings(&memory_path)
            {
                Ok(store) => Arc::new(store),
                Err(_) => match PluresDbStore::open(&memory_path) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        // DB locked by serve process — fall back to in-memory store
                        // so the TUI can still function for chat without persistent memory.
                        tracing::warn!(
                            "Memory DB locked (serve running?), using ephemeral memory: {e}"
                        );
                        Arc::new(PluresDbStore::in_memory())
                    }
                },
            };

            let plures_lm = Arc::new(PluresLm::new(
                Arc::clone(&store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
                Box::new(MockEmbedder),
                128_000,
            ));
            let memory = Arc::new(PluresMemory {
                plures_lm: Arc::clone(&plures_lm),
            });

            // Tools
            let mut procedure_registry = ProcedureRegistry::new();
            procedure_registry.register(Box::new(ReadFileProcedure));
            procedure_registry.register(Box::new(WriteFileProcedure));
            procedure_registry.register(Box::new(EditFileProcedure));
            procedure_registry.register(Box::new(ListDirectoryProcedure));
            let shell_executor = Arc::new(ShellExecutor::new());
            procedure_registry.register(Box::new(RunCommandProcedure {
                executor: Arc::clone(&shell_executor),
            }));
            procedure_registry.register(Box::new(ProcessManageProcedure {
                executor: Arc::clone(&shell_executor),
            }));
            procedure_registry.register(Box::new(WebFetchProcedure));
            procedure_registry.register(Box::new(MemorySearchProcedure {
                plures_lm: Arc::clone(&plures_lm),
            }));
            procedure_registry.register(Box::new(MemoryStoreProcedure {
                plures_lm: Arc::clone(&plures_lm),
            }));

            // Load .px procedures from praxis/ directory (TUI mode)
            let px_action_handler =
                Arc::new(pares_radix_core::px_adapter::ToolDispatchActionHandler::new_lazy());
            {
                let praxis_dir = std::path::Path::new("praxis");
                if praxis_dir.is_dir() {
                    let adapters = pares_radix_core::px_adapter::load_px_directory(
                        praxis_dir,
                        px_action_handler.clone()
                            as Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>,
                    );
                    if !adapters.is_empty() {
                        tracing::info!(
                            count = adapters.len(),
                            "loaded .px procedures from praxis/"
                        );
                        for adapter in adapters {
                            procedure_registry.register(Box::new(adapter));
                        }
                    }
                }
            }

            let procedure_registry = Arc::new(procedure_registry);
            let governor = Arc::new(ToolGovernor::with_defaults());
            let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
                trace_store: ToolTraceStore::default(),
                governor: Arc::clone(&governor),
                plugin_runtime: None,
                // TUI mode has no interactive-card adapter yet; give it its own
                // registry so the struct is complete. Resolve routing is a no-op here.
                approval_registry: Arc::new(pares_radix_core::approval::ApprovalRegistry::new()),
            });

            // Complete lazy initialization of .px action handler (TUI mode)
            px_action_handler.set_dispatcher(Arc::clone(&tool_dispatcher));

            // Update loading screen: building agent
            let _ = terminal.draw(|f| {
                use ratatui::layout::{Alignment, Constraint, Direction, Layout};
                use ratatui::style::{Color, Style};
                use ratatui::widgets::{Block, Borders, Paragraph};

                let area = f.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(40),
                        Constraint::Length(5),
                        Constraint::Percentage(40),
                    ])
                    .split(area);

                let block = Block::default()
                    .title(" pares-radix ")
                    .borders(Borders::ALL)
                    .style(Style::default().fg(Color::Cyan));
                let text = Paragraph::new("Building agent...\n\nInitializing tools and orchestrator")
                    .block(block)
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::White));
                f.render_widget(text, chunks[1]);
            });

            // Auto-download BitNet for orchestrator if not explicitly provided
            let _cerebellum_model_path = if cerebellum_model_path.is_some() {
                cerebellum_model_path.clone()
            } else {
                let model_manager = pares_radix_core::model_download::ModelManager::new();
                match model_manager.ensure_bitnet_model().await {
                    Ok(path) => {
                        tracing::info!(path = %path.display(), "Auto-downloaded BitNet model for orchestrator (TUI)");
                        Some(path)
                    }
                    Err(e) => {
                        tracing::warn!(
                            "BitNet auto-download failed (will use heuristic classifier): {e}"
                        );
                        None
                    }
                }
            };

            let orchestrator = Orchestrator::new(CerebellumConfig::default());
            #[cfg(feature = "bitnet-native")]
            let orchestrator = if let Some(ref path) = cerebellum_model_path {
                match super::bitnet_classifier::BitNetClassifier::new(path) {
                    Ok(backend) => {
                        let classifier = pares_agens_core::orchestrator::classifier::CerebellumClassifier::with_backend(
                            Arc::new(backend),
                            vec![],
                        );
                        tracing::info!("orchestrator classifier enabled (BitNet)");
                        orchestrator.with_classifier(classifier)
                    }
                    Err(e) => {
                        tracing::warn!("BitNet classifier load failed: {e}, using heuristic");
                        let classifier = pares_agens_core::orchestrator::classifier::CerebellumClassifier::heuristic_only(vec![]);
                        orchestrator.with_classifier(classifier)
                    }
                }
            } else {
                orchestrator
            };

            // Load .px procedures for orchestrator routing/classification (serve-spine)
            let orchestrator = {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default();
                let px_dir = std::path::PathBuf::from(&home)
                    .join(".pares-radix")
                    .join("praxis")
                    .join("procedures");
                let bridge = Arc::new(PxBridge::new(Arc::new(
                    pares_agens_core::orchestrator::actions::CerebellumActionHandler::new_minimal(),
                )));
                let loaded = bridge.load_from_directory_sync(&px_dir);
                if loaded > 0 {
                    tracing::info!(count = loaded, dir = %px_dir.display(), "px_bridge: loaded orchestrator procedures (spine)");
                    orchestrator.with_px_bridge(bridge)
                } else {
                    let local_dir = std::path::PathBuf::from("praxis/procedures");
                    let loaded_local = bridge.load_from_directory_sync(&local_dir);
                    if loaded_local > 0 {
                        tracing::info!(
                            count = loaded_local,
                            "px_bridge: loaded orchestrator procedures (local/spine)"
                        );
                        orchestrator.with_px_bridge(bridge)
                    } else {
                        tracing::debug!(
                            "px_bridge: no .px procedures found (spine), using Rust fallback"
                        );
                        orchestrator
                    }
                }
            };

            // Load dataflow procedures (queue-driven, no triggers) for serve-spine
            let orchestrator = {
                use pares_agens_core::orchestrator::dataflow_bridge::DataflowBridge;
                use pares_radix_praxis::dataflow::{ast_to_node, parse_px};

                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_default();
                let px_dir = std::path::PathBuf::from(&home)
                    .join(".pares-radix")
                    .join("praxis")
                    .join("procedures");
                let local_dir = std::path::PathBuf::from("praxis/procedures");

                let mut df_bridge = DataflowBridge::new(Arc::new(
                pares_agens_core::orchestrator::dataflow_bridge::DataflowActionAdapter::new(
                    Arc::new(pares_agens_core::orchestrator::actions::CerebellumActionHandler::new_minimal()),
                ),
            ));
                let mut df_count = 0usize;
                let mut px_parse_failures: Vec<(std::path::PathBuf, String)> = Vec::new();

                for dir in [&px_dir, &local_dir] {
                    if !dir.exists() {
                        continue;
                    }
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|e| e.to_str()) != Some("px") {
                                continue;
                            }
                            if let Ok(source) = std::fs::read_to_string(&path) {
                                match parse_px(&source) {
                                    Ok(doc) => {
                                    for proc in doc.statements.iter().filter_map(|s| match s {
                                        pares_radix_praxis::px::Statement::DataflowProcedure(p) => {
                                            Some(p)
                                        }
                                        _ => None,
                                    }) {
                                        let node = ast_to_node(proc);
                                        let name = node.name.clone();
                                        let rt = tokio::runtime::Handle::current();
                                        let result = tokio::task::block_in_place(|| {
                                            rt.block_on(df_bridge.register(node))
                                        });
                                        if let Err(e) = result {
                                            tracing::warn!(name = %name, error = %e, "dataflow: failed to register (spine)");
                                        } else {
                                            df_count += 1;
                                        }
                                    }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            file = %path.display(),
                                            error = %e,
                                            "px_loader: FAILED to parse procedure file (spine) - this policy file is NOT active"
                                        );
                                        px_parse_failures.push((path.clone(), e.to_string()));
                                    }
                                }
                            } else {
                                tracing::error!(
                                    file = %path.display(),
                                    "px_loader: FAILED to read procedure file (spine) - this policy file is NOT active"
                                );
                            }
                        }
                    }
                }

                if !px_parse_failures.is_empty() {
                    tracing::error!(
                        count = px_parse_failures.len(),
                        files = ?px_parse_failures.iter().map(|(p, _)| p.display().to_string()).collect::<Vec<_>>(),
                        "px_loader: {} .px procedure file(s) failed to parse (spine) and are NOT active",
                        px_parse_failures.len()
                    );
                }

                if df_count > 0 {
                    tracing::info!(count = df_count, "dataflow_bridge: loaded procedures (spine)");
                    orchestrator.with_dataflow_bridge(Arc::new(df_bridge))
                } else {
                    orchestrator
                }
            };

            // Shared Chronos timeline: attached to BOTH the Orchestrator (so
            // autorecall emits real `recall_query` operations, ADR-0019 4.3)
            // and the Agent (tool execution auditing).
            let chronos = Arc::new(pares_radix_core::chronos::ChronosTimeline::with_jsonl_from_env(
                store.crdt_store_arc(),
            ));
            let orchestrator = orchestrator.with_chronos(Arc::clone(&chronos));

            let system_prompt_text = build_system_prompt(system_prompt).unwrap_or_else(|e| {
                eprintln!("Warning: {e}");
                "You are Pares Radix, an AI assistant. Be direct and helpful.".to_string()
            });

            let mut registry = pares_agens_core::delegation::registry::AgentRegistry::new();
            registry.register_builtins();

            let agent = Arc::new(
                Agent::with_cerebellum(memory, orchestrator, plures_lm)
                    .with_model(
                        Arc::clone(&model_client),
                        Arc::clone(&tool_dispatcher),
                        system_prompt_text,
                    )
                    .with_turn_store(
                        Arc::clone(&store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>
                    )
                    .with_chronos(chronos),
            );

            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
            let mut app = App::new(agent, model.clone(), event_tx);

            // Wire session persistence via PluresDbStateStore
            {
                let state_path = PathBuf::from(&home).join(".pares-radix/state");
                let state_store: Arc<dyn pares_radix_core::StateStore> =
                    match PluresDbStateStore::open(&state_path) {
                        Ok(s) => Arc::new(s),
                        Err(_) => Arc::new(pares_radix_core::InMemoryStateStore::new()),
                    };
                let session_mgr =
                    Arc::new(pares_radix_core::session::SessionManager::new(state_store));
                app = app.with_session_manager(session_mgr);
                app.load_persisted_sessions();
            }

            // Restore conversation history from PluresDB for display continuity
            {
                use pares_agens_core::memory::store::MemoryStore;
                let channel = "tui";
                match store.recent_turns(channel, 50).await {
                    Ok(turns) if !turns.is_empty() => {
                        let display_turns: Vec<(String, String, String)> = turns
                            .into_iter()
                            .flat_map(|t| {
                                let ts = t.timestamp.clone();
                                t.messages.into_iter().filter_map(move |m| {
                                    let role = m.role.clone();
                                    if role == "system" || m.content.trim().is_empty() {
                                        None
                                    } else {
                                        Some((role, m.content, ts.clone()))
                                    }
                                })
                            })
                            .collect();
                        if !display_turns.is_empty() {
                            app.load_history_from_turns(display_turns);
                            tracing::info!(
                                count = app.messages.len(),
                                "restored TUI conversation history"
                            );
                        }
                    }
                    Ok(_) => {} // no prior turns
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to load conversation history for TUI");
                    }
                }
            }

            // Main loop
            let result: Result<(), Box<dyn std::error::Error>> = 'main_loop: loop {
                app.viewport_height = terminal
                    .size()
                    .map(|r| r.height.saturating_sub(6))
                    .unwrap_or(35);
                match terminal.draw(|f| pares_agens_tui::ui::draw(f, &app)) {
                    Ok(_) => {}
                    Err(e) => break 'main_loop Err(e.into()),
                }

                // Poll for crossterm events with a short timeout
                let has_event = match ct_event::poll(std::time::Duration::from_millis(50)) {
                    Ok(v) => v,
                    Err(e) => break 'main_loop Err(e.into()),
                };
                if has_event {
                    let event = match ct_event::read() {
                        Ok(v) => v,
                        Err(e) => break 'main_loop Err(e.into()),
                    };
                    if let CtEvent::Key(key) = event {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        // Handle Ctrl+<key> shortcuts first
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            match key.code {
                                KeyCode::Char('c') => {
                                    break 'main_loop Ok(());
                                }
                                KeyCode::Char('l') => {
                                    app.clear_chat();
                                }
                                KeyCode::Char('u') => {
                                    app.clear_input();
                                }
                                KeyCode::Char('w') => {
                                    app.delete_word_backward();
                                }
                                _ => {}
                            }
                            continue;
                        }
                        // Alt+Enter inserts a newline for multi-line input
                        if key.modifiers.contains(KeyModifiers::ALT) && key.code == KeyCode::Enter {
                            app.insert_newline();
                            continue;
                        }
                        // Alt+1..9 switches to session by index
                        if key.modifiers.contains(KeyModifiers::ALT) {
                            if let KeyCode::Char(c @ '1'..='9') = key.code {
                                let idx = (c as u8 - b'1') as usize;
                                app.switch_to_index(idx);
                                continue;
                            }
                        }
                        match key.code {
                            KeyCode::Enter => {
                                app.submit_input();
                            }
                            KeyCode::Char(c) => {
                                // Clamp cursor to valid char boundary
                                let cursor = app.input_cursor.min(app.input.len());
                                app.input.insert(cursor, c);
                                app.input_cursor = cursor + c.len_utf8();
                            }
                            KeyCode::Backspace if app.input_cursor > 0 => {
                                // Find previous char boundary
                                let new_cursor = app.input[..app.input_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map(|(i, _)| i)
                                    .unwrap_or(0);
                                app.input.remove(new_cursor);
                                app.input_cursor = new_cursor;
                            }
                            KeyCode::Left if app.input_cursor > 0 => {
                                app.input_cursor = app.input[..app.input_cursor]
                                    .char_indices()
                                    .next_back()
                                    .map(|(i, _)| i)
                                    .unwrap_or(0);
                            }
                            KeyCode::Right if app.input_cursor < app.input.len() => {
                                app.input_cursor = app.input[app.input_cursor..]
                                    .char_indices()
                                    .nth(1)
                                    .map(|(i, _)| app.input_cursor + i)
                                    .unwrap_or(app.input.len());
                            }
                            KeyCode::Home => {
                                app.input_cursor = 0;
                            }
                            KeyCode::End => {
                                app.input_cursor = app.input.len();
                            }
                            KeyCode::PageUp => {
                                app.scroll_offset = app.scroll_offset.saturating_add(5);
                                app.user_scrolled = true;
                            }
                            KeyCode::PageDown => {
                                if app.scroll_offset > 5 {
                                    app.scroll_offset -= 5;
                                } else {
                                    app.scroll_offset = 0;
                                    app.user_scrolled = false;
                                }
                            }
                            KeyCode::Up => {
                                app.history_up();
                            }
                            KeyCode::Down => {
                                app.history_down();
                            }
                            KeyCode::Esc => {
                                break 'main_loop Ok(());
                            }
                            _ => {}
                        }
                    }
                }

                // Drain app events (non-blocking) — MUST be outside the key-event block
                // so agent responses are picked up even when no key is pressed.
                while let Ok(ev) = event_rx.try_recv() {
                    match ev {
                        AppEvent::StreamChunk(chunk) => {
                            app.handle_stream_chunk(chunk);
                        }
                        AppEvent::AgentResponse(content) => {
                            app.handle_agent_response(content);
                            // Auto-persist session after each response
                            app.persist_current_session();
                        }
                        AppEvent::Quit => {
                            // Persist before quitting
                            app.persist_current_session();
                            break 'main_loop Ok(());
                        }
                        AppEvent::Redraw => {}
                        AppEvent::UserInput(_) => {}
                        AppEvent::SessionsLoaded(sessions) => {
                            app.handle_sessions_loaded(sessions);
                        }
                        AppEvent::SessionMessagesLoaded(name, turns) => {
                            app.handle_session_messages_loaded(name, turns);
                        }
                    }
                }
            };

            // Restore terminal
            disable_raw_mode().expect("failed to disable raw mode");
            execute!(terminal.backend_mut(), LeaveAlternateScreen)
                .expect("failed to leave alternate screen");
            terminal.show_cursor().expect("failed to show cursor");

            if let Err(e) = result {
                eprintln!("TUI error: {e}");
                std::process::exit(1);
            }
        }

pub(crate) async fn run_ask(
    prompt: String,
    model: String,
    copilot: bool,
    bitnet_model_path: Option<std::path::PathBuf>,
    system_prompt: Option<std::path::PathBuf>,
    format: String,
) {
    let radix_config = super::config::RadixConfig::load();

            use std::io::Write;
            let start = std::time::Instant::now();
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

            // Apply config file defaults
            let mut model = model;
            if model == "claude-sonnet-4.5" {
                model = radix_config.model.primary.clone();
            }
            let copilot = copilot || radix_config.model.copilot;

            let sys_prompt = system_prompt
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_else(|| "You are a helpful assistant. Be concise.".into());

            type CM = pares_radix_core::model::ChatMessage;
            let messages: Vec<CM> = vec![
                CM {
                    role: "system".into(),
                    content: sys_prompt.clone(),
                    tool_call_id: None,
                    tool_calls: None,
                },
                CM {
                    role: "user".into(),
                    content: prompt.clone(),
                    tool_call_id: None,
                    tool_calls: None,
                },
            ];

            // Build model client
            if let Some(ref path) = bitnet_model_path {
                let client = BitnetModelClient::new(path);
                let mc: Arc<dyn ModelClient> = Arc::new(client);
                match mc
                    .complete(
                        &messages[..],
                        &[],
                        &pares_radix_core::model::ChatOptions::default(),
                    )
                    .await
                {
                    Ok(resp) => {
                        let elapsed = start.elapsed();
                        if format == "json" {
                            println!(
                                "{}",
                                serde_json::json!({"response": resp.content.unwrap_or_default(), "model": "bitnet", "latency_ms": elapsed.as_millis(), "prompt": prompt})
                            );
                        } else {
                            print!("{}", resp.content.unwrap_or_default());
                            std::io::stdout().flush().ok();
                        }
                    }
                    Err(e) => {
                        eprintln!("ERROR: {e}");
                        std::process::exit(1);
                    }
                }
            } else if copilot {
                let auth_path = PathBuf::from(&home).join(".pares-radix/copilot-auth.json");
                let cached = std::fs::read_to_string(&auth_path)
                    .ok()
                    .and_then(|raw| serde_json::from_str::<CopilotAuthCache>(&raw).ok());
                let oauth_token = match cached {
                    Some(c) => c.oauth_token,
                    None => {
                        eprintln!("No cached Copilot auth. Run 'pares-radix tui --copilot' first.");
                        std::process::exit(1);
                    }
                };
                let auth = CopilotAuth::new(oauth_token);
                let client = CopilotModelClient::new(auth, model.clone());
                let mc: Arc<dyn ModelClient> = Arc::new(client);
                match mc
                    .complete(
                        &messages[..],
                        &[],
                        &pares_radix_core::model::ChatOptions::default(),
                    )
                    .await
                {
                    Ok(resp) => {
                        let elapsed = start.elapsed();
                        if format == "json" {
                            println!(
                                "{}",
                                serde_json::json!({"response": resp.content.unwrap_or_default(), "model": model, "latency_ms": elapsed.as_millis(), "prompt": prompt})
                            );
                        } else {
                            print!("{}", resp.content.unwrap_or_default());
                            std::io::stdout().flush().ok();
                        }
                    }
                    Err(e) => {
                        eprintln!("ERROR: {e}");
                        std::process::exit(1);
                    }
                }
            } else {
                eprintln!("ERROR: specify --copilot or --bitnet-model-path");
                std::process::exit(1);
            }
        }

#[cfg(feature = "bitnet-native")]
pub(crate) async fn run_classify(message: String, bitnet_model_path: std::path::PathBuf) {

            use super::bitnet_classifier::BitNetClassifier;
            use pares_agens_core::orchestrator::classifier::ClassifierBackend;

            let start = std::time::Instant::now();

            match BitNetClassifier::new(&bitnet_model_path) {
                Ok(classifier) => {
                    let elapsed_load = start.elapsed();
                    eprintln!("Model loaded in {:.1}s", elapsed_load.as_secs_f64());

                    let class_start = std::time::Instant::now();
                    match classifier.classify("", &message) {
                        Ok(json) => {
                            let elapsed = class_start.elapsed();
                            eprintln!("Classification took {:.0}ms", elapsed.as_millis());
                            println!("{json}");
                        }
                        Err(e) => {
                            eprintln!("Classification failed: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Failed to load classifier: {e}");
                    std::process::exit(1);
                }
            }
        }

#[cfg(test)]
mod tests {
    use super::*;
    use pares_radix_core::model::{ModelClientError, ModelCompletion, ToolCall, ToolDefinition};

    #[test]
    fn spine_commands_are_channel_independent_and_skip_model_invocation() {
        let status = spine_command_reply("/status", "gpt-test", 13)
            .expect("status must be claimed by the shared command gate");
        assert!(status.contains("gpt-test"));
        assert!(status.contains("13 registered"));

        let help = spine_command_reply("/commands", "gpt-test", 13)
            .expect("commands must be claimed by the shared command gate");
        assert!(help.contains("/status"));

        assert!(spine_command_reply("ordinary conversation", "gpt-test", 13).is_none());
    }

    struct TestModelClient;

    #[async_trait]
    impl ModelClient for TestModelClient {
        async fn complete(
            &self,
            _messages: &[CoreChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, ModelClientError> {
            Ok(ModelCompletion {
                content: Some("ok".to_string()),
                tool_calls: Vec::<ToolCall>::new(),
                logprobs: None,
                model: None,
            })
        }
    }

    struct TestToolDispatcher;

    #[async_trait]
    impl ToolDispatcher for TestToolDispatcher {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            vec![]
        }

        async fn call_tool(&self, _name: &str, _arguments: serde_json::Value) -> String {
            String::new()
        }
    }

    #[tokio::test]
    async fn task_graph_tool_creates_durable_child_and_blocks_parent_completion() {
        use pluresdb::{CrdtStore, MemoryStorage, StorageEngine};

        let storage: Arc<dyn StorageEngine> = Arc::new(MemoryStorage::default());
        let store = Arc::new(CrdtStore::default().with_persistence(storage));
        let task_manager = Arc::new(TaskManager::new(store));
        let parent = task_manager.create_task("Ship the feature", "chat-1", vec![]);
        let dispatcher = TaskGraphToolDispatcher::new(
            Arc::new(TestToolDispatcher),
            Arc::clone(&task_manager),
        );

        let created: serde_json::Value = serde_json::from_str(
            &dispatcher
                .call_tool(
                    TaskGraphToolDispatcher::CREATE_SUBTASK,
                    serde_json::json!({
                        "parent_task_id": &parent.id[..8],
                        "description": "Add the durable task graph tool",
                        "completion_conditions": ["A regression test passes"]
                    }),
                )
                .await,
        )
        .unwrap();
        assert_eq!(created["status"], "created");
        assert_eq!(created["parent_task_id"], parent.id);
        let child_id = created["task_id"].as_str().unwrap();

        let persisted_parent = task_manager.get_task(&parent.id).unwrap();
        let persisted_child = task_manager.get_task(child_id).unwrap();
        assert_eq!(persisted_parent.subtasks, vec![child_id.to_string()]);
        assert_eq!(persisted_child.parent_task.as_deref(), Some(parent.id.as_str()));

        let blocked: serde_json::Value = serde_json::from_str(
            &dispatcher
                .call_tool("task_complete", serde_json::json!({"task_id": parent.id}))
                .await,
        )
        .unwrap();
        assert_eq!(blocked["status"], "error");
        assert_eq!(blocked["outstanding_subtask_ids"], serde_json::json!([child_id]));

        task_manager.complete_task(child_id, Some("child shipped"));
        assert!(dispatcher
            .completion_is_blocked(&serde_json::json!({"task_id": parent.id}))
            .is_none());
    }


    #[tokio::test]
    async fn web_search_procedure_calls_brave_and_parses_results() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        let brave_response = serde_json::json!({
            "web": {
                "results": [
                    {
                        "title": "Example Result",
                        "url": "https://example.com",
                        "description": "An example description"
                    }
                ]
            }
        });

        Mock::given(method("GET"))
            .and(path("/res/v1/web/search"))
            .and(header("X-Subscription-Token", "test-key"))
            .and(query_param("q", "rust programming"))
            .and(query_param("count", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(brave_response))
            .mount(&mock_server)
            .await;

        let procedure = WebSearchProcedure::with_base_url(
            Some("test-key".to_string()),
            format!("{}/res/v1/web/search", mock_server.uri()),
        );

        let event = Event::Message {
            id: Uuid::new_v4().to_string(),
            channel: "tool".into(),
            sender: "model".into(),
            content: serde_json::json!({"query": "rust programming", "count": 3}).to_string(),
        };

        let events = procedure.execute(&event).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::ToolResult {
                tool_name,
                content,
                is_error,
                ..
            } => {
                assert_eq!(tool_name, "web_search");
                assert!(!is_error, "expected success, got error content: {content}");
                assert!(content.contains("Example Result"));
                assert!(content.contains("https://example.com"));
            }
            _ => panic!("expected ToolResult event"),
        }
    }

    #[tokio::test]
    async fn web_search_procedure_errors_without_api_key() {
        let procedure = WebSearchProcedure::new(None);

        let event = Event::Message {
            id: Uuid::new_v4().to_string(),
            channel: "tool".into(),
            sender: "model".into(),
            content: serde_json::json!({"query": "rust programming"}).to_string(),
        };

        let events = procedure.execute(&event).await;
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::ToolResult {
                is_error, content, ..
            } => {
                assert!(*is_error);
                assert!(content.contains("BRAVE_API_KEY"));
            }
            _ => panic!("expected ToolResult event"),
        }
    }

    #[test]
    fn relocated_self_update_task_from_env_builds_interval_task() {
        // The self-update command/task builders were relocated into
        // `crate::self_update` (Stage R2). Verify the host wiring still resolves
        // and produces a valid interval task via the relocated module. The
        // command-shape assertions live in `crate::self_update`'s own tests.
        let task = crate::self_update::build_self_update_task(
            ".",
            "praxisbot",
            crate::self_update::DEFAULT_SELF_UPDATE_INTERVAL_SECS,
        );
        assert_eq!(task.id, "self-update.rebuild");
        assert!(task.enabled);
        match task.schedule {
            pares_agens_agenda::scheduler::Schedule::Interval { every_secs } => {
                assert_eq!(
                    every_secs,
                    crate::self_update::DEFAULT_SELF_UPDATE_INTERVAL_SECS
                );
            }
            _ => panic!("expected interval schedule"),
        }
    }


    #[tokio::test]
    async fn runtime_model_control_persists_primary_model_override() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_radix_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4.1".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
            fast_model: Arc::new(RwLock::new(String::new())),
            available_models: Arc::new(RwLock::new(Vec::new())),
            agent_ref: Arc::new(RwLock::new(None)),
            deep_escalation_enabled: Arc::new(RwLock::new(true)),
            state_store: Arc::clone(&state_store),
        };

        control.set_primary_model("gpt-4o").await.unwrap();

        assert_eq!(
            control.current_models().await,
            ("gpt-4o".to_string(), "claude-opus-4.6".to_string())
        );
        assert_eq!(
            state_store.get(MODEL_OVERRIDE_STATE_KEY).await,
            Some(serde_json::json!({
                "model": "gpt-4o",
                "deep_model": "claude-opus-4.6",
                "deep_escalation_enabled": true
            }))
        );
    }

    #[tokio::test]
    async fn runtime_model_control_persists_deep_model_override() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_radix_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
            fast_model: Arc::new(RwLock::new(String::new())),
            available_models: Arc::new(RwLock::new(Vec::new())),
            agent_ref: Arc::new(RwLock::new(None)),
            deep_escalation_enabled: Arc::new(RwLock::new(true)),
            state_store: Arc::clone(&state_store),
        };

        control.set_deep_model("claude-sonnet-4.5").await.unwrap();

        assert_eq!(
            control.current_models().await,
            ("gpt-4o".to_string(), "claude-sonnet-4.5".to_string())
        );
        assert_eq!(
            state_store.get(MODEL_OVERRIDE_STATE_KEY).await,
            Some(serde_json::json!({
                "model": "gpt-4o",
                "deep_model": "claude-sonnet-4.5",
                "deep_escalation_enabled": true
            }))
        );
    }

    #[tokio::test]
    async fn runtime_model_control_persists_deep_escalation_toggle() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_radix_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
            fast_model: Arc::new(RwLock::new(String::new())),
            available_models: Arc::new(RwLock::new(Vec::new())),
            agent_ref: Arc::new(RwLock::new(None)),
            deep_escalation_enabled: Arc::new(RwLock::new(true)),
            state_store: Arc::clone(&state_store),
        };

        control.set_deep_escalation_enabled(false).await.unwrap();

        assert!(!control.deep_escalation_enabled().await);
        assert_eq!(
            state_store.get(MODEL_OVERRIDE_STATE_KEY).await,
            Some(serde_json::json!({
                "model": "gpt-4o",
                "deep_model": "claude-opus-4.6",
                "deep_escalation_enabled": false
            }))
        );
    }


    #[tokio::test]
    async fn runtime_config_control_persists_model_endpoint_and_log_level() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_radix_core::InMemoryStateStore::new());
        let runtime_model_control = Arc::new(RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
            fast_model: Arc::new(RwLock::new(String::new())),
            available_models: Arc::new(RwLock::new(Vec::new())),
            agent_ref: Arc::new(RwLock::new(None)),
            deep_escalation_enabled: Arc::new(RwLock::new(true)),
            state_store: Arc::clone(&state_store),
        });
        let provider_config = ProviderConfig::new("http://localhost:11434/v1", None);
        let router_config = RouterConfig::single("default", provider_config);
        let primary_client = Arc::new(RouterModelClient {
            router: Arc::new(RwLock::new(Arc::new(ModelRouter::new(router_config)))),
            model: Arc::clone(&runtime_model_control.primary_model),
            endpoint: Arc::new(RwLock::new("http://localhost:11434/v1".to_string())),
            api_key: None,
        });
        let (_layer, log_filter_handle) =
            tracing_subscriber::reload::Layer::new(build_env_filter("info").unwrap());
        let control = RuntimeConfigControl {
            model_control: Arc::clone(&runtime_model_control),
            primary_client: Arc::clone(&primary_client),
            state_store: Arc::clone(&state_store),
            log_level: Arc::new(RwLock::new("info".to_string())),
            log_filter_handle,
        };

        control.set_model("gpt-4.1").await.unwrap();
        control
            .set_endpoint("https://models.inference.ai.azure.com")
            .await
            .unwrap();

        let config = control.current_config().await;
        assert_eq!(config.model, "gpt-4.1");
        assert_eq!(config.endpoint, "https://models.inference.ai.azure.com");
        assert_eq!(config.log_level, "info");
        assert_eq!(
            state_store.get(RUNTIME_CONFIG_OVERRIDE_STATE_KEY).await,
            Some(serde_json::json!({
                "model": "gpt-4.1",
                "endpoint": "https://models.inference.ai.azure.com",
                "log_level": "info"
            }))
        );
    }

    #[tokio::test]
    async fn runtime_reset_control_rebuilds_agent_instance() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = Arc::new(PluresDbStore::open(temp_dir.path()).expect("open pluresdb store"));

        let mut registry = AgentRegistry::new();
        registry.register_builtins();

        let model_client: Arc<dyn ModelClient> = Arc::new(TestModelClient);
        let deep_model_client: Arc<dyn ModelClient> = Arc::new(TestModelClient);
        let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(TestToolDispatcher);

        let factory = Arc::new(RuntimeAgentFactory {
            store,
            model_client,
            deep_model_client,
            fast_model_client: None,
            tool_dispatcher,
            registry: Arc::new(registry),
            embed_url: None,
            embed_model: "nomic-embed-text".to_string(),
            api_key: None,
            system_prompt_path: None,
            cerebellum_model_path: None,
        });

        let first_agent = factory.build_agent().expect("build initial agent");
        let first_ptr = Arc::as_ptr(&first_agent);
        let agent = Arc::new(RwLock::new(first_agent));
        let control = RuntimeResetControl {
            agent: Arc::clone(&agent),
            factory,
        };

        control.reset_runtime().await.expect("reset runtime");

        let second_agent = agent.read().await.clone();
        assert!(
            !std::ptr::eq(first_ptr, Arc::as_ptr(&second_agent)),
            "reset should replace the live agent instance"
        );
    }
}
