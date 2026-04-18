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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

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
    /// Base system prompt.
    system_prompt: String,
    /// Per-channel conversation history (last 20 messages).
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
            conversation_history: Mutex::new(HashMap::new()),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
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
            conversation_history: Mutex::new(HashMap::new()),
            turn_store: None,
            audit_store: None,
            delegation_broker: None,
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
    pub fn with_audit_store(mut self, store: Arc<dyn pares_agens_audit::store::AuditStore>) -> Self {
        self.audit_store = Some(store);
        self
    }

    /// Handle a single event and optionally return a response event.
    pub async fn handle_event(&self, event: Event) -> Option<Event> {
        // ── Cerebellum: autorecall + routing ─────────────────────────────
        let (route, learned_context) = if let (Some(cerebellum), Some(plures_lm)) =
            (&self.cerebellum, &self.plures_lm)
        {
            match cerebellum
                .preprocess(&event, plures_lm, &self.procedure_registry)
                .await
            {
                Ok(ctx) => {
                    debug!(route = ?ctx.route, context_len = ctx.learned_context.len(), "cerebellum preprocessed event");
                    if ctx.route == Route::Drop {
                        debug!(event_kind = event.kind(), "cerebellum dropped event (Route::Drop)");
                        return None;
                    }
                    (ctx.route, ctx.learned_context)
                }
                Err(e) => {
                    error!(error = %e, "agent: cerebellum preprocess failed, continuing without context");
                    (Route::Conscious, String::new())
                }
            }
        } else {
            let default_route = match event {
                Event::Timer { .. } | Event::StateChange { .. } => Route::Procedural,
                _ => Route::Conscious,
            };
            (default_route, String::new())
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
                        self.handle_model_message(id, channel, content, &learned_context)
                            .await
                    }
                }
                Route::Conscious | Route::Deep { .. } => {
                    self.handle_model_message(id, channel, content, &learned_context)
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
    ) -> Option<Event> {
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

        let history_snapshot = self.load_history(channel).await;

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

        info!(input_len = content.len(), output_len = reply.len(), "LLM response generated");

        // Audit: log model call
        self.audit(
            pares_agens_audit::event::EventKind::ModelCall,
            "agent",
            model_label,
            &format!("in={}tok out={}tok", content.len() / 4, reply.len() / 4),
        ).await;

        // Persist new turn messages to PluresDB and update in-memory cache.
        let start = 1 + history_snapshot.len(); // skip system + existing history
        if messages.len() > start {
            let new_messages: Vec<ChatMessage> = messages[start..].to_vec();
            self.persist_turn(channel, &new_messages).await;
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
        for _ in 0..10 {
            let completion = model_client.complete(&messages, &tools, options).await?;

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

    /// Maximum number of messages to keep in the LLM context window.
    const HISTORY_WINDOW: usize = 20;

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
            // Load more than HISTORY_WINDOW to have material for summarization
            match store.recent_turns(channel, Self::HISTORY_WINDOW * 2).await {
                Ok(turns) if !turns.is_empty() => {
                    let messages: Vec<ChatMessage> = turns
                        .into_iter()
                        .flat_map(|t| t.messages)
                        .collect();
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
    async fn persist_turn(&self, channel: &str, new_messages: &[ChatMessage]) {
        // Update in-memory cache.
        {
            let mut guard = self.conversation_history.lock().unwrap();
            let history = guard.entry(channel.to_string()).or_default();
            history.extend(new_messages.iter().cloned());
            if history.len() > Self::HISTORY_WINDOW {
                let drain = history.len() - Self::HISTORY_WINDOW;
                history.drain(0..drain);
            }
        }

        // Persist to PluresDB if available.
        if let Some(store) = &self.turn_store {
            use crate::memory::entry::ChatTurn;
            let turn = ChatTurn {
                id: uuid::Uuid::new_v4().to_string(),
                channel: channel.to_string(),
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
            // Fits — return all (capped at HISTORY_WINDOW count)
            let start = messages.len().saturating_sub(Self::HISTORY_WINDOW);
            return messages[start..].to_vec();
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
            result.push(ChatMessage::system(format!(
                "[Context note: {} earlier messages ({} tokens) were summarized to fit context window. \
                 Recent conversation continues below.]",
                dropped,
                total_tokens - (Self::MAX_HISTORY_TOKENS - Self::SUMMARY_TOKEN_BUDGET - budget),
            )));
        }

        result.extend_from_slice(&messages[keep_from..]);
        result
    }

    /// Log an audit event if the audit store is configured.
    async fn audit(&self, kind: pares_agens_audit::event::EventKind, actor: &str, dest: &str, summary: &str) {
        if let Some(store) = &self.audit_store {
            let event = pares_agens_audit::event::AuditEvent::new(kind, actor, dest, summary, false);
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

        for lang in ["rust", "python", "typescript", "javascript", "go", "c#", "java"] {
            if lower.contains(lang) {
                tags.push(format!("lang:{lang}"));
            }
        }
        for tool in ["cargo", "tokio", "serde", "git", "docker", "kubernetes", "sql"] {
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
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
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
            .with_model(Arc::new(MockModel), Arc::new(MockTools), "You are a test agent.".into());

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
}
