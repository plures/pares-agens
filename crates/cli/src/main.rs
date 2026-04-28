//! `pares-agens` CLI binary.
//!
//! # Usage
//!
//! ```text
//! pares-agens migrate [--from ~/.openclaw] [--output ./migration] [--dry-run]
//! pares-agens serve --telegram-token <TOKEN> [--model-url <URL>] [--model <MODEL>]
//! ```

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use clap::{Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

use reqwest::header::{HeaderMap, HeaderValue};

use pares_agens_channels::adapter::ChannelAdapter;
use pares_agens_channels::telegram::{
    TelegramAdapter, TelegramConfig, TelegramConfigControl, TelegramModelControl,
    TelegramPersonalityControl, TelegramRuntimeConfig, TelegramRuntimeControl,
    TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER,
};
use pares_agens_core::agent::{Agent, Memory};
use pares_agens_core::auth::copilot::{CopilotAuth, CopilotModelClient};
use pares_agens_core::cerebellum::{Cerebellum, CerebellumConfig};
use pares_agens_core::delegation::{broker::DelegationBroker, registry::AgentRegistry};
use pares_agens_core::memory::{
    embed::{EmbeddingProvider, MockEmbedder, OpenAiEmbedder},
    entry::Exchange,
    store::{HostAdapterConfig, HostAdapterRecord, PluresDbStore},
    PluresLm,
};
use pares_agens_core::model::{
    ChatMessage as CoreChatMessage, ChatOptions, ModelClient, ToolDefinition, ToolDispatcher,
};
use pares_agens_core::procedure::{Procedure, ProcedureRegistry};
use pares_agens_core::plugins::{PluginCrudExecutor, PluginRuntime};
use pares_agens_core::tool_governance::{GovernanceVerdict, ToolGovernor};
use pares_agens_core::Event;
use pares_agens_core::{PluresDbStateStore, StateStore};
use pares_agens_migrate::{migrate, openclaw};
use pares_models::config::{ProviderConfig, RouterConfig};
use pares_models::router::ModelRouter;
use pares_models::types::{ChatCompletionRequest, ChatMessage, Role, Tool};

#[derive(Debug, Parser)]
#[command(
    name = "pares-agens",
    version,
    about = "Pares Agens agent runtime CLI",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

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

impl ToggleableModelClient {
    fn new(inner: Arc<dyn ModelClient>, enabled: Arc<RwLock<bool>>) -> Self {
        Self { inner, enabled }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct CopilotAuthCache {
    oauth_token: String,
}

const MODEL_OVERRIDE_STATE_KEY: &str = "agent.runtime_model_override";
const RUNTIME_CONFIG_OVERRIDE_STATE_KEY: &str = "agent.runtime_config_override";
/// Maximum characters shown for tool-call argument previews in `/verbose`.
const VERBOSE_TOOL_ARGS_PREVIEW_CHARS: usize = 240;
/// Maximum characters shown for tool-call result previews in `/verbose`.
const VERBOSE_TOOL_RESULT_PREVIEW_CHARS: usize = 500;

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
    deep_escalation_enabled: Arc<RwLock<bool>>,
    state_store: Arc<dyn StateStore>,
}

fn default_deep_escalation_enabled() -> bool {
    true
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
    tool_dispatcher: Arc<dyn ToolDispatcher>,
    registry: Arc<AgentRegistry>,
    embed_url: Option<String>,
    embed_model: String,
    api_key: Option<String>,
    system_prompt_path: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct ToolCallTrace {
    tool_name: String,
    arguments: serde_json::Value,
    result: String,
    is_error: bool,
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

fn normalize_log_level(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(value.trim().to_ascii_lowercase()),
        _ => Err("log level must be one of: trace, debug, info, warn, error".to_string()),
    }
}

fn build_env_filter(level: &str) -> Result<EnvFilter, String> {
    let level = normalize_log_level(level)?;
    let directive = level
        .parse()
        .map_err(|e| format!("failed to parse '{level}' as tracing directive: {e}"))?;
    Ok(EnvFilter::from_default_env().add_directive(directive))
}

fn apply_runtime_log_level(
    handle: &tracing_subscriber::reload::Handle<EnvFilter, tracing_subscriber::Registry>,
    level: &str,
) -> Result<String, String> {
    let normalized = normalize_log_level(level)?;
    let filter = build_env_filter(&normalized)?;
    handle
        .reload(filter)
        .map_err(|e| format!("failed to reload log filter: {e}"))?;
    Ok(normalized)
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

    fn load_system_prompt(&self) -> Result<String, String> {
        build_system_prompt(self.system_prompt_path.clone())
    }

    fn build_agent(&self) -> Result<Arc<Agent>, String> {
        let plures_lm = Arc::new(PluresLm::new(
            Arc::clone(&self.store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>,
            self.build_embedder(),
            128_000,
        ));
        let memory = Arc::new(PluresMemory {
            plures_lm: Arc::clone(&plures_lm),
        });
        let cerebellum = Cerebellum::new(CerebellumConfig::default());
        let system_prompt = self.load_system_prompt()?;

        // Create default personality contract. Runtime seeding into PluresDB
        // happens in the async serve path.
        let personality = pares_agens_core::personality::PersonalityContract::default_contract(None);
        let delegation_broker = DelegationBroker::new(
            Arc::clone(&self.registry),
            Arc::clone(&self.model_client),
            Arc::clone(&self.tool_dispatcher),
        );
        let turn_store: Arc<dyn pares_agens_core::memory::store::MemoryStore> = self.store.clone();

        Ok(Arc::new(
            Agent::with_cerebellum(memory, cerebellum, plures_lm)
                .with_model(
                    Arc::clone(&self.model_client),
                    Arc::clone(&self.tool_dispatcher),
                    system_prompt,
                )
                .with_deep_model(Arc::clone(&self.deep_model_client))
                .with_delegation(delegation_broker)
                .with_turn_store(turn_store)
                .with_personality(personality),
        ))
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
        use pares_agens_core::personality::{BehaviorRule, PersonalityContract, PERSONALITY_STATE_KEY};
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
            Some(doc) => format!("## {} (updated: {})\n{}", doc.doc_type, doc.updated_at, doc.content),
            None => format!("No '{doc_type}' document found."),
        }
    }

    async fn set_document(&self, doc_type: &str, content: &str) -> Result<(), String> {
        use pares_agens_core::personality::{store_document, get_all_documents, format_documents_for_prompt, PERSONALITY_DOC_TYPES};
        if !PERSONALITY_DOC_TYPES.contains(&doc_type) {
            return Err(format!("Unknown document type '{}'. Valid types: {:?}", doc_type, PERSONALITY_DOC_TYPES));
        }
        store_document(self.state_store.as_ref(), doc_type, content).await;
        // Update agent cache
        let docs = get_all_documents(self.state_store.as_ref()).await;
        let formatted = format_documents_for_prompt(&docs);
        self.agent.read().await.set_personality_documents(Some(formatted));
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
    ) -> Result<pares_agens_core::model::ModelCompletion, String> {
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
                            .map(|call| pares_models::types::ToolCall {
                                id: call.id,
                                kind: "function".into(),
                                function: pares_models::types::FunctionCall {
                                    name: call.name,
                                    arguments: call.arguments.to_string(),
                                },
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
        let response = router.chat(&request).await.map_err(|e| e.to_string())?;

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
            .map(|call| pares_agens_core::model::ToolCall {
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

        Ok(pares_agens_core::model::ModelCompletion {
            content: choice.message.content.clone(),
            tool_calls,
            logprobs,
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
    ) -> Result<pares_agens_core::model::ModelCompletion, String> {
        if !*self.enabled.read().await {
            return Err("deep model escalation is disabled".to_string());
        }
        self.inner.complete(messages, tools, options).await
    }
}

struct ProcedureToolDispatcher {
    registry: Arc<ProcedureRegistry>,
    trace_store: ToolTraceStore,
    governor: Arc<ToolGovernor>,
    plugin_runtime: Option<Arc<PluginRuntime>>,
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
                let result = format!("Command blocked by policy: matched blocked pattern \"{}\".", pattern);
                self.trace_store
                    .record_for_current_request(name, &args_for_trace, &result, true)
                    .await;
                return result;
            }
            GovernanceVerdict::AllowWithApprovalWarning => {
                tracing::info!(tool = name, "tool execution proceeding with approval warning (Phase 5+)");
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
        tracing::debug!(tool = name, elapsed_ms = elapsed.as_millis() as u64, "tool execution completed");
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

/// Detect and strip the Telegram verbose marker from inbound content.
fn extract_verbose_tool_marker(content: &str) -> (bool, String) {
    match content.strip_prefix(TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER) {
        Some(stripped) => (true, stripped.to_string()),
        None => (false, content.to_string()),
    }
}

/// Truncate verbose previews to keep Telegram replies within practical limits.
fn truncate_verbose_preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// Format request-scoped tool traces for inline Telegram `/verbose` output.
fn format_verbose_tool_traces(traces: &[ToolCallTrace]) -> String {
    use std::fmt::Write;

    if traces.is_empty() {
        return "Tool execution details:\n(no tool calls made)".to_string();
    }

    let mut output = String::from("Tool execution details:");
    for (idx, trace) in traces.iter().enumerate() {
        let status = if trace.is_error { "error" } else { "ok" };
        let args = truncate_verbose_preview(
            &trace.arguments.to_string(),
            VERBOSE_TOOL_ARGS_PREVIEW_CHARS,
        );
        let result = truncate_verbose_preview(&trace.result, VERBOSE_TOOL_RESULT_PREVIEW_CHARS);
        let _ = write!(
            output,
            "\n{}. {} [{}]\nargs: {}\nresult: {}",
            idx + 1,
            trace.tool_name,
            status,
            args,
            result
        );
    }
    output
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
struct RunCommandProcedure;
struct EditFileProcedure;
struct ListDirectoryProcedure;
struct WebFetchProcedure;
struct WebSearchProcedure {
    brave_api_key: Option<String>,
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
                    Ok(args) => match args.get("command").and_then(|v| v.as_str()) {
                        Some(command) => {
                            // Default 30s timeout — the governance layer may
                            // override this, but RunCommandProcedure also
                            // enforces its own as a safety net.
                            let timeout_secs: u64 = std::env::var("PARES_CMD_TIMEOUT_SECS")
                                .ok()
                                .and_then(|v| v.parse().ok())
                                .unwrap_or(30);
                            let timeout_dur = Duration::from_secs(timeout_secs);

                            let mut child = match tokio::process::Command::new("sh")
                                .arg("-c")
                                .arg(command)
                                .stdout(std::process::Stdio::piped())
                                .stderr(std::process::Stdio::piped())
                                .kill_on_drop(true)
                                .spawn()
                            {
                                Ok(child) => child,
                                Err(e) => return vec![Event::ToolResult {
                                    tool_call_id: id.clone(),
                                    tool_name: "run_command".into(),
                                    content: format!("failed to spawn command: {e}"),
                                    is_error: true,
                                }],
                            };

                            match tokio::time::timeout(timeout_dur, child.wait()).await {
                                Ok(Ok(status)) => {
                                    let mut stdout_buf = Vec::new();
                                    let mut stderr_buf = Vec::new();
                                    if let Some(mut out) = child.stdout.take() {
                                        let _ = tokio::io::AsyncReadExt::read_to_end(&mut out, &mut stdout_buf).await;
                                    }
                                    if let Some(mut err) = child.stderr.take() {
                                        let _ = tokio::io::AsyncReadExt::read_to_end(&mut err, &mut stderr_buf).await;
                                    }
                                    let stdout = String::from_utf8_lossy(&stdout_buf);
                                    let stderr = String::from_utf8_lossy(&stderr_buf);
                                    let code = status
                                        .code()
                                        .map(|c| c.to_string())
                                        .unwrap_or_else(|| "signal".into());
                                    Ok(format!(
                                        "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
                                        code, stdout, stderr
                                    ))
                                }
                                Ok(Err(e)) => Err(format!("command I/O error: {e}")),
                                Err(_) => {
                                    // Timeout — kill the child process (kill_on_drop also covers this)
                                    let _ = child.kill().await;
                                    Err(format!(
                                        "command timed out after {timeout_secs}s and was killed"
                                    ))
                                }
                            }
                        }
                        None => Err("missing 'command'".into()),
                    },
                    Err(e) => Err(e),
                };

                vec![Event::ToolResult {
                    tool_call_id: id.clone(),
                    tool_name: "run_command".into(),
                    content: result.clone().unwrap_or_else(|e| e),
                    is_error: result.is_err(),
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
                            let response = reqwest::get(url).await.map_err(|e| e.to_string());
                            match response {
                                Ok(response) => {
                                    match response.text().await.map_err(|e| e.to_string()) {
                                        Ok(body) => {
                                            let truncated = if body.len() > 10_000 {
                                                body.chars().take(10_000).collect::<String>()
                                            } else {
                                                body
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
                                            .get("https://api.search.brave.com/res/v1/web/search")
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

fn value_to_tool_content(value: &serde_json::Value) -> String {
    value
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| value.to_string())
}

fn manus_request_for_tool(
    tool_name: &str,
    args: serde_json::Value,
) -> Result<(&'static str, serde_json::Value), String> {
    match tool_name {
        "browser_open" => {
            let url = args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'url'".to_string())?;
            Ok(("browser.open", serde_json::json!({ "url": url })))
        }
        "browser_screenshot" => Ok(("browser.screenshot", serde_json::json!({}))),
        "browser_click" => {
            let x = args
                .get("x")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "missing 'x'".to_string())?;
            let y = args
                .get("y")
                .and_then(|v| v.as_i64())
                .ok_or_else(|| "missing 'y'".to_string())?;
            Ok(("gui.click", serde_json::json!({ "x": x, "y": y })))
        }
        "browser_type" => {
            let text = args
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'text'".to_string())?;
            Ok(("gui.type", serde_json::json!({ "text": text })))
        }
        "screen_capture" => {
            let monitor = args.get("monitor").and_then(|v| v.as_u64());
            let window = args.get("window").and_then(|v| v.as_str());
            let mut params = serde_json::Map::new();
            if let Some(monitor) = monitor {
                params.insert("monitor".to_string(), serde_json::Value::from(monitor));
            }
            if let Some(window) = window {
                params.insert("window".to_string(), serde_json::Value::from(window));
            }
            Ok(("screen.capture", serde_json::Value::Object(params)))
        }
        "cdp_execute" => {
            let script = args
                .get("script")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'script'".to_string())?;
            Ok(("cdp.execute", serde_json::json!({ "script": script })))
        }
        _ => Err(format!("unsupported pares-manus tool '{tool_name}'")),
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
                let fields = args
                    .get("fields")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
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
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as usize;
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
                let fields = args
                    .get("fields")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
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
                self.executor
                    .delete(entity_id)
                    .map_err(|e| e.to_string())?;
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
                let limit = args
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;
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

fn parse_tool_args(raw: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(raw).map_err(|e| format!("invalid tool arguments: {e}"))
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
            description: "Fetch a URL and return the response body (truncated)".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"}
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
            description: "Run a shell command and return stdout/stderr".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
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

    // Auto-discover: check $HOME/.pares-agens/SYSTEM-PROMPT.md
    if let Ok(home) = std::env::var("HOME") {
        let home_prompt = PathBuf::from(&home).join(".pares-agens/SYSTEM-PROMPT.md");
        if home_prompt.exists() {
            tracing::info!("Loading system prompt from {}", home_prompt.display());
            return std::fs::read_to_string(&home_prompt)
                .map_err(|e| format!("failed to read {}: {e}", home_prompt.display()));
        }
    }

    // Built-in fallback
    Ok("You are Pares Agens, an AI agent built on the plures technology stack. Be direct, use tools proactively, and push commits without asking.".to_string())
}

fn parse_sync_topic_key(raw: &str) -> Result<[u8; 32], String> {
    let trimmed = raw.trim();
    let value = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    if value.len() != 64 {
        return Err("sync topic key must be 64 hex characters (32 bytes)".to_string());
    }

    let mut key = [0u8; 32];
    for i in 0..32 {
        let pair = &value[(i * 2)..(i * 2 + 2)];
        key[i] = u8::from_str_radix(pair, 16)
            .map_err(|_| format!("invalid hex byte at position {}: {pair}", i * 2))?;
    }
    Ok(key)
}

const ADAPTER_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1200);
const ADAPTER_DISCOVERY_INTERVAL: Duration = Duration::from_millis(200);
const TELEGRAM_RECONNECT_MAX_ATTEMPTS: u32 = 8;
const TELEGRAM_RECONNECT_BASE_DELAY_SECS: u64 = 2;
const TELEGRAM_RECONNECT_MAX_DELAY_SECS: u64 = 30;
const MEMORY_MONITOR_INTERVAL_SECS: u64 = 60;
const DEFAULT_NIX_FLAKE_DIR: &str = ".";
const DEFAULT_NIX_HOST: &str = "praxisbot";
const DEFAULT_SELF_UPDATE_INTERVAL_SECS: u64 = 3600;
const MANUS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MANUS_RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq)]
struct SingleConnectionConflict {
    kind: String,
    connection_id: String,
    hosts: Vec<String>,
}

fn sanitize_hostname(raw: &str) -> String {
    let mut value = String::new();
    let mut prev_underscore = false;
    for c in raw.trim().chars() {
        let mapped = if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            c
        } else {
            '_'
        };
        if mapped == '_' {
            if prev_underscore {
                continue;
            }
            prev_underscore = true;
        } else {
            prev_underscore = false;
        }
        value.push(mapped);
    }
    value = value.trim_matches('_').to_string();
    if value.is_empty() {
        value = "unknown-host".to_string();
    }
    value
}

fn current_hostname() -> String {
    if let Ok(value) = std::env::var("HOSTNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    if let Ok(value) = std::env::var("COMPUTERNAME") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    #[cfg(unix)]
    if let Ok(value) = std::fs::read_to_string("/etc/hostname") {
        let clean = sanitize_hostname(&value);
        if clean != "unknown-host" {
            return clean;
        }
    }
    "unknown-host".to_string()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn build_nixos_update_command(flake_dir: &str, host: &str) -> String {
    let flake_dir = shell_single_quote(flake_dir);
    let host = shell_single_quote(host);
    format!(
        "set -eu; cd {flake_dir}; lock_before=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); sudo nix flake update pares-agens; lock_after=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); if [ \"$lock_before\" != \"$lock_after\" ]; then sudo nixos-rebuild switch --flake .#{host}; echo \"Self-update applied\"; else echo \"No new pares-agens commits on main\"; fi"
    )
}

fn build_self_update_task(
    flake_dir: &str,
    host: &str,
    interval_secs: u64,
) -> pares_agens_agenda::scheduler::Task {
    pares_agens_agenda::scheduler::Task {
        id: "self-update.nixos-rebuild".to_string(),
        name: "Self-update via NixOS rebuild".to_string(),
        schedule: pares_agens_agenda::scheduler::Schedule::Interval {
            every_secs: interval_secs,
        },
        command: build_nixos_update_command(flake_dir, host),
        enabled: true,
        last_run: None,
        last_result: None,
    }
}

fn self_update_task_from_env() -> pares_agens_agenda::scheduler::Task {
    let flake_dir =
        std::env::var("PARES_NIX_FLAKE_DIR").unwrap_or_else(|_| DEFAULT_NIX_FLAKE_DIR.into());
    let host = std::env::var("PARES_NIX_HOST").unwrap_or_else(|_| DEFAULT_NIX_HOST.into());
    let interval = std::env::var("PARES_SELF_UPDATE_INTERVAL_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_SELF_UPDATE_INTERVAL_SECS);

    build_self_update_task(&flake_dir, &host, interval)
}

fn parse_vm_rss_kib(contents: &str) -> Option<u64> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with("VmRSS:") {
            return None;
        }
        line.split_whitespace().nth(1)?.parse::<u64>().ok()
    })
}

fn current_process_rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        parse_vm_rss_kib(&status)
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn parse_watchdog_ping_interval(watchdog_usec: &str) -> Option<Duration> {
    let micros = watchdog_usec.trim().parse::<u64>().ok()?;
    if micros == 0 {
        return None;
    }
    let half = micros / 2;
    let ping_interval_micros = std::cmp::max(half, 1_000_000);
    Some(Duration::from_micros(ping_interval_micros))
}

#[cfg(unix)]
fn systemd_notify(state: &str) -> Result<(), String> {
    use std::os::unix::net::UnixDatagram;

    let notify_socket = match std::env::var("NOTIFY_SOCKET") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => return Ok(()),
    };

    let sock = UnixDatagram::unbound().map_err(|e| format!("sd_notify socket failed: {e}"))?;
    if notify_socket.starts_with('@') {
        return Err("abstract NOTIFY_SOCKET is not supported in this build".to_string());
    }

    sock.send_to(state.as_bytes(), &notify_socket)
        .map_err(|e| format!("sd_notify send failed: {e}"))?;

    Ok(())
}

#[cfg(not(unix))]
fn systemd_notify(_state: &str) -> Result<(), String> {
    Ok(())
}

fn spawn_memory_monitor() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(MEMORY_MONITOR_INTERVAL_SECS));
        loop {
            interval.tick().await;
            if let Some(rss_kib) = current_process_rss_kib() {
                tracing::info!(memory_rss_kib = rss_kib, commit = env!("GIT_COMMIT_HASH"), "process memory usage");
            }
        }
    })
}

fn spawn_systemd_watchdog() -> Option<tokio::task::JoinHandle<()>> {
    let watchdog_usec = std::env::var("WATCHDOG_USEC").ok()?;
    let ping_interval = parse_watchdog_ping_interval(&watchdog_usec)?;

    if let Err(e) = systemd_notify("READY=1") {
        tracing::warn!("failed to send systemd READY=1: {e}");
    }

    Some(tokio::spawn(async move {
        let mut interval = tokio::time::interval(ping_interval);
        loop {
            interval.tick().await;
            if let Err(e) = systemd_notify("WATCHDOG=1") {
                tracing::warn!("failed to send systemd WATCHDOG=1: {e}");
            }
        }
    }))
}

async fn run_adapter_with_recovery(
    adapter: &TelegramAdapter,
    agent: Arc<RwLock<Arc<Agent>>>,
    trace_store: ToolTraceStore,
) -> Result<(), String> {
    let mut attempts = 0u32;
    loop {
        let agent_clone = Arc::clone(&agent);
        let trace_store = trace_store.clone();
        match adapter
            .run(move |mut event: Event| {
                let agent = Arc::clone(&agent_clone);
                let trace_store = trace_store.clone();
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
                        ACTIVE_TELEGRAM_REQUEST_ID
                            .scope(request_id, async { agent.handle_event(event).await })
                            .await
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

fn detect_single_connection_conflicts(
    local_host: &str,
    records: &[HostAdapterRecord],
) -> Vec<SingleConnectionConflict> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut owners: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for record in records {
        for adapter in &record.adapters {
            if !adapter.single_connection || adapter.connection_id.trim().is_empty() {
                continue;
            }
            owners
                .entry((adapter.kind.clone(), adapter.connection_id.clone()))
                .or_default()
                .insert(record.host.clone());
        }
    }

    owners
        .into_iter()
        .filter_map(|((kind, connection_id), hosts)| {
            if hosts.len() < 2 || !hosts.contains(local_host) {
                return None;
            }
            Some(SingleConnectionConflict {
                kind,
                connection_id,
                hosts: hosts.into_iter().collect(),
            })
        })
        .collect()
}

fn redact_connection_id(value: &str) -> String {
    let len = value.chars().count();
    if len <= 8 {
        return "********".to_string();
    }
    let start: String = value.chars().take(4).collect();
    let end: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{start}…{end}")
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Migrate data from an existing OpenClaw installation.
    Migrate {
        /// Path to the OpenClaw installation directory.
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Directory to write migrated output files.
        #[arg(long, value_name = "PATH", default_value = "migration")]
        output: PathBuf,

        /// Simulate the migration without writing any files.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run the agent as a headless daemon with a channel adapter.
    Serve {
        /// Telegram bot token (from BotFather).
        #[arg(long, env = "PARES_TELEGRAM_TOKEN")]
        telegram_token: String,

        /// OpenAI-compatible API URL (GitHub Models or OpenAI compatible endpoint).
        #[arg(
            long,
            env = "PARES_MODEL_URL",
            default_value = "https://models.inference.ai.azure.com"
        )]
        model_url: String,

        /// Model name to use.
        #[arg(long, env = "PARES_MODEL", default_value = "gpt-4o")]
        model: String,

        /// Use GitHub Copilot device flow authentication.
        #[arg(long)]
        copilot: bool,

        /// Deep model name used for low-confidence escalation.
        #[arg(long, env = "PARES_DEEP_MODEL", default_value = "gpt-4.1")]
        deep_model: String,

        /// Deep model API URL (defaults to --model-url).
        #[arg(long, env = "PARES_DEEP_MODEL_URL")]
        deep_model_url: Option<String>,

        /// API key for the model provider.
        #[arg(long, env = "PARES_API_KEY")]
        api_key: Option<String>,

        /// Optional OpenAI-compatible embeddings endpoint.
        #[arg(long, env = "PARES_EMBED_URL")]
        embed_url: Option<String>,

        /// Embeddings model name.
        #[arg(long, env = "PARES_EMBED_MODEL", default_value = "nomic-embed-text")]
        embed_model: String,

        /// Path to a system prompt file.
        #[arg(long, value_name = "PATH")]
        system_prompt: Option<PathBuf>,

        /// Brave Search API key (falls back to BRAVE_API_KEY env var).
        #[arg(long, env = "BRAVE_API_KEY")]
        brave_api_key: Option<String>,

        /// Pares Manus WebSocket endpoint for browser/GUI automation tools.
        #[arg(
            long,
            env = "PARES_MANUS_WS_URL",
            default_value = "ws://127.0.0.1:18790"
        )]
        manus_ws_url: String,

        /// 32-byte Hyperswarm sync topic key (hex) for multi-host replication.
        #[arg(long, env = "PARES_SYNC_TOPIC_KEY")]
        sync_topic_key: Option<String>,

        /// Shared SEA key (base64url-encoded SeaKeyPair JSON) required to decrypt sync payloads.
        #[arg(long, env = "PARES_SYNC_SHARED_KEY")]
        sync_shared_key: Option<String>,

        /// Disable the AgensRuntime event spine for message delivery.
        ///
        /// The event spine is enabled by default.  Inbound messages are
        /// emitted through the event spine and channel contracts are seeded
        /// into PluresDB.  Pass this flag to disable it.
        #[arg(long, env = "PARES_NO_EVENT_SPINE")]
        no_event_spine: bool,
    },

    /// Run the agent with an interactive terminal UI.
    Tui {
        /// OpenAI-compatible API URL.
        #[arg(
            long,
            env = "PARES_MODEL_URL",
            default_value = "https://models.inference.ai.azure.com"
        )]
        model_url: String,

        /// Model name to use.
        #[arg(long, env = "PARES_MODEL", default_value = "gpt-4.1")]
        model: String,

        /// Use GitHub Copilot device flow authentication.
        #[arg(long)]
        copilot: bool,

        /// API key for the model provider.
        #[arg(long, env = "PARES_API_KEY")]
        api_key: Option<String>,

        /// Path to a system prompt file.
        #[arg(long, value_name = "PATH")]
        system_prompt: Option<PathBuf>,

    },
}

#[tokio::main]
async fn main() {
    let initial_filter = build_env_filter("info").expect("default log level should be valid");
    let (filter_layer, log_filter_handle) = tracing_subscriber::reload::Layer::new(initial_filter);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Migrate {
            from,
            output,
            dry_run,
        } => {
            let source = match from.or_else(openclaw::auto_detect) {
                Some(p) => p,
                None => {
                    eprintln!(
                        "No OpenClaw installation found. \
                         Use --from <PATH> to specify one."
                    );
                    std::process::exit(1);
                }
            };
            match migrate::run(&source, &output, dry_run) {
                Ok(report) => {
                    report.print();
                }
                Err(e) => {
                    eprintln!("Migration failed: {e}");
                    std::process::exit(1);
                }
            }
        }

        Commands::Serve {
            telegram_token,
            model_url,
            model,
            copilot,
            deep_model,
            deep_model_url,
            api_key,
            embed_url,
            embed_model,
            system_prompt,
            brave_api_key,
            manus_ws_url,
            sync_topic_key,
            sync_shared_key,
            no_event_spine,
        } => {
            tracing::info!(commit = env!("GIT_COMMIT_HASH"), "Starting Pares Agens daemon");
            let started_at = Instant::now();
            let sync_enabled = sync_topic_key.is_some();

            let system_prompt_path = system_prompt;

            let mut model_url = model_url;
            let mut model = model;
            let mut deep_model = deep_model;
            let mut deep_escalation_enabled = default_deep_escalation_enabled();
            let mut runtime_log_level = "info".to_string();

            if copilot {
                if model == "gpt-4o" {
                    // Benchmark 2026-04-16: GPT-4.1 = 90% combined (GPQA+coding)
                    // at 3.7s avg. Fastest frontier model on Copilot Enterprise.
                    model = "gpt-4.1".into();
                }
                if deep_model == "gpt-4.1" {
                    // Benchmark 2026-04-16: Opus 4.6 = only model scoring 100%
                    // on BOTH GPQA Diamond AND coding. Worth the latency.
                    deep_model = "claude-opus-4.6".into();
                }
                tracing::info!("Copilot auth enabled");
                tracing::info!("Model: {model} (copilot)");
            } else {
                tracing::info!("Model: {model} @ {model_url}");
            }

            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let runtime_state_dir = PathBuf::from(&home).join(".pares-agens/runtime-state");
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
            let deep_escalation_enabled_state = Arc::new(RwLock::new(deep_escalation_enabled));
            let runtime_log_level_state = Arc::new(RwLock::new(runtime_log_level.clone()));
            let runtime_model_control = Arc::new(RuntimeModelControl {
                primary_model: Arc::clone(&model_name),
                deep_model: Arc::clone(&deep_model_name),
                deep_escalation_enabled: Arc::clone(&deep_escalation_enabled_state),
                state_store: Arc::clone(&runtime_state_store),
            });
            let mut runtime_config_control: Option<Arc<dyn TelegramConfigControl>> = None;

            let (model_client, deep_model_client): (Arc<dyn ModelClient>, Arc<dyn ModelClient>) =
                if copilot {
                    let auth_path = PathBuf::from(&home).join(".pares-agens/copilot-auth.json");
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
                        }) {
                            if let Err(e) = std::fs::write(&auth_path, serialized) {
                                tracing::warn!("failed to persist copilot auth: {e}");
                            }
                        }

                        oauth_token
                    };

                    let auth = CopilotAuth::new(oauth_token.clone());
                    let deep_auth = CopilotAuth::new(oauth_token);

                    // Default fallback chain for Copilot: if the primary model
                    // is unavailable (enterprise-only, rate-limited, etc.), try
                    // progressively simpler models.
                    let conscious_fallbacks = vec![
                        "gpt-4o".to_string(),
                        "gpt-4o-mini".to_string(),
                        "claude-3.5-sonnet".to_string(),
                    ];
                    let deep_fallbacks = vec![
                        "claude-3.5-sonnet".to_string(),
                        "gpt-4o".to_string(),
                    ];

                    (
                        Arc::new(CopilotModelClient::new_with_model_handle(
                            auth,
                            Arc::clone(&model_name),
                        ).with_fallbacks(conscious_fallbacks)),
                        Arc::new(CopilotModelClient::new_with_model_handle(
                            deep_auth,
                            Arc::clone(&deep_model_name),
                        ).with_fallbacks(deep_fallbacks)),
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
                    )
                };
            let deep_model_client: Arc<dyn ModelClient> = Arc::new(ToggleableModelClient::new(
                deep_model_client,
                Arc::clone(&deep_escalation_enabled_state),
            ));

            // Set up PluresDB memory store + PluresLM (native)
            let memory_path = PathBuf::from(home).join(".pares-agens/memory");
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
            procedure_registry.register(Box::new(WebSearchProcedure { brave_api_key }));
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
            procedure_registry.register(Box::new(RunCommandProcedure));

            // Initialize plugin framework
            let plugin_runtime = Arc::new(PluginRuntime::new());
            let plugin_executor = Arc::new(PluginCrudExecutor::new(
                store.crdt_store_arc(),
            ));

            // Load persisted plugins from PluresDB
            {
                let manifests = plugin_executor.load_persisted_manifests();
                for manifest_json in manifests {
                    if let Ok(manifest) = serde_json::from_value::<pares_agens_core::plugins::PluginManifest>(manifest_json) {
                        let name = manifest.name.clone();
                        if let Err(e) = plugin_runtime.install(manifest).await {
                            tracing::warn!(plugin = %name, error = %e, "failed to restore persisted plugin");
                        } else {
                            tracing::info!(plugin = %name, "restored persisted plugin");
                        }
                    }
                }
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

            let procedure_registry = Arc::new(procedure_registry);

            let tool_trace_store = ToolTraceStore::default();
            let governor = Arc::new(ToolGovernor::with_defaults());
            let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
                trace_store: tool_trace_store.clone(),
                governor: Arc::clone(&governor),
                plugin_runtime: Some(Arc::clone(&plugin_runtime)),
            });

            let mut registry = AgentRegistry::new();
            registry.register_builtins();
            let registry = Arc::new(registry);
            let agent_factory = Arc::new(RuntimeAgentFactory {
                store: Arc::clone(&store),
                model_client: Arc::clone(&model_client),
                deep_model_client: Arc::clone(&deep_model_client),
                tool_dispatcher: Arc::clone(&tool_dispatcher),
                registry: Arc::clone(&registry),
                embed_url,
                embed_model: embed_model.clone(),
                api_key: api_key.clone(),
                system_prompt_path: system_prompt_path.clone(),
            });
            let agent = match agent_factory.build_agent() {
                Ok(agent) => agent,
                Err(e) => {
                    tracing::error!("failed to initialize runtime agent: {e}");
                    std::process::exit(1);
                }
            };
            let agent_handle = Arc::new(RwLock::new(agent));

            // Inject plugin schema context into agent's system prompt
            {
                let schema_ctx = plugin_runtime.schema_context().await;
                if !schema_ctx.is_empty() {
                    let agent = agent_handle.read().await;
                    agent.set_plugin_context(Some(schema_ctx));
                    tracing::info!("Plugin schema context injected into system prompt");
                }
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
            config = config.with_plugin_runtime(
                Arc::clone(&plugin_runtime),
                Arc::clone(&plugin_executor),
            );
            let adapter = TelegramAdapter::new(config);

            tracing::info!("Telegram adapter starting — bot is live");

            // Initialize the event spine if enabled
            if !no_event_spine {
                let crdt = store.crdt_store();
                let spine = pares_agens_core::event_spine::EventSpine::new(crdt, "pares-agens");
                spine.seed_contracts();
                spine.register_core_procedures();
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
                if existing.and_then(|v| serde_json::from_value::<PersonalityContract>(v).ok()).is_none() {
                    let default = PersonalityContract::default_contract(None);
                    if let Ok(value) = serde_json::to_value(&default) {
                        runtime_state_store.set(PERSONALITY_STATE_KEY, value).await;
                        tracing::info!("Seeded default personality contract into PluresDB state");
                    }
                }
            }

            // Seed personality documents from ~/.pares-agens/ directory
            {
                use pares_agens_core::personality::{seed_from_directory, get_all_documents, format_documents_for_prompt};
                if let Ok(home) = std::env::var("HOME") {
                    let config_dir = std::path::PathBuf::from(&home).join(".pares-agens");
                    if config_dir.exists() {
                        seed_from_directory(runtime_state_store.as_ref(), &config_dir).await;
                    }
                }
                // Load documents and cache in agent
                let docs = get_all_documents(runtime_state_store.as_ref()).await;
                if !docs.is_empty() {
                    let formatted = format_documents_for_prompt(&docs);
                    agent_handle.read().await.set_personality_documents(Some(formatted));
                    tracing::info!(count = docs.len(), "loaded personality documents into agent");
                }
            }

            // Start the task scheduler in the background
            let scheduler = pares_agens_agenda::scheduler::Scheduler::new().with_executor(
                std::sync::Arc::new(|cmd: String| {
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
                }),
            );

            scheduler.add(self_update_task_from_env()).await;
            tracing::info!("Registered scheduled NixOS self-update task");

            // Spawn scheduler loop
            tokio::spawn(async move {
                scheduler.start().await;
            });
            tracing::info!("Scheduler started");

            let memory_monitor = spawn_memory_monitor();
            let watchdog = spawn_systemd_watchdog();

            let adapter_result =
                run_adapter_with_recovery(&adapter, Arc::clone(&agent_handle), tool_trace_store)
                    .await;

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

        Commands::Tui {
            model_url,
            model,
            copilot,
            api_key,
            system_prompt,
        } => {
            use crossterm::{
                event::{self as ct_event, Event as CtEvent, KeyCode, KeyEventKind},
                execute,
                terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
            };
            use ratatui::backend::CrosstermBackend;
            use ratatui::Terminal;
            use pares_agens_tui::app::{App, AppEvent};

            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            let mut model = model;

            // Build model client
            let model_name_handle = Arc::new(RwLock::new(model.clone()));
            let model_client: Arc<dyn ModelClient> = if copilot {
                if model == "gpt-4.1" || model == "gpt-4o" {
                    model = "gpt-4.1".into();
                    *model_name_handle.write().await = model.clone();
                }
                let auth_path = PathBuf::from(&home).join(".pares-agens/copilot-auth.json");
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
                                eprintln!("Copilot device flow failed: {e}");
                                std::process::exit(1);
                            }
                        };

                    println!("Authorize Copilot: visit {verification_uri} and enter code {user_code}");

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
                    }) {
                        let _ = std::fs::write(&auth_path, serialized);
                    }
                    token
                };

                let auth = CopilotAuth::new(oauth_token);
                Arc::new(CopilotModelClient::new_with_model_handle(
                    auth,
                    Arc::clone(&model_name_handle),
                ).with_fallbacks(vec!["gpt-4o".into(), "gpt-4o-mini".into()]))
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

            // Build memory + agent
            let memory_path = PathBuf::from(&home).join(".pares-agens/memory");
            let store: Arc<PluresDbStore> = match PluresDbStore::open_with_embeddings(&memory_path) {
                Ok(store) => Arc::new(store),
                Err(_) => match PluresDbStore::open(&memory_path) {
                    Ok(store) => Arc::new(store),
                    Err(e) => {
                        eprintln!("Failed to open memory store: {e}");
                        std::process::exit(1);
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
            procedure_registry.register(Box::new(RunCommandProcedure));
            procedure_registry.register(Box::new(WebFetchProcedure));
            let procedure_registry = Arc::new(procedure_registry);
            let governor = Arc::new(ToolGovernor::with_defaults());
            let tool_dispatcher: Arc<dyn ToolDispatcher> = Arc::new(ProcedureToolDispatcher {
                registry: Arc::clone(&procedure_registry),
                trace_store: ToolTraceStore::default(),
                governor: Arc::clone(&governor),
                plugin_runtime: None,
            });

            let cerebellum = Cerebellum::new(CerebellumConfig::default());
            let system_prompt_text = build_system_prompt(system_prompt)
                .unwrap_or_else(|e| {
                    eprintln!("Warning: {e}");
                    "You are Pares Agens, an AI assistant. Be direct and helpful.".to_string()
                });

            let mut registry = pares_agens_core::delegation::registry::AgentRegistry::new();
            registry.register_builtins();

            let agent = Arc::new(
                Agent::with_cerebellum(memory, cerebellum, plures_lm)
                    .with_model(
                        Arc::clone(&model_client),
                        Arc::clone(&tool_dispatcher),
                        system_prompt_text,
                    )
                    .with_turn_store(Arc::clone(&store) as Arc<dyn pares_agens_core::memory::store::MemoryStore>),
            );

            // Set up terminal
            enable_raw_mode().expect("failed to enable raw mode");
            let mut stdout = std::io::stdout();
            execute!(stdout, EnterAlternateScreen).expect("failed to enter alternate screen");
            let backend = CrosstermBackend::new(stdout);
            let mut terminal = Terminal::new(backend).expect("failed to create terminal");

            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<AppEvent>();
            let mut app = App::new(agent, model.clone(), event_tx);

            // Main loop
            let result: Result<(), Box<dyn std::error::Error>> = 'main_loop: loop {
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
                        match key.code {
                            KeyCode::Enter => {
                                app.submit_input();
                            }
                            KeyCode::Char(c) => {
                                app.input.insert(app.input_cursor, c);
                                app.input_cursor += 1;
                            }
                            KeyCode::Backspace => {
                                if app.input_cursor > 0 {
                                    app.input_cursor -= 1;
                                    app.input.remove(app.input_cursor);
                                }
                            }
                            KeyCode::Left => {
                                if app.input_cursor > 0 {
                                    app.input_cursor -= 1;
                                }
                            }
                            KeyCode::Right => {
                                if app.input_cursor < app.input.len() {
                                    app.input_cursor += 1;
                                }
                            }
                            KeyCode::Home => {
                                app.input_cursor = 0;
                            }
                            KeyCode::End => {
                                app.input_cursor = app.input.len();
                            }
                            KeyCode::PageUp => {
                                app.scroll_offset = app.scroll_offset.saturating_add(10);
                            }
                            KeyCode::PageDown => {
                                app.scroll_offset = app.scroll_offset.saturating_sub(10);
                            }
                            KeyCode::Esc => {
                                break 'main_loop Ok(());
                            }
                            _ => {}
                        }
                    }
                }

                // Drain app events (non-blocking)
                while let Ok(ev) = event_rx.try_recv() {
                    match ev {
                        AppEvent::AgentResponse(content) => {
                            app.handle_agent_response(content);
                        }
                        AppEvent::Quit => {
                            break 'main_loop Ok(());
                        }
                        AppEvent::Redraw => {}
                        AppEvent::UserInput(_) => {}
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_core::model::{ModelCompletion, ToolCall, ToolDefinition};

    struct TestModelClient;

    #[async_trait]
    impl ModelClient for TestModelClient {
        async fn complete(
            &self,
            _messages: &[CoreChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some("ok".to_string()),
                tool_calls: Vec::<ToolCall>::new(),
                logprobs: None,
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

    #[test]
    fn detect_single_connection_conflicts_for_local_host() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, "telegram");
        assert_eq!(
            conflicts[0].hosts,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_single_connections() {
        let records = vec![
            HostAdapterRecord {
                host: "alpha".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "local".to_string(),
                    connection_id: "n/a".to_string(),
                    single_connection: false,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn detect_single_connection_conflicts_ignores_non_local_conflicts() {
        let records = vec![
            HostAdapterRecord {
                host: "beta".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
            HostAdapterRecord {
                host: "gamma".to_string(),
                adapters: vec![HostAdapterConfig {
                    kind: "telegram".to_string(),
                    connection_id: "token-a".to_string(),
                    single_connection: true,
                }],
            },
        ];
        let conflicts = detect_single_connection_conflicts("alpha", &records);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn build_nixos_update_command_includes_required_commands() {
        let command = build_nixos_update_command("/etc/nixos", "praxisbot");
        assert!(command.contains("sudo nix flake update pares-agens"));
        assert!(command.contains("sudo nixos-rebuild switch --flake .#'praxisbot'"));
        assert!(command.contains("No new pares-agens commits on main"));
    }

    #[test]
    fn self_update_task_defaults_are_applied() {
        let task = build_self_update_task(
            DEFAULT_NIX_FLAKE_DIR,
            DEFAULT_NIX_HOST,
            DEFAULT_SELF_UPDATE_INTERVAL_SECS,
        );
        assert_eq!(task.id, "self-update.nixos-rebuild");
        assert!(task.enabled);
        match task.schedule {
            pares_agens_agenda::scheduler::Schedule::Interval { every_secs } => {
                assert_eq!(every_secs, DEFAULT_SELF_UPDATE_INTERVAL_SECS);
            }
            _ => panic!("expected interval schedule"),
        }
    }

    #[test]
    fn parse_vm_rss_kib_extracts_numeric_value() {
        let status = "Name:\tpares-agens\nVmRSS:\t   42104 kB\nThreads:\t6\n";
        assert_eq!(parse_vm_rss_kib(status), Some(42104));
    }

    #[test]
    fn parse_watchdog_ping_interval_uses_half_of_watchdog_usec() {
        let interval = parse_watchdog_ping_interval("4000000").expect("watchdog interval");
        assert_eq!(interval, Duration::from_secs(2));
    }

    #[test]
    fn parse_watchdog_ping_interval_has_safe_minimum() {
        let interval = parse_watchdog_ping_interval("1000").expect("watchdog interval");
        assert_eq!(interval, Duration::from_secs(1));
    }

    #[test]
    fn extract_verbose_tool_marker_detects_and_strips_prefix() {
        let (is_verbose, stripped) =
            extract_verbose_tool_marker("__PARES_VERBOSE_TOOL_DETAILS__:run diagnostics");
        assert!(is_verbose);
        assert_eq!(stripped, "run diagnostics");
    }

    #[test]
    fn extract_verbose_tool_marker_preserves_plain_content() {
        let (is_verbose, stripped) = extract_verbose_tool_marker("hello");
        assert!(!is_verbose);
        assert_eq!(stripped, "hello");
    }

    #[test]
    fn format_verbose_tool_traces_renders_tool_name_and_result() {
        let traces = vec![ToolCallTrace {
            tool_name: "web_search".to_string(),
            arguments: serde_json::json!({"q":"status"}),
            result: "{\"ok\":true}".to_string(),
            is_error: false,
        }];
        let formatted = format_verbose_tool_traces(&traces);
        assert!(formatted.contains("Tool execution details:"));
        assert!(formatted.contains("web_search [ok]"));
        assert!(formatted.contains("result: {\"ok\":true}"));
    }

    #[test]
    fn manus_request_maps_browser_click_to_gui_click() {
        let (method, params) =
            manus_request_for_tool("browser_click", serde_json::json!({"x": 21, "y": 34}))
                .expect("request should map");
        assert_eq!(method, "gui.click");
        assert_eq!(params, serde_json::json!({"x": 21, "y": 34}));
    }

    #[test]
    fn manus_request_requires_browser_open_url() {
        let err = manus_request_for_tool("browser_open", serde_json::json!({}))
            .expect_err("missing url should fail");
        assert!(err.contains("missing 'url'"));
    }

    #[test]
    fn manus_request_maps_screen_capture_optional_fields() {
        let (method, params) = manus_request_for_tool(
            "screen_capture",
            serde_json::json!({"monitor": 1, "window": "Edge"}),
        )
        .expect("request should map");
        assert_eq!(method, "screen.capture");
        assert_eq!(params, serde_json::json!({"monitor": 1, "window": "Edge"}));
    }

    #[tokio::test]
    async fn runtime_model_control_persists_primary_model_override() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_agens_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4.1".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
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
            Arc::new(pares_agens_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
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
            Arc::new(pares_agens_core::InMemoryStateStore::new());
        let control = RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
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

    #[test]
    fn normalize_log_level_accepts_known_values() {
        assert_eq!(normalize_log_level("DEBUG").unwrap(), "debug");
        assert_eq!(normalize_log_level(" warn ").unwrap(), "warn");
    }

    #[test]
    fn normalize_log_level_rejects_unknown_values() {
        assert!(normalize_log_level("verbose").is_err());
    }

    #[tokio::test]
    async fn runtime_config_control_persists_model_endpoint_and_log_level() {
        let state_store: Arc<dyn StateStore> =
            Arc::new(pares_agens_core::InMemoryStateStore::new());
        let runtime_model_control = Arc::new(RuntimeModelControl {
            primary_model: Arc::new(RwLock::new("gpt-4o".to_string())),
            deep_model: Arc::new(RwLock::new("claude-opus-4.6".to_string())),
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
            tool_dispatcher,
            registry: Arc::new(registry),
            embed_url: None,
            embed_model: "nomic-embed-text".to_string(),
            api_key: None,
            system_prompt_path: None,
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
