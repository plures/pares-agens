//! High-level agent abstraction and in-memory storage for testing/development.
//!
//! [`Agent`] is the top-level entry point used by channel adapters (stdin,
//! Telegram) to process inbound [`Event`]s and produce an optional response.
//!
//! When built with a [`Cerebellum`] via [`Agent::with_cerebellum`], every
//! inbound [`Event::Message`] is first preprocessed by the cerebellum:
//! autorecall retrieves relevant memories, the router determines the path
//! (conscious / deep / procedural / drop), and any recalled context is
//! injected into the response.
//!
//! [`Memory`] is the trait implemented by storage backends.  [`InMemory`]
//! provides a simple in-process implementation suitable for tests and the
//! first-run experience before a persistent store is configured.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::cerebellum::{Cerebellum, Route};
use crate::delegation::aggregator::ResultAggregator;
use crate::delegation::broker::DelegationBroker;
use crate::event::Event;
use crate::memory::entry::Exchange;
use crate::memory::store::MemoryStore;
use crate::memory::{passes_quality_gate, PluresLm};
use crate::model::{ChatMessage, ChatOptions, ModelClient, ToolDispatcher};
use crate::procedure::ProcedureRegistry;

// ---------------------------------------------------------------------------
// Memory trait
// ---------------------------------------------------------------------------

/// Trait for agent memory storage.
///
/// Implementations persist conversation content and support fuzzy recall.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Persist `content` to memory.
    ///
    /// Returns `Err` if the backend is unavailable or the write fails.
    async fn capture(&self, content: &str) -> Result<(), String>;

    /// Retrieve entries that match `query`.
    ///
    /// The query is matched case-insensitively as a substring against stored
    /// entries.  Returns an empty `Vec` when nothing matches.
    async fn recall(&self, query: &str) -> Result<Vec<String>, String>;
}

// ---------------------------------------------------------------------------
// InMemory
// ---------------------------------------------------------------------------

/// In-memory [`Memory`] implementation for testing and development.
///
/// All entries are stored in a `Vec<String>` guarded by a `tokio::sync::Mutex`
/// so the lock is held only briefly and never blocks the async executor.
/// Recall performs a simple case-insensitive substring match.
pub struct InMemory {
    entries: Arc<TokioMutex<Vec<String>>>,
}

impl InMemory {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(TokioMutex::new(Vec::new())),
        }
    }
}

impl Default for InMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Memory for InMemory {
    async fn capture(&self, content: &str) -> Result<(), String> {
        self.entries.lock().await.push(content.to_string());
        Ok(())
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, String> {
        let q = query.to_lowercase();
        let entries = self.entries.lock().await;
        Ok(entries
            .iter()
            .filter(|e| e.to_lowercase().contains(&q))
            .cloned()
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// High-level agent that handles events and captures memory.
///
/// `Agent` is the entry-point used by channel adapters (stdin, Telegram)
/// to process inbound [`Event`]s and produce an optional response.
///
/// # Behaviour
///
/// For [`Event::Message`] events the agent:
/// 1. Runs the event through the [`Cerebellum`] (if configured) to perform
///    autorecall and routing.  A [`Route::Drop`] causes the event to be
///    silently discarded.
/// 2. Dispatches the event based on the chosen route:
///    - Conscious/Deep: call the model client with context + history
///    - Procedural: execute matching procedures from the registry
/// 3. Captures the conversation exchange in memory when a response is
///    produced.
///
/// All other event kinds follow the routing decision or return `None`.
pub struct Agent {
    memory: Arc<dyn Memory + Send + Sync>,
    /// Optional cerebellum for autorecall and routing.
    cerebellum: Option<Cerebellum>,
    /// PluresLM memory client passed to the cerebellum's `preprocess()`.
    plures_lm: Option<Arc<PluresLm>>,
    /// Procedure registry used for `Route::Procedural` dispatch.
    procedure_registry: ProcedureRegistry,
    /// Model client for conscious/subconscious completions.
    model_client: Option<Arc<dyn ModelClient>>,
    /// Optional deep model client for low-confidence escalation.
    deep_model_client: Option<Arc<dyn ModelClient>>,
    /// Tool dispatcher for model tool calls.
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    /// Base system prompt (legacy fallback).
    system_prompt: String,
    /// Personality contract for dynamic prompt building.
    personality: Option<crate::personality::PersonalityContract>,
    /// Current channel name (e.g. "telegram").
    current_channel: std::sync::Mutex<Option<String>>,
    /// Per-channel conversation history keyed by channel/session label.
    conversation_history: Mutex<HashMap<String, Vec<ChatMessage>>>,
    /// Optional persistent turn store (PluresDB). When `Some`, conversation
    /// turns are persisted across restarts. Falls back to in-memory only.
    turn_store: Option<Arc<dyn MemoryStore>>,
    /// Optional audit store for logging all agent actions (model calls, tool
    /// executions, memory writes). When `Some`, every significant action is
    /// recorded for compliance and debugging.
    audit_store: Option<Arc<dyn pares_agens_audit::store::AuditStore>>,
    /// Optional delegation broker for decomposed tasks.
    delegation_broker: Option<DelegationBroker>,
    /// Per-channel conversation branch/session state.
    branch_state: Mutex<HashMap<String, ChannelBranches>>,
    /// Cached formatted personality documents (SOUL.md, IDENTITY.md, etc.).
    /// Populated on startup from PluresDB and updated via `/personality doc` commands.
    personality_documents_cache: Mutex<Option<String>>,
    /// Cached plugin schema context for system prompt injection.
    plugin_context: Mutex<Option<String>>,
}

#[derive(Debug, Clone)]
struct ChannelBranches {
    active: String,
    branches: BTreeSet<String>,
}

impl Default for ChannelBranches {
    fn default() -> Self {
        let mut branches = BTreeSet::new();
        branches.insert("main".to_string());
        Self {
            active: "main".to_string(),
            branches,
        }
    }
}

impl Agent {
    /// Create a basic agent backed by `memory` (no cerebellum).
    pub fn new(memory: Arc<dyn Memory + Send + Sync>) -> Self {
        Self {
            memory,
            cerebellum: None,
            plures_lm: None,
            procedure_registry: ProcedureRegistry::new(),
            model_client: None,
            deep_model_client: None,
            tool_dispatcher: None,
            system_prompt: String::new(),
            personality: None,
            current_channel: std::sync::Mutex::new(None),
            conversation_history: Mutex::new(HashMap::new()),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
            branch_state: Mutex::new(HashMap::new()),
            personality_documents_cache: Mutex::new(None),
            plugin_context: Mutex::new(None),
        }
    }

    /// Create an agent with a [`Cerebellum`] wired in.
    ///
    /// Every inbound [`Event::Message`] is routed through
    /// `cerebellum.preprocess()` before being handled.  The `plures_lm`
    /// instance is used for autorecall; pass the same [`PluresLm`] that
    /// backs the application's memory store so recalled memories are live.
    pub fn with_cerebellum(
        memory: Arc<dyn Memory + Send + Sync>,
        cerebellum: Cerebellum,
        plures_lm: Arc<PluresLm>,
    ) -> Self {
        Self {
            memory,
            cerebellum: Some(cerebellum),
            plures_lm: Some(plures_lm),
            procedure_registry: ProcedureRegistry::new(),
            model_client: None,
            deep_model_client: None,
            tool_dispatcher: None,
            system_prompt: String::new(),
            personality: None,
            current_channel: std::sync::Mutex::new(None),
            conversation_history: Mutex::new(HashMap::new()),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
            branch_state: Mutex::new(HashMap::new()),
            personality_documents_cache: Mutex::new(None),
            plugin_context: Mutex::new(None),
        }
    }

    /// Attach a model client + tool dispatcher + system prompt to the agent.
    pub fn with_model(
        mut self,
        client: Arc<dyn ModelClient>,
        dispatcher: Arc<dyn ToolDispatcher>,
        system_prompt: String,
    ) -> Self {
        self.model_client = Some(client);
        self.tool_dispatcher = Some(dispatcher);
        self.system_prompt = system_prompt;
        self
    }

    /// Attach a personality contract for dynamic prompt building.
    pub fn with_personality(mut self, personality: crate::personality::PersonalityContract) -> Self {
        self.personality = Some(personality);
        self
    }

    /// Set the current channel name for personality overrides.
    pub fn set_channel(&self, channel: &str) {
        if let Ok(mut ch) = self.current_channel.lock() {
            *ch = Some(channel.to_string());
        }
    }

    /// Update the cached personality documents text.
    ///
    /// Called on startup after loading documents from PluresDB and after
    /// `/personality doc` mutations.
    pub fn set_personality_documents(&self, formatted: Option<String>) {
        if let Ok(mut cache) = self.personality_documents_cache.lock() {
            *cache = formatted;
        }
    }

    /// Set the plugin schema context for system prompt injection.
    pub fn set_plugin_context(&self, context: Option<String>) {
        if let Ok(mut cache) = self.plugin_context.lock() {
            *cache = context;
        }
    }

    /// Get a mutable reference to the personality contract.
    pub fn personality_mut(&mut self) -> Option<&mut crate::personality::PersonalityContract> {
        self.personality.as_mut()
    }

    /// Get a reference to the personality contract.
    pub fn personality(&self) -> Option<&crate::personality::PersonalityContract> {
        self.personality.as_ref()
    }

    /// Attach a deep model client used for low-confidence escalation.
    pub fn with_deep_model(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.deep_model_client = Some(client);
        self
    }

    /// Attach a delegation broker for decomposed tasks.
    pub fn with_delegation(mut self, broker: DelegationBroker) -> Self {
        self.delegation_broker = Some(broker);
        self
    }

    /// Attach a persistent turn store (PluresDB) for conversation history.
    ///
    /// When set, every user→assistant exchange is persisted as a [`ChatTurn`]
    /// and history survives process restarts.  On first message in a channel
    /// the agent hydrates the in-memory cache from PluresDB.
    pub fn with_turn_store(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.turn_store = Some(store);
        self
    }

    /// Attach an audit store for logging all agent actions.
    pub fn with_audit_store(
        mut self,
        store: Arc<dyn pares_agens_audit::store::AuditStore>,
    ) -> Self {
        self.audit_store = Some(store);
        self
    }

    /// Handle a single event and optionally return a response event.
    pub async fn handle_event(&self, event: Event) -> Option<Event> {
        let request_id = Uuid::new_v4();
        let _event_start = Instant::now();
        info!(%request_id, event_kind = %event.kind(), "received event");
        if let Event::Message {
            ref id,
            ref channel,
            ref content,
            ..
        } = event
        {
            if let Some(command_response) = self.handle_branch_command(id, channel, content).await {
                return Some(command_response);
            }
        }

        // ── Cerebellum: autorecall + routing ─────────────────────────────
        let (route, learned_context, clear_history) = if let (Some(cerebellum), Some(plures_lm)) =
            (&self.cerebellum, &self.plures_lm)
        {
            match cerebellum
                .preprocess(&event, plures_lm, &self.procedure_registry)
                .await
            {
                Ok(ctx) => {
                    debug!(route = ?ctx.route, context_len = ctx.learned_context.len(), "cerebellum preprocessed event");
                    if ctx.route == Route::Drop {
                        debug!(
                            event_kind = event.kind(),
                            "cerebellum dropped event (Route::Drop)"
                        );
                        return None;
                    }
                    (ctx.route, ctx.learned_context, ctx.clear_history)
                }
                Err(e) => {
                    error!(error = %e, "agent: cerebellum preprocess failed, continuing without context");
                    (Route::Conscious, String::new(), false)
                }
            }
        } else {
            let default_route = match event {
                Event::Timer { .. } | Event::StateChange { .. } => Route::Procedural,
                _ => Route::Conscious,
            };
            (default_route, String::new(), false)
        };

        if route == Route::Drop {
            return None;
        }

        match event {
            Event::Message {
                ref id,
                ref channel,
                ref content,
                ..
            } => match route {
                Route::Procedural => {
                    self.dispatch_procedures(&Event::Message {
                        id: id.clone(),
                        channel: channel.clone(),
                        sender: String::new(),
                        content: content.clone(),
                    })
                    .await
                }
                Route::Delegate { reason, tasks } => {
                    let delegated = self
                        .handle_delegation(id, channel, content, &learned_context, &reason, tasks)
                        .await;
                    if delegated.is_some() {
                        delegated
                    } else {
                        self.handle_model_message(
                            id,
                            channel,
                            content,
                            &learned_context,
                            clear_history,
                        )
                        .await
                    }
                }
                Route::Conscious | Route::Deep { .. } => {
                    self.handle_model_message(id, channel, content, &learned_context, clear_history)
                        .await
                }
                Route::Drop => None,
            },
            Event::Timer { .. } | Event::StateChange { .. } => {
                if matches!(route, Route::Procedural) {
                    self.dispatch_procedures(&event).await
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    async fn handle_model_message(
        &self,
        id: &str,
        channel: &str,
        content: &str,
        learned_context: &str,
        clear_history: bool,
    ) -> Option<Event> {
        let session_channel = self.resolve_branch_channel(channel);
        let session_id = Self::branch_label(&session_channel);
        let model_client = match &self.model_client {
            Some(client) => client,
            None => {
                warn!("agent: model client not configured");
                return Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "unconfigured".into(),
                    content: "⚠️ Model client not configured.".into(),
                });
            }
        };
        let tool_dispatcher = match &self.tool_dispatcher {
            Some(dispatcher) => dispatcher,
            None => {
                warn!("agent: tool dispatcher not configured");
                return Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "unconfigured".into(),
                    content: "⚠️ Tool dispatcher not configured.".into(),
                });
            }
        };

        let history_snapshot = if clear_history {
            vec![]
        } else {
            self.load_history(&session_channel).await
        };

        let base_system_text = self.build_system_prompt(learned_context, false);
        let options = ChatOptions {
            temperature: None,
            logprobs: true,
        };

        let (mut reply, logprobs, mut messages) = match self
            .run_model_loop(
                model_client,
                tool_dispatcher,
                base_system_text,
                &history_snapshot,
                content,
                &options,
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "model completion failed");
                return Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "error".into(),
                    content: format!("⚠️ Model error: {e}"),
                });
            }
        };

        let mut model_label = "model";
        if self.is_low_confidence(logprobs.as_deref()) {
            if let Some(deep_client) = &self.deep_model_client {
                let deep_system_text = self.build_system_prompt(learned_context, true);
                let deep_options = ChatOptions {
                    temperature: None,
                    logprobs: false,
                };
                match self
                    .run_model_loop(
                        deep_client,
                        tool_dispatcher,
                        deep_system_text,
                        &history_snapshot,
                        content,
                        &deep_options,
                    )
                    .await
                {
                    Ok((deep_reply, _deep_logprobs, deep_messages)) => {
                        reply = deep_reply;
                        messages = deep_messages;
                        model_label = "deep-model";
                    }
                    Err(e) => {
                        warn!(error = %e, "deep model completion failed, using conscious reply");
                    }
                }
            } else {
                debug!("low confidence detected, but no deep model configured");
            }
        }

        info!(
            input_len = content.len(),
            output_len = reply.len(),
            "LLM response generated"
        );

        // Audit: log model call
        self.audit(
            pares_agens_audit::event::EventKind::ModelCall,
            "agent",
            model_label,
            &format!("in={}tok out={}tok", content.len() / 4, reply.len() / 4),
        )
        .await;

        // Persist new turn messages to PluresDB and update in-memory cache.
        let start = 1 + history_snapshot.len(); // skip system + existing history
        if messages.len() > start {
            let new_messages: Vec<ChatMessage> = messages[start..].to_vec();
            self.persist_turn(&session_channel, &session_id, &new_messages)
                .await;
        }

        self.capture_exchange(content, &reply).await;
        self.spawn_procedure_writer(content, &reply);

        Some(Event::ModelResponse {
            request_id: id.to_string(),
            model: model_label.into(),
            content: reply,
        })
    }

    async fn handle_delegation(
        &self,
        id: &str,
        channel: &str,
        content: &str,
        learned_context: &str,
        reason: &str,
        tasks: Vec<crate::delegation::broker::SubTask>,
    ) -> Option<Event> {
        let broker = match &self.delegation_broker {
            Some(broker) => broker,
            None => {
                warn!("agent: delegation broker not configured");
                return None;
            }
        };

        let enriched_tasks = if learned_context.trim().is_empty() {
            tasks
        } else {
            tasks
                .into_iter()
                .map(|task| {
                    if task.parent_context.is_none() {
                        task.with_parent_context(learned_context.trim().to_string())
                    } else {
                        task
                    }
                })
                .collect()
        };

        info!(
            request_id = %id,
            agent_count = enriched_tasks.len(),
            %channel,
            %reason,
            "delegating task"
        );

        let results = broker.delegate(enriched_tasks).await;
        let aggregated = ResultAggregator::new().aggregate(results);
        if !aggregated.has_output() {
            warn!(request_id = %id, "delegation returned no output; falling back");
            return None;
        }

        let mut content_out = aggregated.content;
        if !aggregated.failed.is_empty() {
            content_out.push_str("\n\n## Delegation failures\n");
            for (agent, err) in aggregated.failed {
                content_out.push_str(&format!("- {agent}: {err}\n"));
            }
        }

        self.spawn_procedure_writer(content, &content_out);

        Some(Event::ModelResponse {
            request_id: id.to_string(),
            model: "delegated".into(),
            content: content_out,
        })
    }

    fn spawn_procedure_writer(&self, user: &str, assistant: &str) {
        let Some(plures_lm) = &self.plures_lm else {
            return;
        };

        let Some(candidate) = self.extract_procedure_candidate(user, assistant) else {
            return;
        };

        let tags = self.extract_domain_tags(user);
        let plures_lm = Arc::clone(plures_lm);
        tokio::spawn(async move {
            if let Err(e) = plures_lm
                .capture_procedure_candidate(&candidate, tags)
                .await
            {
                error!(error = %e, "agent: failed to capture procedure candidate");
            }
        });
    }

    fn extract_procedure_candidate(&self, user: &str, assistant: &str) -> Option<String> {
        let lower = assistant.to_lowercase();
        let triggers = [
            "steps:",
            "procedure:",
            "workflow:",
            "runbook:",
            "playbook:",
            "checklist:",
        ];
        let has_steps = triggers.iter().any(|t| lower.contains(t))
            || (lower.contains("step 1") && lower.contains("step 2"))
            || (assistant.contains("1.") && assistant.contains("2."));

        if !has_steps {
            return None;
        }

        let summary = assistant
            .lines()
            .filter(|line| !line.trim().is_empty())
            .take(12)
            .collect::<Vec<_>>()
            .join("\n");

        if summary.trim().is_empty() {
            return None;
        }

        Some(format!(
            "Procedure candidate derived from user request:\nUser: {user}\n\n{summary}"
        ))
    }

    fn build_system_prompt(&self, learned_context: &str, deep: bool) -> String {
        // If a personality contract is set, use the dynamic prompt builder.
        if let Some(personality) = &self.personality {
            let channel = self.current_channel.lock().ok().and_then(|ch| ch.clone());
            let docs_cache = self.personality_documents_cache.lock().ok()
                .and_then(|g| g.clone());
            let plugin_cache = self.plugin_context.lock().ok()
                .and_then(|g| g.clone());
            let ctx = crate::prompt_builder::AgentContext {
                channel: channel.as_deref(),
                learned_context,
                conversation_summary: None,
                deep,
                personality_documents: docs_cache.as_deref(),
                plugin_context: plugin_cache.as_deref(),
            };
            return crate::prompt_builder::build_system_prompt(personality, &ctx);
        }

        // Legacy fallback: flat system prompt string.
        let mut prompt = String::new();
        if deep {
            prompt.push_str("Think deeply about this. Analyze thoroughly.");
            if !self.system_prompt.is_empty() {
                prompt.push(' ');
            }
        }
        prompt.push_str(&self.system_prompt);
        if !learned_context.trim().is_empty() {
            prompt.push_str("\n\n## Recalled Context\n");
            prompt.push_str(learned_context.trim());
        }
        prompt
    }

    async fn run_model_loop(
        &self,
        model_client: &Arc<dyn ModelClient>,
        tool_dispatcher: &Arc<dyn ToolDispatcher>,
        system_text: String,
        history_snapshot: &[ChatMessage],
        content: &str,
        options: &ChatOptions,
    ) -> Result<(String, Option<Vec<f64>>, Vec<ChatMessage>), String> {
        let mut messages = Vec::with_capacity(history_snapshot.len() + 2);
        messages.push(ChatMessage::system(system_text));
        messages.extend(history_snapshot.iter().cloned());
        messages.push(ChatMessage::user(content));

        let tools = tool_dispatcher.available_tools().await;

        let mut final_reply = None;
        let mut final_logprobs = None;
        for turn in 0..10 {
            let model_start = Instant::now();
            let completion = model_client.complete(&messages, &tools, options).await?;
            let latency_ms = model_start.elapsed().as_millis();
            info!(turn, latency_ms, tool_calls = completion.tool_calls.len(), "model completion received");

            if !completion.tool_calls.is_empty() {
                let tool_calls = completion.tool_calls.clone();
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: completion.content.unwrap_or_default(),
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                });

                for tool_call in tool_calls {
                    let tool_result = tool_dispatcher
                        .call_tool(&tool_call.name, tool_call.arguments)
                        .await;
                    messages.push(ChatMessage::tool_result(tool_call.id, tool_result));
                }
                continue;
            }

            if let Some(content) = completion.content {
                messages.push(ChatMessage::assistant(content.clone()));
                final_reply = Some(content);
                final_logprobs = completion.logprobs;
                break;
            }

            final_reply = Some("(empty response from model)".into());
            break;
        }

        let reply = final_reply.unwrap_or_else(|| "(no response from model)".into());
        Ok((reply, final_logprobs, messages))
    }

    fn is_low_confidence(&self, logprobs: Option<&[f64]>) -> bool {
        let Some(logprobs) = logprobs else {
            return false;
        };
        if logprobs.is_empty() {
            return false;
        }
        let avg_logprob = logprobs.iter().sum::<f64>() / logprobs.len() as f64;
        let min_prob = logprobs
            .iter()
            .map(|lp| lp.exp())
            .fold(1.0_f64, |acc, p| acc.min(p));
        avg_logprob < -1.0 || min_prob < 0.6
    }

    // ── Conversation history persistence ─────────────────────────────────

    /// Number of persisted turns to hydrate when rebuilding in-memory history.
    const HYDRATE_TURN_LIMIT: usize = 512;

    /// Approximate tokens per character (conservative estimate).
    const CHARS_PER_TOKEN: usize = 4;

    /// Maximum context budget for history (tokens). Default ~80% of 128K.
    const MAX_HISTORY_TOKENS: usize = 100_000;

    /// When history exceeds token budget, summarize older messages into this
    /// many tokens worth of condensed context.
    const SUMMARY_TOKEN_BUDGET: usize = 2_000;

    /// Load conversation history for `channel`.
    ///
    /// Enforces a token budget: if history exceeds [`MAX_HISTORY_TOKENS`],
    /// older messages are dropped and a summary prefix is prepended.
    /// PluresDB stores ALL turns (no data loss); only the LLM window is trimmed.
    async fn load_history(&self, channel: &str) -> Vec<ChatMessage> {
        // Fast path: check in-memory cache first.
        {
            let guard = self.conversation_history.lock().unwrap();
            if let Some(cached) = guard.get(channel) {
                if !cached.is_empty() {
                    return Self::trim_to_token_budget(cached);
                }
            }
        }

        // Slow path: hydrate from PluresDB if available.
        if let Some(store) = &self.turn_store {
            match store.recent_turns(channel, Self::HYDRATE_TURN_LIMIT).await {
                Ok(turns) if !turns.is_empty() => {
                    let messages: Vec<ChatMessage> =
                        turns.into_iter().flat_map(|t| t.messages).collect();
                    let trimmed = Self::trim_to_token_budget(&messages);
                    info!(
                        channel,
                        total = messages.len(),
                        trimmed = trimmed.len(),
                        "hydrated conversation history from PluresDB"
                    );
                    // Cache for future calls.
                    let mut guard = self.conversation_history.lock().unwrap();
                    guard.insert(channel.to_string(), messages);
                    return trimmed;
                }
                Ok(_) => {} // no turns yet
                Err(e) => {
                    warn!(error = %e, channel, "failed to load turns from PluresDB, using empty history");
                }
            }
        }

        Vec::new()
    }

    /// Persist a set of new messages as a conversation turn.
    ///
    /// Updates both the in-memory cache and (if configured) the persistent
    /// PluresDB turn store.
    async fn persist_turn(&self, channel: &str, session_id: &str, new_messages: &[ChatMessage]) {
        // Update in-memory cache.
        {
            let mut guard = self.conversation_history.lock().unwrap();
            let history = guard.entry(channel.to_string()).or_default();
            history.extend(new_messages.iter().cloned());
            let compacted = Self::trim_to_token_budget(history);
            *history = compacted;
        }

        // Persist to PluresDB if available.
        if let Some(store) = &self.turn_store {
            use crate::memory::entry::ChatTurn;
            let turn = ChatTurn {
                id: uuid::Uuid::new_v4().to_string(),
                channel: channel.to_string(),
                session_id: session_id.to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                messages: new_messages.to_vec(),
            };
            if let Err(e) = store.insert_turn(turn).await {
                warn!(error = %e, channel, "failed to persist conversation turn");
            }
        }
    }

    /// Estimate token count for a message (chars / 4).
    fn estimate_tokens(msg: &ChatMessage) -> usize {
        msg.content.len() / Self::CHARS_PER_TOKEN + 1
    }

    /// Trim message history to fit within the token budget.
    ///
    /// Keeps the most recent messages that fit. If the full history exceeds
    /// the budget, a summary system message is prepended noting how many
    /// older messages were truncated.
    fn trim_to_token_budget(messages: &[ChatMessage]) -> Vec<ChatMessage> {
        // Count total tokens
        let total_tokens: usize = messages.iter().map(Self::estimate_tokens).sum();

        if total_tokens <= Self::MAX_HISTORY_TOKENS {
            // Fits — return all.
            return messages.to_vec();
        }

        // Exceeds budget — keep most recent messages that fit
        let mut budget = Self::MAX_HISTORY_TOKENS - Self::SUMMARY_TOKEN_BUDGET;
        let mut keep_from = messages.len();
        for (i, msg) in messages.iter().enumerate().rev() {
            let tokens = Self::estimate_tokens(msg);
            if tokens > budget {
                break;
            }
            budget -= tokens;
            keep_from = i;
        }

        let dropped = keep_from;
        let mut result = Vec::with_capacity(messages.len() - keep_from + 1);

        if dropped > 0 {
            let dropped_tokens: usize = messages[..keep_from]
                .iter()
                .map(Self::estimate_tokens)
                .sum();
            let summary = Self::build_compacted_summary(&messages[..keep_from]);
            result.push(ChatMessage::system(format!(
                "[Compacted context]\n{}\n\n(Compacted {} earlier messages, ~{} tokens to fit context window. Full history remains persisted.)",
                summary,
                dropped,
                dropped_tokens,
            )));
        }

        result.extend_from_slice(&messages[keep_from..]);
        result
    }

    fn build_compacted_summary(messages: &[ChatMessage]) -> String {
        let mut lines = messages
            .iter()
            .rev()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .filter_map(|m| {
                let compact = m.content.replace('\n', " ").trim().to_string();
                if compact.is_empty() {
                    return None;
                }
                let truncated: String = compact.chars().take(160).collect();
                let snippet = if compact.chars().count() > 160 {
                    format!("{truncated}…")
                } else {
                    truncated
                };
                Some(format!("- {}: {}", m.role, snippet))
            })
            .take(12)
            .collect::<Vec<_>>();
        lines.reverse();
        if lines.is_empty() {
            "No non-empty user/assistant messages available for summary.".to_string()
        } else {
            lines.join("\n")
        }
    }

    /// Log an audit event if the audit store is configured.
    async fn audit(
        &self,
        kind: pares_agens_audit::event::EventKind,
        actor: &str,
        dest: &str,
        summary: &str,
    ) {
        if let Some(store) = &self.audit_store {
            let event =
                pares_agens_audit::event::AuditEvent::new(kind, actor, dest, summary, false);
            store.append(event).await;
        }
    }

    async fn dispatch_procedures(&self, event: &Event) -> Option<Event> {
        let mut last_response = None;
        for proc in self.procedure_registry.matching(event.kind()) {
            for result in proc.execute(event).await {
                if matches!(result, Event::ModelResponse { .. }) {
                    last_response = Some(result);
                }
            }
        }
        last_response
    }

    fn extract_domain_tags(&self, question: &str) -> Vec<String> {
        let lower = question.to_lowercase();
        let mut tags = Vec::new();

        for lang in [
            "rust",
            "python",
            "typescript",
            "javascript",
            "go",
            "c#",
            "java",
        ] {
            if lower.contains(lang) {
                tags.push(format!("lang:{lang}"));
            }
        }
        for tool in [
            "cargo",
            "tokio",
            "serde",
            "git",
            "docker",
            "kubernetes",
            "sql",
        ] {
            if lower.contains(tool) {
                tags.push(format!("tool:{tool}"));
            }
        }

        tags
    }

    fn looks_like_correction(&self, sentence: &str) -> bool {
        let lower = sentence.to_lowercase();
        lower.contains("you were wrong")
            || lower.contains("that's wrong")
            || lower.contains("that is wrong")
            || lower.contains("incorrect")
            || lower.contains("mistake")
            || lower.contains("sorry")
            || lower.contains("apologize")
    }

    fn extract_facts(&self, response: &str) -> Vec<String> {
        response
            .lines()
            .flat_map(|line| line.split(['.', '!', '?']))
            .map(|s| s.trim().trim_start_matches(['-', '*', '•']))
            .filter(|s| !s.is_empty())
            .filter(|s| !self.looks_like_correction(s))
            .map(|s| s.to_string())
            .collect()
    }

    async fn capture_exchange(&self, user: &str, assistant: &str) {
        if assistant.trim().is_empty() {
            return;
        }

        if let Some(plures_lm) = &self.plures_lm {
            let tags = self.extract_domain_tags(user);
            for fact in self.extract_facts(assistant) {
                if !passes_quality_gate(&fact) {
                    continue;
                }
                if let Err(e) = plures_lm.capture_fact(&fact, tags.clone()).await {
                    error!(error = %e, "agent: failed to capture fact in PluresLm");
                }
            }

            let exchange = Exchange {
                user: user.to_string(),
                assistant: assistant.to_string(),
            };
            if let Err(e) = plures_lm.capture(&exchange).await {
                error!(error = %e, "agent: failed to capture exchange in PluresLm");
            }
            return;
        }

        let combined = format!("User: {user}\nAssistant: {assistant}");
        if let Err(e) = self.memory.capture(&combined).await {
            error!(error = %e, "agent: failed to capture exchange in memory");
        }
    }

    fn resolve_branch_channel(&self, channel: &str) -> String {
        let guard = self.branch_state.lock().unwrap();
        let state = guard.get(channel).cloned().unwrap_or_default();
        Self::scoped_channel(channel, &state.active)
    }

    fn branch_label(channel: &str) -> String {
        channel
            .rsplit_once("::")
            .map_or_else(|| "main".to_string(), |(_, branch)| branch.to_string())
    }

    fn scoped_channel(channel: &str, branch: &str) -> String {
        if branch == "main" {
            channel.to_string()
        } else {
            format!("{channel}::{branch}")
        }
    }

    fn collect_command_target<'a, I>(parts: I) -> String
    where
        I: Iterator<Item = &'a str>,
    {
        parts.collect::<Vec<_>>().join(" ").trim().to_string()
    }

    async fn handle_branch_command(&self, id: &str, channel: &str, content: &str) -> Option<Event> {
        let trimmed = content.trim();
        if !trimmed.starts_with('/') {
            return None;
        }

        let mut parts = trimmed.split_whitespace();
        let cmd = parts
            .next()
            .unwrap_or("")
            .trim_start_matches('/')
            .split('@')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match cmd.as_str() {
            "session" => {
                let subcommand = parts
                    .next()
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .to_ascii_lowercase();
                match subcommand.as_str() {
                    "new" => {
                        let requested_name = Self::collect_command_target(parts.by_ref());
                        let requested_name = requested_name.trim();

                        let (new_branch, created) = {
                            let mut guard = self.branch_state.lock().unwrap();
                            let state = guard.entry(channel.to_string()).or_default();
                            let branch = if requested_name.is_empty() {
                                let mut idx = 1usize;
                                loop {
                                    let candidate = format!("session-{idx}");
                                    if !state.branches.contains(&candidate) {
                                        break candidate;
                                    }
                                    idx += 1;
                                }
                            } else {
                                requested_name.to_string()
                            };

                            let created = state.branches.insert(branch.clone());
                            state.active = branch.clone();
                            (branch, created)
                        };

                        let new_branch_channel = Self::scoped_channel(channel, &new_branch);

                        {
                            let mut history = self.conversation_history.lock().unwrap();
                            history.entry(new_branch_channel).or_default();
                        }

                        let action = if created {
                            "Created new"
                        } else {
                            "Switched to existing"
                        };
                        Some(Event::ModelResponse {
                            request_id: id.to_string(),
                            model: "command".into(),
                            content: format!(
                                "{action} session '{new_branch}'. Previous session was archived."
                            ),
                        })
                    }
                    "list" => {
                        let (active, branches) = {
                            let guard = self.branch_state.lock().unwrap();
                            let state = guard.get(channel).cloned().unwrap_or_default();
                            (state.active, state.branches)
                        };
                        let mut lines = vec![format!("Active session: {active}")];
                        lines.push("Sessions:".to_string());
                        for branch in branches {
                            if branch == active {
                                lines.push(format!("* {branch} (active)"));
                            } else {
                                lines.push(format!("* {branch}"));
                            }
                        }
                        Some(Event::ModelResponse {
                            request_id: id.to_string(),
                            model: "command".into(),
                            content: lines.join("\n"),
                        })
                    }
                    "switch" => {
                        let target = Self::collect_command_target(parts.by_ref());
                        if target.is_empty() {
                            return Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: "Usage: /session switch <id>".into(),
                            });
                        }

                        {
                            let mut guard = self.branch_state.lock().unwrap();
                            let state = guard.entry(channel.to_string()).or_default();
                            state.branches.insert(target.clone());
                            state.active = target.clone();
                        }

                        Some(Event::ModelResponse {
                            request_id: id.to_string(),
                            model: "command".into(),
                            content: format!("Switched to session '{target}'."),
                        })
                    }
                    _ => Some(Event::ModelResponse {
                        request_id: id.to_string(),
                        model: "command".into(),
                        content: "Usage: /session <new|list|switch> [id]".into(),
                    }),
                }
            }
            "branch" => {
                let requested_name = Self::collect_command_target(parts.by_ref());
                let requested_name = requested_name.trim();

                let current_branch_channel = self.resolve_branch_channel(channel);
                let current_branch = Self::branch_label(&current_branch_channel);
                let snapshot = self.load_history(&current_branch_channel).await;

                let (new_branch, created) = {
                    let mut guard = self.branch_state.lock().unwrap();
                    let state = guard.entry(channel.to_string()).or_default();
                    let mut branch = if requested_name.is_empty() {
                        let mut idx = 1usize;
                        loop {
                            let candidate = format!("branch-{idx}");
                            if !state.branches.contains(&candidate) {
                                break candidate;
                            }
                            idx += 1;
                        }
                    } else {
                        requested_name.to_string()
                    };

                    if branch.eq_ignore_ascii_case("main") {
                        branch = "main".to_string();
                    }

                    let created = state.branches.insert(branch.clone());
                    state.active = branch.clone();
                    (branch, created)
                };

                let new_branch_channel = Self::scoped_channel(channel, &new_branch);

                {
                    let mut history = self.conversation_history.lock().unwrap();
                    history.insert(new_branch_channel, snapshot);
                }

                let action = if created {
                    "Created"
                } else {
                    "Switched to existing"
                };
                Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "command".into(),
                    content: format!("{action} branch '{new_branch}' from '{current_branch}'."),
                })
            }
            "branches" => {
                let (active, branches) = {
                    let guard = self.branch_state.lock().unwrap();
                    let state = guard.get(channel).cloned().unwrap_or_default();
                    (state.active, state.branches)
                };
                let mut lines = vec![format!("Active branch: {active}")];
                lines.push("Branches:".to_string());
                for branch in branches {
                    if branch == active {
                        lines.push(format!("* {branch} (active)"));
                    } else {
                        lines.push(format!("* {branch}"));
                    }
                }
                Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "command".into(),
                    content: lines.join("\n"),
                })
            }
            "switch" => {
                let target = Self::collect_command_target(parts.by_ref());
                if target.is_empty() {
                    return Some(Event::ModelResponse {
                        request_id: id.to_string(),
                        model: "command".into(),
                        content: "Usage: /switch <branch>".into(),
                    });
                }

                let switched = {
                    let mut guard = self.branch_state.lock().unwrap();
                    let state = guard.entry(channel.to_string()).or_default();
                    if state.branches.contains(&target) {
                        state.active = target.clone();
                        true
                    } else {
                        false
                    }
                };

                let message = if switched {
                    format!("Switched to branch '{target}'.")
                } else {
                    format!(
                        "Branch '{target}' not found. Use /branches to list available branches."
                    )
                };

                Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "command".into(),
                    content: message,
                })
            }
            "clear" => {
                let (previous_session, new_session) = {
                    let mut guard = match self.branch_state.lock() {
                        Ok(guard) => guard,
                        Err(e) => {
                            error!(
                                error = %e,
                                channel,
                                "failed to acquire branch_state lock for /clear"
                            );
                            return Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: "Failed to clear conversation context due to internal state error.".into(),
                            });
                        }
                    };
                    let state = guard.entry(channel.to_string()).or_default();
                    let previous = state.active.clone();
                    let mut idx = 1usize;
                    let session = loop {
                        let candidate = format!("session-{idx}");
                        if !state.branches.contains(&candidate) {
                            break candidate;
                        }
                        idx += 1;
                    };
                    state.branches.insert(session.clone());
                    state.active = session.clone();
                    (previous, session)
                };

                let new_session_channel = Self::scoped_channel(channel, &new_session);
                {
                    match self.conversation_history.lock() {
                        Ok(mut history) => {
                            history.entry(new_session_channel).or_default();
                        }
                        Err(e) => {
                            error!(
                                error = %e,
                                channel,
                                "failed to acquire conversation_history lock for /clear"
                            );
                            return Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: "Failed to clear conversation context due to internal state error."
                                    .into(),
                            });
                        }
                    }
                }

                info!(
                    channel,
                    from_session = previous_session.as_str(),
                    to_session = new_session.as_str(),
                    trigger = "/clear",
                    "conversation session transitioned"
                );

                Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "command".into(),
                    content: format!(
                        "Cleared conversation context. Started new session '{new_session}'."
                    ),
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::store::InMemoryStore as InMemoryTurnStore;
    use crate::model::{ChatOptions, ModelCompletion, ToolDefinition};
    use serde_json::json;

    fn msg(content: &str) -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: content.into(),
        }
    }

    struct MockModel;

    #[async_trait]
    impl ModelClient for MockModel {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            let last_user = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(ModelCompletion {
                content: Some(format!("Echo: {last_user}")),
                tool_calls: vec![],
                logprobs: None,
            })
        }
    }

    struct MockTools;

    #[async_trait]
    impl ToolDispatcher for MockTools {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "noop".into(),
                description: "noop".into(),
                parameters: json!({"type": "object"}),
            }]
        }

        async fn call_tool(&self, _name: &str, _arguments: serde_json::Value) -> String {
            "ok".into()
        }
    }

    #[tokio::test]
    async fn agent_returns_model_response() {
        let agent = Agent::new(Arc::new(InMemory::new())).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );
        let response = agent.handle_event(msg("hello")).await;
        assert!(
            matches!(response, Some(Event::ModelResponse { ref content, .. }) if content == "Echo: hello")
        );
    }

    #[tokio::test]
    async fn agent_captures_exchange() {
        let memory = Arc::new(InMemory::new());
        let agent = Agent::new(Arc::clone(&memory) as Arc<dyn Memory + Send + Sync>).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );
        agent.handle_event(msg("remember this")).await;
        let recalled = memory.recall("remember").await.unwrap();
        assert!(recalled.iter().any(|entry| entry.contains("remember this")));
    }

    #[tokio::test]
    async fn agent_ignores_non_message_events() {
        let agent = Agent::new(Arc::new(InMemory::new())).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );
        let timer = Event::Timer {
            id: "t1".into(),
            name: "tick".into(),
            recurring: false,
        };
        let response = agent.handle_event(timer).await;
        assert!(response.is_none());
    }

    #[tokio::test]
    async fn in_memory_recall_returns_matching_entries() {
        let mem = InMemory::new();
        mem.capture("hello world").await.unwrap();
        mem.capture("goodbye world").await.unwrap();
        mem.capture("unrelated").await.unwrap();
        let results = mem.recall("hello").await.unwrap();
        assert_eq!(results, vec!["hello world"]);
    }

    #[tokio::test]
    async fn in_memory_recall_case_insensitive() {
        let mem = InMemory::new();
        mem.capture("Hello World").await.unwrap();
        let results = mem.recall("hello").await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn in_memory_recall_empty_when_no_match() {
        let mem = InMemory::new();
        mem.capture("something else").await.unwrap();
        let results = mem.recall("hello").await.unwrap();
        assert!(results.is_empty());
    }

    // ── Cerebellum-aware agent tests ─────────────────────────────────────

    fn make_agent_with_cerebellum() -> Agent {
        use crate::cerebellum::{Cerebellum, CerebellumConfig};
        use crate::memory::{embed::MockEmbedder, store::InMemoryStore, PluresLm};

        let store = Arc::new(InMemoryStore::new());
        let plures_lm = Arc::new(PluresLm::new(
            store as Arc<dyn crate::memory::store::MemoryStore>,
            Box::new(MockEmbedder),
            128_000,
        ));
        let cerebellum = Cerebellum::new(CerebellumConfig::default());
        Agent::with_cerebellum(Arc::new(InMemory::new()), cerebellum, plures_lm).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        )
    }

    #[tokio::test]
    async fn agent_with_cerebellum_returns_response_for_conscious_route() {
        let agent = make_agent_with_cerebellum();
        // Short message → Conscious route → response returned.
        let response = agent.handle_event(msg("push now")).await;
        assert!(
            matches!(response, Some(Event::ModelResponse { .. })),
            "expected ModelResponse for Conscious route"
        );
    }

    #[tokio::test]
    async fn agent_with_cerebellum_drops_noise_messages() {
        let agent = make_agent_with_cerebellum();
        // Single-word ack "ok" → Route::Drop → None.
        let response = agent.handle_event(msg("ok")).await;
        assert!(response.is_none(), "expected None for Route::Drop");
    }

    #[tokio::test]
    async fn agent_with_cerebellum_injects_learned_context_when_memories_exist() {
        use crate::cerebellum::{Cerebellum, CerebellumConfig};
        use crate::memory::{
            embed::{EmbeddingProvider, MockEmbedder},
            entry::{MemoryCategory, MemoryEntry},
            store::{InMemoryStore, MemoryStore as _},
            PluresLm,
        };

        let store = Arc::new(InMemoryStore::new());
        // Pre-populate with a memory related to async Rust so the cerebellum
        // can recall it when asked "How do I use async in Rust?".
        let embedding = MockEmbedder
            .embed("Use tokio for async Rust tasks")
            .await
            .unwrap();
        store
            .insert(MemoryEntry {
                id: "m1".into(),
                content: "Use tokio for async Rust tasks".into(),
                category: MemoryCategory::CodePattern,
                tags: vec![],
                embedding,
                score: 0.9,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .await
            .unwrap();

        let plures_lm = Arc::new(PluresLm::new(
            Arc::clone(&store) as Arc<dyn crate::memory::store::MemoryStore>,
            Box::new(MockEmbedder),
            128_000,
        ));
        let cerebellum = Cerebellum::new(CerebellumConfig::default());
        let agent = Agent::with_cerebellum(Arc::new(InMemory::new()), cerebellum, plures_lm)
            .with_model(
                Arc::new(MockModel),
                Arc::new(MockTools),
                "You are a test agent.".into(),
            );

        let event = Event::Message {
            id: "q1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: "How do I use async in Rust?".into(),
        };
        let response = agent.handle_event(event).await;
        if let Some(Event::ModelResponse { content, .. }) = response {
            assert!(
                content.contains("Echo: How do I use async in Rust?"),
                "expected model response, got: {content}"
            );
        } else {
            panic!("expected ModelResponse with recalled context");
        }
    }

    #[tokio::test]
    async fn branch_commands_create_list_and_switch() {
        let agent = Agent::new(Arc::new(InMemory::new())).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );

        let branch_response = agent.handle_event(msg("/branch alt")).await;
        assert!(
            matches!(branch_response, Some(Event::ModelResponse { ref content, .. }) if content.contains("branch 'alt'"))
        );

        let list_response = agent.handle_event(msg("/branches")).await;
        assert!(matches!(
            list_response,
            Some(Event::ModelResponse { ref content, .. })
            if content.contains("Active branch: alt")
                && content.contains("* main")
                && content.contains("* alt (active)")
        ));

        let switch_response = agent.handle_event(msg("/switch main")).await;
        assert!(matches!(
            switch_response,
            Some(Event::ModelResponse { ref content, .. }) if content == "Switched to branch 'main'."
        ));
    }

    #[tokio::test]
    async fn branch_turns_are_persisted_to_separate_channels() {
        let turn_store = Arc::new(InMemoryTurnStore::new());
        let agent = Agent::new(Arc::new(InMemory::new()))
            .with_model(
                Arc::new(MockModel),
                Arc::new(MockTools),
                "You are a test agent.".into(),
            )
            .with_turn_store(turn_store.clone() as Arc<dyn crate::memory::store::MemoryStore>);

        let _ = agent.handle_event(msg("main path")).await;
        let _ = agent.handle_event(msg("/branch alt")).await;
        let _ = agent.handle_event(msg("alt path")).await;
        let _ = agent.handle_event(msg("/switch main")).await;
        let _ = agent.handle_event(msg("main again")).await;

        let main_turns = turn_store.recent_turns("test", 10).await.unwrap();
        let alt_turns = turn_store.recent_turns("test::alt", 10).await.unwrap();

        assert_eq!(main_turns.len(), 2, "main should keep its own turn chain");
        assert_eq!(alt_turns.len(), 1, "branch should have its own turn chain");
        assert!(main_turns
            .iter()
            .flat_map(|t| t.messages.iter())
            .any(|m| m.content.contains("main path")));
        assert!(alt_turns
            .iter()
            .flat_map(|t| t.messages.iter())
            .any(|m| m.content.contains("alt path")));
        assert!(main_turns.iter().all(|t| t.session_id == "main"));
        assert!(alt_turns.iter().all(|t| t.session_id == "alt"));
    }

    #[tokio::test]
    async fn session_commands_create_list_switch_and_start_fresh() {
        let agent = Agent::new(Arc::new(InMemory::new())).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );

        let _ = agent.handle_event(msg("main path")).await;

        let new_response = agent.handle_event(msg("/session new work")).await;
        assert!(matches!(
            new_response,
            Some(Event::ModelResponse { ref content, .. }) if content.contains("session 'work'")
        ));

        let work_history = agent.load_history("test::work").await;
        assert!(
            work_history.is_empty(),
            "new session should start with fresh context"
        );

        let list_response = agent.handle_event(msg("/session list")).await;
        assert!(matches!(
            list_response,
            Some(Event::ModelResponse { ref content, .. })
            if content.contains("Active session: work")
                && content.contains("* main")
                && content.contains("* work (active)")
        ));

        let switch_response = agent.handle_event(msg("/session switch main")).await;
        assert!(matches!(
            switch_response,
            Some(Event::ModelResponse { ref content, .. }) if content == "Switched to session 'main'."
        ));
    }

    #[tokio::test]
    async fn clear_command_starts_fresh_session_without_deleting_turns() {
        let turn_store = Arc::new(InMemoryTurnStore::new());
        let agent = Agent::new(Arc::new(InMemory::new()))
            .with_model(
                Arc::new(MockModel),
                Arc::new(MockTools),
                "You are a test agent.".into(),
            )
            .with_turn_store(turn_store.clone() as Arc<dyn crate::memory::store::MemoryStore>);

        let _ = agent.handle_event(msg("main path")).await;
        let clear_response = agent.handle_event(msg("/clear")).await;
        assert!(matches!(
            clear_response,
            Some(Event::ModelResponse { ref content, .. })
            if content == "Cleared conversation context. Started new session 'session-1'."
        ));

        let _ = agent.handle_event(msg("fresh path")).await;

        let main_turns = turn_store.recent_turns("test", 10).await.unwrap();
        let cleared_turns = turn_store
            .recent_turns("test::session-1", 10)
            .await
            .unwrap();
        assert_eq!(
            main_turns.len(),
            1,
            "clear should keep prior main turns intact"
        );
        assert_eq!(
            cleared_turns.len(),
            1,
            "clear should route follow-up messages into fresh session history"
        );
    }

    #[test]
    fn trim_to_token_budget_adds_compacted_summary_block() {
        let mut messages = Vec::new();
        for i in 0..260 {
            messages.push(ChatMessage::user(format!(
                "user-{i}: {}",
                "x".repeat(2_000)
            )));
            messages.push(ChatMessage::assistant(format!(
                "assistant-{i}: {}",
                "y".repeat(2_000)
            )));
        }

        let trimmed = Agent::trim_to_token_budget(&messages);
        assert!(
            trimmed.len() < messages.len(),
            "expected compaction when token budget is exceeded"
        );
        assert_eq!(trimmed[0].role, "system");
        assert!(
            trimmed[0].content.contains("[Compacted context]"),
            "expected compacted context note, got: {}",
            trimmed[0].content
        );
    }
}
