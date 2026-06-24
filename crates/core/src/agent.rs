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
use pares_radix_core::chronos::{ChronosAction, ChronosTimeline};
use crate::delegation::aggregator::ResultAggregator;
use crate::delegation::broker::DelegationBroker;
use pares_radix_core::event::Event;
use crate::memory::entry::Exchange;
use crate::memory::store::MemoryStore;
use crate::memory::{passes_quality_gate, PluresLm};
use pares_radix_core::model::{
    ChatMessage, ChatOptions, ModelClient, StreamDelta, StreamSender, ToolDispatcher,
};
use pares_radix_core::pii_guard::PiiGuard;
use pares_radix_core::plugins::hooks::{HookAction, HookContext, HookManager, HookPoint};
use pares_radix_core::procedure::ProcedureRegistry;
use pares_radix_core::session::{SessionManager, SessionMetadata};
use pares_radix_core::state::StateStore;

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
    /// Optional fast model client for simple responses.
    fast_model_client: Option<Arc<dyn ModelClient>>,
    /// Tool dispatcher for model tool calls.
    tool_dispatcher: Option<Arc<dyn ToolDispatcher>>,
    /// Base system prompt (legacy fallback).
    system_prompt: String,
    /// Personality contract for dynamic prompt building.
    personality: Option<crate::personality::PersonalityContract>,
    /// Current channel name (e.g. "telegram").
    current_channel: std::sync::Mutex<Option<String>>,
    /// Per-channel conversation history keyed by channel/session label.
    conversation_history: Arc<Mutex<HashMap<String, Vec<ChatMessage>>>>,
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
    /// Hook manager for plugin lifecycle intercepts.
    hook_manager: Arc<HookManager>,
    /// Session manager for cross-restart persistence.
    session_manager: Option<Arc<SessionManager>>,
    /// Chronos timeline for tool execution auditing.
    chronos: Option<Arc<ChronosTimeline>>,
    /// PII guard for redacting sensitive data before model calls.
    pii_guard: PiiGuard,
    /// Optional state store for persisting agent state (promises, preferences).
    state_store: Option<Arc<dyn StateStore>>,
    /// Optional task manager for autonomous work execution.
    task_manager: Option<Arc<pares_radix_core::task_manager::TaskManager>>,
    // Telemetry logger for interaction tracking.
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
            fast_model_client: None,
            tool_dispatcher: None,
            system_prompt: String::new(),
            personality: None,
            current_channel: std::sync::Mutex::new(None),
            conversation_history: Arc::new(Mutex::new(HashMap::new())),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
            branch_state: Mutex::new(HashMap::new()),
            personality_documents_cache: Mutex::new(None),
            plugin_context: Mutex::new(None),
            hook_manager: Arc::new(HookManager::new()),
            session_manager: None,
            chronos: None,
            pii_guard: PiiGuard::new(),
            state_store: None,
            task_manager: None,
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
            fast_model_client: None,
            tool_dispatcher: None,
            system_prompt: String::new(),
            personality: None,
            current_channel: std::sync::Mutex::new(None),
            conversation_history: Arc::new(Mutex::new(HashMap::new())),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
            branch_state: Mutex::new(HashMap::new()),
            personality_documents_cache: Mutex::new(None),
            plugin_context: Mutex::new(None),
            hook_manager: Arc::new(HookManager::new()),
            session_manager: None,
            chronos: None,
            pii_guard: PiiGuard::new(),
            state_store: None,
            task_manager: None,
        }
    }

    /// Attach a session manager for cross-restart session persistence.
    pub fn with_session_manager(mut self, manager: Arc<SessionManager>) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Attach a Chronos timeline for tool execution auditing.
    pub fn with_chronos(mut self, chronos: Arc<ChronosTimeline>) -> Self {
        self.chronos = Some(chronos);
        self
    }

    /// Attach a state store for persisting agent state.
    pub fn with_state_store(mut self, state: Arc<dyn StateStore>) -> Self {
        self.state_store = Some(state);
        self
    }

    /// Attach a task manager for autonomous work execution.
    pub fn with_task_manager(mut self, task_manager: Arc<pares_radix_core::task_manager::TaskManager>) -> Self {
        self.task_manager = Some(task_manager);
        self
    }

    /// Get a reference to the hook manager for plugin registration.
    pub fn hook_manager(&self) -> &Arc<HookManager> {
        &self.hook_manager
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
    pub fn with_personality(
        mut self,
        personality: crate::personality::PersonalityContract,
    ) -> Self {
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

    /// Attach a fast model client for simple/short responses.
    pub fn with_fast_model(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.fast_model_client = Some(client);
        self
    }

    /// Select the appropriate model client for a request based on context size and complexity.
    ///
    /// Algorithm:
    /// 1. Context size is the GATE - if a tier's model can't fit the context, skip it
    /// 2. Complexity is the SELECTOR - pick the cheapest tier that handles the task
    /// 3. Failover is handled at the client level (fallback chains per tier)
    ///
    /// Context size gate: estimate total tokens, reject models where
    /// context_window < total_tokens * 1.3 (30% headroom for response generation).
    fn select_model_for_request(&self, content: &str) -> Option<&Arc<dyn ModelClient>> {
        let word_count = content.split_whitespace().count();
        let complexity = Self::estimate_complexity(content, word_count);

        // Rough token estimate: ~1.3 tokens per word for English text.
        // This doesn't include system prompt or history - those are managed
        // separately by the context window manager. This is just the MESSAGE
        // itself as a quick filter.
        let estimated_message_tokens = (word_count as u64).saturating_mul(13) / 10;

        tracing::info!(
            words = word_count,
            complexity = complexity,
            est_tokens = estimated_message_tokens,
            has_fast = self.fast_model_client.is_some(),
            has_deep = self.deep_model_client.is_some(),
            "model selection: context-size-gated complexity routing"
        );

        // Context-size gate check for a model client.
        // Returns true if the model's window can accommodate this message
        // (with headroom). Note: full context (history + system prompt) is
        // managed elsewhere - this prevents obviously-too-large messages
        // from being sent to small-context models.
        let fits_context = |client: &Arc<dyn ModelClient>| -> bool {
            match client.context_window() {
                Some(window) => {
                    // 30% headroom for system prompt, history, and response
                    let required = estimated_message_tokens.saturating_mul(13) / 10;
                    window >= required
                }
                None => true, // Unknown window = don't gate
            }
        };

        // Tier selection based on complexity score:
        // 0-1: Fast tier (simple follow-ups, acknowledgments, short factual)
        // 2-3: Standard tier (moderate questions, tool use, summaries)
        // 4+:  Premium tier (complex reasoning, multi-step, design, comparison)
        //
        // If the preferred tier can't fit the context, cascade UP (larger models
        // have larger windows). Never cascade DOWN for context overflow.
        match complexity {
            0..=1 => {
                // Try fast first (if it fits), fall back to standard, then deep
                if let Some(ref fast) = self.fast_model_client {
                    if fits_context(fast) {
                        tracing::info!("routing to Fast tier (complexity={})", complexity);
                        return Some(fast);
                    }
                    tracing::info!("fast model context too small, escalating to standard");
                }
                // Standard as fallback
                if let Some(ref standard) = self.model_client {
                    if fits_context(standard) {
                        return Some(standard);
                    }
                }
                // Deep as last resort (largest context)
                self.deep_model_client.as_ref()
            }
            2..=3 => {
                // Standard tier, escalate to deep if context overflows
                if let Some(ref standard) = self.model_client {
                    if fits_context(standard) {
                        tracing::info!("routing to Standard tier (complexity={})", complexity);
                        return Some(standard);
                    }
                    tracing::info!("standard model context too small, escalating to premium");
                }
                // Deep as fallback for large context
                self.deep_model_client.as_ref().or(self.model_client.as_ref())
            }
            _ => {
                // Premium tier directly - these have the largest context windows
                if let Some(ref deep) = self.deep_model_client {
                    tracing::info!("routing to Premium tier (complexity={})", complexity);
                    Some(deep)
                } else {
                    self.model_client.as_ref()
                }
            }
        }
    }

    /// Estimate request complexity from content signals.
    /// Returns a score 0-6 where higher = more complex.
    ///
    /// This is intentionally heuristic - no embedding or model call needed.
    /// The goal is: short contextual follow-ups → 0-1, moderate questions → 2-3,
    /// complex analytical/design/comparison tasks → 4+.
    fn estimate_complexity(content: &str, word_count: usize) -> u8 {
        let mut score: u8 = 0;
        let lower = content.to_lowercase();

        // Length signal: very short messages are almost always simple
        if word_count <= 3 {
            return 0; // "yes", "do it", "thanks", "ok cool"
        }
        if word_count <= 8 {
            score += 1; // Short but might have substance
        } else if word_count <= 30 {
            score += 2; // Medium - could be anything
        } else {
            score += 2; // Long context doesn't mean complex (could be pasting a log)
        }

        // Reasoning indicators: questions that require synthesis/analysis
        let reasoning_words = ["why", "how", "compare", "design", "explain",
            "analyze", "evaluate", "trade-off", "tradeoff", "architect",
            "what are the implications", "pros and cons", "difference between"];
        for word in &reasoning_words {
            if lower.contains(word) {
                score += 1;
                break; // Only count once
            }
        }

        // Multi-step indicators
        let multi_step_markers = ["first", "then", "after that", "finally",
            "step 1", "step 2", "also", "additionally"];
        let multi_step_count = multi_step_markers.iter()
            .filter(|m| lower.contains(*m))
            .count();
        if multi_step_count >= 2 {
            score += 1;
        }

        // Code/technical complexity
        if content.contains('`') || content.contains("fn ") || content.contains("def ")
            || content.contains("impl ") || content.contains("class ")
            || content.contains("struct ") {
            score += 1;
        }

        // Multi-clause structure (compound questions)
        let clause_separators = content.matches(';').count()
            + content.matches(" and ").count()
            + content.matches(" but ").count()
            + content.matches(" or ").count();
        if clause_separators >= 3 {
            score += 1;
        }

        score.min(6) // Cap at 6
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

    // Telemetry: attach from PARES_TELEMETRY_DIR env var (no-op if unset).

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

        // ── Full Dataflow Pipeline (preferred path) ─────────────────────────
        // If the dataflow bridge is active, run the entire pipeline through .px
        // procedures. This handles routing, context, model invocation, tool loops,
        // and delivery all within the PluresDB dataflow graph.
        if let Some(cerebellum) = &self.cerebellum {
            if let Event::Message { ref id, ref sender, ref content, ref channel, .. } = event {
                // Parse chat_id from message id (format: "chat_id:msg_id" or just numeric)
                let chat_id = id.split(':').next()
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(0);
                // Load conversation history so the dataflow pipeline has multi-turn context
                let session_channel = self.resolve_branch_channel(channel);
                let history = self.load_history(&session_channel).await;
                if let Some(delivery) = cerebellum.try_full_dataflow(
                    chat_id,
                    sender,
                    content,
                    Some(id.as_str()),
                    if history.is_empty() { None } else { Some(&history) },
                ).await {
                    // Persist the exchange in history so future turns have context
                    let session_id = Self::branch_label(&session_channel);
                    let new_messages = vec![
                        ChatMessage { role: "user".into(), content: content.clone(), tool_call_id: None, tool_calls: None },
                        ChatMessage { role: "assistant".into(), content: delivery.content.clone(), tool_call_id: None, tool_calls: None },
                    ];
                    self.persist_turn(&session_channel, &session_id, &new_messages).await;

                    // Full pipeline handled - package as ModelResponse
                    return Some(Event::ModelResponse {
                        request_id: request_id.to_string(),
                        content: delivery.content,
                        model: "dataflow".to_string(),
                    });
                }
            }
        }

        // ── Legacy Cerebellum: autorecall + routing ──────────────────────────
        // Falls through here when dataflow bridge isn't active or didn't produce output.
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

        // Log routing decision (Chronos recording disabled - causes sled deadlock in async context)
        info!(route = ?route, event_kind = event.kind(), context_len = learned_context.len(), "cerebellum routing decision");

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
                    info!("handle_event: routing to Procedural");
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
                Route::Fast | Route::Conscious | Route::Deep { .. } => {
                    info!(
                        id,
                        channel,
                        route = ?route,
                        "handle_event: routing to model (Fast/Conscious/Deep)"
                    );
                    self.handle_model_message(id, channel, content, &learned_context, clear_history)
                        .await
                }
                Route::Drop => {
                    info!(
                        id,
                        channel, "handle_event: Route::Drop - message suppressed"
                    );
                    None
                }
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

    /// Handle a single event with real-time streaming support.
    ///
    /// When `stream_tx` is provided, content tokens from the model's first
    /// completion turn are forwarded as [`StreamDelta`] events before the
    /// full response is assembled. This enables live token-by-token UI updates.
    ///
    /// Falls back to non-streaming for procedural routes, delegations, and
    /// subsequent tool-loop turns.
    pub async fn handle_event_streaming(
        &self,
        event: Event,
        stream_tx: StreamSender,
    ) -> Option<Event> {
        let request_id = Uuid::new_v4();
        let _event_start = Instant::now();
        info!(%request_id, event_kind = %event.kind(), "received event (streaming)");
        if let Event::Message {
            ref id,
            ref channel,
            ref content,
            ..
        } = event
        {
            if let Some(command_response) = self.handle_branch_command(id, channel, content).await {
                let _ = stream_tx.send(StreamDelta::Done);
                return Some(command_response);
            }
        }

        // Cerebellum preprocessing (same as handle_event)
        let (route, learned_context, clear_history) =
            if let (Some(cerebellum), Some(plures_lm)) = (&self.cerebellum, &self.plures_lm) {
                match cerebellum
                    .preprocess(&event, plures_lm, &self.procedure_registry)
                    .await
                {
                    Ok(ctx) => {
                        if ctx.route == Route::Drop {
                            let _ = stream_tx.send(StreamDelta::Done);
                            return None;
                        }
                        (ctx.route, ctx.learned_context, ctx.clear_history)
                    }
                    Err(e) => {
                        error!(error = %e, "cerebellum preprocess failed (streaming)");
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
            let _ = stream_tx.send(StreamDelta::Done);
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
                    let _ = stream_tx.send(StreamDelta::Done);
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
                        let _ = stream_tx.send(StreamDelta::Done);
                        delegated
                    } else {
                        self.handle_model_message_streaming(
                            id,
                            channel,
                            content,
                            &learned_context,
                            clear_history,
                            stream_tx,
                        )
                        .await
                    }
                }
                Route::Fast | Route::Conscious | Route::Deep { .. } => {
                    self.handle_model_message_streaming(
                        id,
                        channel,
                        content,
                        &learned_context,
                        clear_history,
                        stream_tx,
                    )
                    .await
                }
                Route::Drop => {
                    let _ = stream_tx.send(StreamDelta::Done);
                    None
                }
            },
            Event::Timer { .. } | Event::StateChange { .. } => {
                let _ = stream_tx.send(StreamDelta::Done);
                if matches!(route, Route::Procedural) {
                    self.dispatch_procedures(&event).await
                } else {
                    None
                }
            }
            _ => {
                let _ = stream_tx.send(StreamDelta::Done);
                None
            }
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
        self.handle_model_message_inner(id, channel, content, learned_context, clear_history, None)
            .await
    }

    async fn handle_model_message_streaming(
        &self,
        id: &str,
        channel: &str,
        content: &str,
        learned_context: &str,
        clear_history: bool,
        stream_tx: StreamSender,
    ) -> Option<Event> {
        self.handle_model_message_inner(
            id,
            channel,
            content,
            learned_context,
            clear_history,
            Some(stream_tx),
        )
        .await
    }

    async fn handle_model_message_inner(
        &self,
        id: &str,
        channel: &str,
        content: &str,
        learned_context: &str,
        clear_history: bool,
        stream_tx: Option<StreamSender>,
    ) -> Option<Event> {
        info!(id, channel, "handle_model_message: starting model call");
        let session_channel = self.resolve_branch_channel(channel);
        let session_id = Self::branch_label(&session_channel);

        // Dynamic model selection based on context size + complexity.
        // Context size is the GATE (hard constraint), complexity is the SELECTOR.
        let effective_client = self.select_model_for_request(content);
        let model_client = match effective_client {
            Some(client) => client,
            None => {
                warn!("agent: no model client available for request");
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
            model: None,
        };

        let model_start = std::time::Instant::now();
        let (mut reply, logprobs, mut messages) = match self
            .run_model_loop(
                model_client,
                tool_dispatcher,
                base_system_text,
                &history_snapshot,
                content,
                &options,
                stream_tx.as_ref(),
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
        let model_elapsed = model_start.elapsed();
        tracing::info!(model_ms = model_elapsed.as_millis(), "model loop complete");

        let mut model_label = "model";
        if self.is_low_confidence(logprobs.as_deref()) {
            if let Some(deep_client) = &self.deep_model_client {
                let deep_system_text = self.build_system_prompt(learned_context, true);
                let deep_options = ChatOptions {
                    temperature: None,
                    logprobs: false,
                    model: None,
                };
                match self
                    .run_model_loop(
                        deep_client,
                        tool_dispatcher,
                        deep_system_text,
                        &history_snapshot,
                        content,
                        &deep_options,
                        None, // no streaming for deep fallback
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

        // Persist session state for /resume support.
        if let Some(session_mgr) = &self.session_manager {
            let all_history = self.load_history(&session_channel).await;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let metadata = SessionMetadata {
                started_at: now, // approximation; first message time would be better
                last_message_at: now,
                message_count: all_history.len(),
                topic_summary: None,
            };
            session_mgr
                .save_session(channel, &all_history, metadata)
                .await;
        }

        self.capture_exchange(content, &reply).await;
        self.spawn_procedure_writer(content, &reply);
        self.detect_and_store_promises(content, &reply).await;

        // Log interaction to Chronos (PluresDB + JSONL)
        if let Some(ref chronos) = self.chronos {
            let entry = chronos.build_entry(
                &format!("agent:interaction:{channel}"),
                "agent",
                pares_radix_core::chronos::ChronosAction::ResponseGenerated,
                &serde_json::json!({
                    "user_message": content,
                    "response": &reply,
                    "response_len": reply.len(),
                    "model": model_label,
                }),
                vec![],
                Some(format!(
                    "response to: {}",
                    &content[..content.len().min(80)]
                )),
            );
            chronos.record(&entry);
        }

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
            let docs_cache = self
                .personality_documents_cache
                .lock()
                .ok()
                .and_then(|g| g.clone());
            let plugin_cache = self.plugin_context.lock().ok().and_then(|g| g.clone());
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

    #[allow(clippy::too_many_arguments)]
    async fn run_model_loop(
        &self,
        model_client: &Arc<dyn ModelClient>,
        tool_dispatcher: &Arc<dyn ToolDispatcher>,
        system_text: String,
        history_snapshot: &[ChatMessage],
        content: &str,
        options: &ChatOptions,
        stream_tx: Option<&StreamSender>,
    ) -> Result<(String, Option<Vec<f64>>, Vec<ChatMessage>), String> {
        let mut messages = Vec::with_capacity(history_snapshot.len() + 2);
        messages.push(ChatMessage::system(system_text));
        messages.extend(history_snapshot.iter().cloned());
        messages.push(ChatMessage::user(content));

        let tools = tool_dispatcher.available_tools().await;

        // Fire OnMessage hook.
        let mut msg_ctx = HookContext {
            message_text: Some(content.to_string()),
            ..Default::default()
        };
        if let HookAction::Block(reason) =
            self.hook_manager.fire(HookPoint::OnMessage, &mut msg_ctx)
        {
            return Err(format!("Message blocked by hook: {reason}"));
        }

        let mut final_reply = None;
        let mut final_logprobs = None;
        for turn in 0..10 {
            // Fire PreModelCall hook.
            let mut pre_model_ctx = HookContext {
                model_prompt: messages.last().map(|m| m.content.clone()),
                ..Default::default()
            };
            match self
                .hook_manager
                .fire(HookPoint::PreModelCall, &mut pre_model_ctx)
            {
                HookAction::Block(reason) => {
                    return Err(format!("Model call blocked by hook: {reason}"))
                }
                HookAction::InjectContext(text) => {
                    // Prepend injected context to the system message.
                    if let Some(sys) = messages.first_mut() {
                        if sys.role == "system" {
                            sys.content.push_str("\n\n");
                            sys.content.push_str(&text);
                        }
                    }
                }
                _ => {}
            }

            // PII guard: redact sensitive data from user messages before model call.
            let messages_for_model: Vec<ChatMessage> = messages.iter().map(|m| {
                if m.role == "user" {
                    let (redacted, report) = self.pii_guard.redact(&m.content);
                    if report.count > 0 {
                        info!(redactions = ?report.redactions, "PII guard redacted sensitive data");
                        if let Some(chronos) = &self.chronos {
                            let entry = chronos.build_entry(
                                "pii:redaction",
                                "pii_guard",
                                ChronosAction::Create,
                                &serde_json::json!({"redactions": report.redactions, "count": report.count}),
                                vec![],
                                Some(format!("PII redaction: {} items", report.count)),
                            );
                            chronos.record(&entry);
                        }
                    }
                    ChatMessage { content: redacted, ..m.clone() }
                } else {
                    m.clone()
                }
            }).collect();

            let model_start = Instant::now();
            info!(
                turn,
                message_count = messages_for_model.len(),
                tool_count = tools.len(),
                "ABOUT TO CALL model_client.complete"
            );
            // Use streaming when a StreamSender is provided (first turn only -
            // subsequent tool-loop turns use non-streaming since the UI already
            // shows the tool execution phase).
            let completion = if let (Some(tx), 0) = (stream_tx, turn) {
                model_client
                    .complete_stream(&messages_for_model, &tools, options, tx.clone())
                    .await?
            } else {
                model_client
                    .complete(&messages_for_model, &tools, options)
                    .await?
            };
            let latency_ms = model_start.elapsed().as_millis();
            info!(
                turn,
                latency_ms,
                tool_calls = completion.tool_calls.len(),
                "model completion received"
            );

            // Fire PostModelCall hook.
            let mut post_model_ctx = HookContext {
                model_response: completion.content.clone(),
                ..Default::default()
            };
            self.hook_manager
                .fire(HookPoint::PostModelCall, &mut post_model_ctx);

            if !completion.tool_calls.is_empty() {
                let tool_calls = completion.tool_calls.clone();
                messages.push(ChatMessage {
                    role: "assistant".into(),
                    content: completion.content.unwrap_or_default(),
                    tool_call_id: None,
                    tool_calls: Some(tool_calls.clone()),
                });

                for tool_call in tool_calls {
                    // Fire PreToolUse hook.
                    let mut pre_ctx = HookContext {
                        tool_name: Some(tool_call.name.clone()),
                        tool_args: Some(tool_call.arguments.clone()),
                        ..Default::default()
                    };
                    match self.hook_manager.fire(HookPoint::PreToolUse, &mut pre_ctx) {
                        HookAction::Block(reason) => {
                            messages.push(ChatMessage::tool_result(
                                tool_call.id,
                                format!("Tool blocked by hook: {reason}"),
                            ));
                            continue;
                        }
                        HookAction::ModifyContext(new_args) => {
                            let call_id = Uuid::new_v4().to_string();
                            if let Some(chronos) = &self.chronos {
                                let entry = chronos.build_entry(
                                    &format!("tool:call:{}", call_id),
                                    "agent",
                                    ChronosAction::Create,
                                    &serde_json::json!({"tool": &tool_call.name, "args_modified": true}),
                                    vec![],
                                    Some(format!("Tool call: {}", tool_call.name)),
                                );
                                chronos.record(&entry);
                            }
                            let tool_start = Instant::now();
                            let tool_result =
                                tool_dispatcher.call_tool(&tool_call.name, new_args).await;
                            let elapsed_ms = tool_start.elapsed().as_millis();
                            if let Some(chronos) = &self.chronos {
                                let success = !tool_result.starts_with("Error")
                                    && !tool_result.starts_with("⚠");
                                let entry = chronos.build_entry(
                                    &format!("tool:result:{}", call_id),
                                    "agent",
                                    ChronosAction::Update,
                                    &serde_json::json!({"success": success, "elapsed_ms": elapsed_ms}),
                                    vec![],
                                    Some(format!("Tool result: {} ({}ms)", if success { "success" } else { "FAILED" }, elapsed_ms)),
                                );
                                chronos.record(&entry);
                            }
                            // Fire PostToolUse hook.
                            let mut post_ctx = HookContext {
                                tool_name: Some(tool_call.name.clone()),
                                tool_result: Some(tool_result.clone()),
                                ..Default::default()
                            };
                            self.hook_manager
                                .fire(HookPoint::PostToolUse, &mut post_ctx);
                            messages.push(ChatMessage::tool_result(tool_call.id, tool_result));
                        }
                        _ => {
                            let call_id = Uuid::new_v4().to_string();
                            if let Some(chronos) = &self.chronos {
                                let args_summary: String =
                                    serde_json::to_string(&tool_call.arguments)
                                        .unwrap_or_default()
                                        .chars()
                                        .take(200)
                                        .collect();
                                let entry = chronos.build_entry(
                                    &format!("tool:call:{}", call_id),
                                    "agent",
                                    ChronosAction::Create,
                                    &serde_json::json!({"tool": &tool_call.name, "args": args_summary}),
                                    vec![],
                                    Some(format!("Tool call: {} with args: {}", tool_call.name, args_summary)),
                                );
                                chronos.record(&entry);
                            }
                            let tool_start = Instant::now();
                            let tool_result = tool_dispatcher
                                .call_tool(&tool_call.name, tool_call.arguments)
                                .await;
                            let elapsed_ms = tool_start.elapsed().as_millis();
                            if let Some(chronos) = &self.chronos {
                                let success = !tool_result.starts_with("Error")
                                    && !tool_result.starts_with("⚠");
                                let entry = chronos.build_entry(
                                    &format!("tool:result:{}", call_id),
                                    "agent",
                                    ChronosAction::Update,
                                    &serde_json::json!({"success": success, "elapsed_ms": elapsed_ms}),
                                    vec![],
                                    Some(format!("Tool result: {} ({}ms)", if success { "success" } else { "FAILED" }, elapsed_ms)),
                                );
                                chronos.record(&entry);
                            }
                            // Fire PostToolUse hook.
                            let mut post_ctx = HookContext {
                                tool_name: Some(tool_call.name.clone()),
                                tool_result: Some(tool_result.clone()),
                                ..Default::default()
                            };
                            self.hook_manager
                                .fire(HookPoint::PostToolUse, &mut post_ctx);
                            messages.push(ChatMessage::tool_result(tool_call.id, tool_result));
                        }
                    }
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
            let guard = self
                .conversation_history
                .lock()
                .unwrap_or_else(|e| e.into_inner());
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
                    let mut guard = self
                        .conversation_history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
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
            let mut guard = self
                .conversation_history
                .lock()
                .unwrap_or_else(|e| e.into_inner());
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
            // Fits - return all.
            return messages.to_vec();
        }

        // Exceeds budget - keep most recent messages that fit
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
                    format!("{truncated}...")
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

    /// Detect commitment language in agent responses and store as promises.
    ///
    /// Scans for patterns like "I'll", "I will", "Let me", "Going to",
    /// "I'm going to" and stores them as pending tasks that the heartbeat
    /// checks every 30 seconds.
    async fn detect_and_store_promises(&self, _user_msg: &str, agent_reply: &str) {
        // Decision logic lives in commitment-detection.px (via PxBridge).
        // This Rust function is the IO boundary: it stores results to state + TaskManager + Chronos.
        //
        // TODO: Route through PxBridge.call("detect_commitments", {response: agent_reply})
        // and PxBridge.call("create_tasks_from_commitments", {commitments: ...})
        //
        // Until PxBridge is wired here, use a minimal Rust fallback
        // that mirrors the .px logic (commitment-detection.px).

        // Minimal fallback: detect numbered action items + "I will" statements
        let commitment_patterns = [
            "i'll ",
            "i will ",
            "let me ",
            "going to ",
        ];

        let action_verbs = [
            "diagnose", "fix", "implement", "write", "create", "update",
            "check", "verify", "build", "deploy", "configure", "refactor",
            "optimize", "debug", "test", "add", "remove", "migrate",
            "install", "resolve", "investigate", "wire", "connect",
            "integrate", "port", "rewrite",
        ];

        let mut promises: Vec<String> = Vec::new();

        for line in agent_reply.lines() {
            let trimmed = line.trim();
            if trimmed.len() < 15 || trimmed.len() > 200 {
                continue;
            }

            // Numbered list items with action verbs
            if trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                if let Some(text) = trimmed.split_once('.').map(|(_, t)| t.trim()) {
                    let lower = text.to_lowercase();
                    if action_verbs.iter().any(|v| lower.starts_with(v)) {
                        promises.push(text.to_string());
                    }
                }
            }

            // "I will..." / "I'll..." with action verbs
            let lower = trimmed.to_lowercase();
            for pattern in &commitment_patterns {
                if lower.contains(pattern) {
                    if let Some(after) = lower.split_once(pattern).map(|(_, a)| a) {
                        if action_verbs.iter().any(|v| after.starts_with(v)) && after.len() >= 15 {
                            let dedup = !promises.iter().any(|p| {
                                p.to_lowercase().contains(&after[..after.len().min(25)])
                            });
                            if dedup {
                                promises.push(trimmed.to_string());
                            }
                        }
                    }
                    break;
                }
            }
        }

        if promises.is_empty() {
            return;
        }

        // IO: Store promises in state (for heartbeat + steering lookup)
        if let Some(state) = &self.state_store {
            let existing = state.get("agent_promises").await
                .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v).ok())
                .unwrap_or_default();

            let mut updated = existing;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            for p in &promises {
                updated.push(serde_json::json!({
                    "what": p,
                    "completed": false,
                    "created_at": now,
                }));
            }

            if updated.len() > 20 {
                updated = updated.into_iter().rev().take(20).collect();
                updated.reverse();
            }

            state.set("agent_promises", serde_json::json!(updated)).await;
            state.set("most_recent_task_id", serde_json::json!(null)).await;
        }

        // IO: Create tasks in TaskManager
        if let Some(task_mgr) = &self.task_manager {
            use pares_radix_core::task::{CompletionCondition, ConditionType};
            let mut last_id = String::new();
            for p in &promises {
                let task = task_mgr.create_task(
                    p,
                    "agent_self",
                    vec![CompletionCondition {
                        description: format!("Complete: {}", &p[..p.len().min(80)]),
                        condition_type: ConditionType::ModelEvaluation(
                            format!("Verify completion: {}", &p[..p.len().min(120)])
                        ),
                        satisfied: false,
                    }],
                );
                last_id = task.id;
            }
            // Track most recent for steering
            if let Some(state) = &self.state_store {
                state.set("most_recent_task_id", serde_json::json!(last_id)).await;
            }
            tracing::info!(
                tasks_created = promises.len(),
                "agent commitments stored as tasks in TaskManager"
            );
        }

        // IO: Log to Chronos for audit trail
        if let Some(ref chronos) = self.chronos {
            for p in &promises {
                let entry = chronos.build_entry(
                    "agent:promise",
                    "agent",
                    pares_radix_core::chronos::ChronosAction::Create,
                    &serde_json::json!({
                        "what": p,
                        "completed": false,
                    }),
                    vec![],
                    Some(format!("promise: {}", &p[..p.len().min(80)])),
                );
                chronos.record(&entry);
            }

            tracing::info!(
                promises = promises.len(),
                "agent promises detected and logged to Chronos"
            );
        }
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
        let guard = self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                            let mut guard =
                                self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                            let mut history = self
                                .conversation_history
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
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
                                "{} session '{}'. Previous session was archived.",
                                action, new_branch
                            ),
                        })
                    }
                    "list" => {
                        let (active, branches) = {
                            let guard = self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                            let mut guard =
                                self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let mut guard = self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let mut history = self
                        .conversation_history
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
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
                    content: format!(
                        "{} branch '{}' from '{}'.",
                        action, new_branch, current_branch
                    ),
                })
            }
            "branches" => {
                let (active, branches) = {
                    let guard = self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
                    let mut guard = self.branch_state.lock().unwrap_or_else(|e| e.into_inner());
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
            "resume" => {
                let subcommand = parts.next().unwrap_or("").to_ascii_lowercase();
                match subcommand.as_str() {
                    "list" | "" if subcommand == "list" => {
                        // /resume list - show recent sessions
                        if let Some(session_mgr) = &self.session_manager {
                            let sessions = session_mgr.list_sessions(channel, 10).await;
                            if sessions.is_empty() {
                                return Some(Event::ModelResponse {
                                    request_id: id.to_string(),
                                    model: "command".into(),
                                    content: "No saved sessions found.".into(),
                                });
                            }
                            let mut lines = vec!["Recent sessions:".to_string()];
                            for s in &sessions {
                                let ts = chrono::DateTime::from_timestamp(s.started_at as i64, 0)
                                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                    .unwrap_or_else(|| s.started_at.to_string());
                                let topic = s.topic_summary.as_deref().unwrap_or("(no topic)");
                                lines.push(format!(
                                    "• {} - {} msgs, {} - {}",
                                    s.key, s.message_count, ts, topic
                                ));
                            }
                            Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: lines.join("\n"),
                            })
                        } else {
                            Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: "Session persistence not configured.".into(),
                            })
                        }
                    }
                    _ => {
                        // /resume (no args) - restore most recent session
                        if let Some(session_mgr) = &self.session_manager {
                            if let Some(saved) = session_mgr.load_active_session(channel).await {
                                let count = saved.messages.len();
                                // Restore into conversation history.
                                {
                                    let mut guard = self
                                        .conversation_history
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    guard.insert(channel.to_string(), saved.messages);
                                }
                                Some(Event::ModelResponse {
                                    request_id: id.to_string(),
                                    model: "command".into(),
                                    content: format!("Resumed session with {count} messages."),
                                })
                            } else {
                                Some(Event::ModelResponse {
                                    request_id: id.to_string(),
                                    model: "command".into(),
                                    content: "No session to resume.".into(),
                                })
                            }
                        } else {
                            Some(Event::ModelResponse {
                                request_id: id.to_string(),
                                model: "command".into(),
                                content: "Session persistence not configured.".into(),
                            })
                        }
                    }
                }
            }
            "sessions" => {
                // Alias for /resume list
                if let Some(session_mgr) = &self.session_manager {
                    let sessions = session_mgr.list_sessions(channel, 10).await;
                    if sessions.is_empty() {
                        return Some(Event::ModelResponse {
                            request_id: id.to_string(),
                            model: "command".into(),
                            content: "No saved sessions found.".into(),
                        });
                    }
                    let mut lines = vec!["Recent sessions:".to_string()];
                    for s in &sessions {
                        let ts = chrono::DateTime::from_timestamp(s.started_at as i64, 0)
                            .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                            .unwrap_or_else(|| s.started_at.to_string());
                        let topic = s.topic_summary.as_deref().unwrap_or("(no topic)");
                        lines.push(format!(
                            "• {} - {} msgs, {} - {}",
                            s.key, s.message_count, ts, topic
                        ));
                    }
                    Some(Event::ModelResponse {
                        request_id: id.to_string(),
                        model: "command".into(),
                        content: lines.join("\n"),
                    })
                } else {
                    Some(Event::ModelResponse {
                        request_id: id.to_string(),
                        model: "command".into(),
                        content: "Session persistence not configured.".into(),
                    })
                }
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

                // Archive the session for /resume support.
                if let Some(session_mgr) = &self.session_manager {
                    session_mgr.archive_session(channel).await;
                }

                info!(
                    channel,
                    from_session = %previous_session,
                    to_session = %new_session,
                    trigger = "/clear",
                    "conversation session transitioned"
                );

                Some(Event::ModelResponse {
                    request_id: id.to_string(),
                    model: "command".into(),
                    content: format!(
                        "Cleared conversation context. Started new session '{}'.",
                        new_session
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
    use pares_radix_core::model::{ChatOptions, ModelCompletion, ToolDefinition};
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
                model: None,
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
    async fn agent_streaming_emits_content_and_done() {
        use tokio::sync::mpsc;

        let agent = Agent::new(Arc::new(InMemory::new())).with_model(
            Arc::new(MockModel),
            Arc::new(MockTools),
            "You are a test agent.".into(),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();
        let response = agent.handle_event_streaming(msg("hello"), tx).await;

        // The response should still be returned.
        assert!(
            matches!(response, Some(Event::ModelResponse { ref content, .. }) if content == "Echo: hello")
        );

        // The stream should have received Content + Done (default complete_stream
        // impl sends the full content as one chunk then Done).
        let mut got_content = false;
        let mut got_done = false;
        while let Ok(delta) = rx.try_recv() {
            match delta {
                StreamDelta::Content(ref s) if s == "Echo: hello" => got_content = true,
                StreamDelta::Done => got_done = true,
                _ => {}
            }
        }
        assert!(got_content, "expected StreamDelta::Content");
        assert!(got_done, "expected StreamDelta::Done");
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

#[cfg(test)]
mod history_persistence_tests {
    use super::*;
    use pares_radix_core::model::ChatMessage;

    struct NullMem;
    #[async_trait::async_trait]
    impl Memory for NullMem {
        async fn capture(&self, _: &str) -> Result<(), String> {
            Ok(())
        }
        async fn recall(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn arc_agent_shares_history() {
        let agent = Arc::new(Agent::new(Arc::new(NullMem)));

        // Turn 1
        agent
            .persist_turn(
                "telegram",
                "s1",
                &[
                    ChatMessage::user("Remember: the codename is FALCON.".to_string()),
                    ChatMessage::assistant("Got it, codename FALCON.".to_string()),
                ],
            )
            .await;

        // Turn 2 via Arc clone
        let a2 = Arc::clone(&agent);
        a2.persist_turn(
            "telegram",
            "s1",
            &[
                ChatMessage::user("What's 2+2?".to_string()),
                ChatMessage::assistant("4".to_string()),
            ],
        )
        .await;

        // Turn 3 via another Arc clone - must see all history
        let a3 = Arc::clone(&agent);
        let history = a3.load_history("telegram").await;

        assert_eq!(
            history.len(),
            4,
            "expected 4 messages, got {}",
            history.len()
        );
        assert!(history[0].content.contains("FALCON"), "turn 1 missing");
        assert!(history[2].content.contains("2+2"), "turn 2 missing");
    }

    #[tokio::test]
    async fn history_survives_unrelated_turn() {
        let agent = Arc::new(Agent::new(Arc::new(NullMem)));

        // Give a task
        agent
            .persist_turn(
                "telegram",
                "s1",
                &[
                    ChatMessage::user("Deploy config to praxisbot".to_string()),
                    ChatMessage::assistant("Done, deployed.".to_string()),
                ],
            )
            .await;

        // Unrelated question
        let a2 = Arc::clone(&agent);
        a2.persist_turn(
            "telegram",
            "s1",
            &[
                ChatMessage::user("What's the weather?".to_string()),
                ChatMessage::assistant("I don't have weather data.".to_string()),
            ],
        )
        .await;

        // Ask about the task - history must include it
        let a3 = Arc::clone(&agent);
        let history = a3.load_history("telegram").await;

        assert_eq!(history.len(), 4);
        assert!(
            history
                .iter()
                .any(|m| m.content.contains("Deploy") || m.content.contains("praxisbot")),
            "task must be in history"
        );
    }
}
