//! Orchestrator — the orchestrator layer of the Three-Agent Cognitive Architecture.
//!
//! The orchestrator receives every inbound event **first**, before the standard or
//! deep_reasoner agents. It:
//!
//! 1. Runs **autorecall** — retrieves and compresses relevant memories into
//!    learned context.
//! 2. **Routes** the event — decides whether the standard agent can handle it
//!    alone, or whether the deep_reasoner should also be spawned for deep
//!    analysis.
//! 3. **Assembles** the final response from one or more agent outputs.
//!
//! The orchestrator itself uses a cheap/fast model (or no model at all for
//! pure-procedure routing). Expensive reasoning is delegated to the
//! deep_reasoner.
//!
//! # Design
//!
//! ```text
//! User ──► Orchestrator ──┬──► Standard  (directed executor)
//!                       └──► DeepReasoner (deep reasoner, optional)
//!                ▲                │
//!                └────────────────┘  (results flow back)
//! ```

pub mod actions;
pub mod bridge;
pub mod classifier;
pub mod dataflow_bridge;

pub mod invoke;
pub mod pipeline;
pub mod px_bridge;
pub mod router;

use pares_radix_core::pluresdb_bridge::PluresDbBridge;
use crate::orchestrator::px_bridge::PxBridge;
use crate::delegation::broker::SubTask;
use pares_radix_core::event::Event;
use crate::memory::entry::MemoryCategory;
use crate::memory::PluresLm;
use pares_radix_core::praxis::constraints::AuthorizationGate;
use pares_radix_core::procedure::{Procedure, ProcedureRegistry};

use async_trait::async_trait;
use pares_radix_praxis::rule::{Rule, RuleContext, RuleResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, instrument, warn};

// ── routing decision ─────────────────────────────────────────────────────────

/// Where the orchestrator decides to send an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Fast-tier model for simple/short responses (haiku, mini, flash).
    Fast,
    /// Standard agent only — standard-tier model (sonnet, gpt-4o).
    Standard,
    /// Both standard and deep_reasoner in parallel.
    /// The `reason` field is injected into the deep_reasoner prompt.
    Deep {
        /// Human-readable explanation of why the deep_reasoner is being invoked.
        reason: String,
    },
    /// Delegate to specialist sub-agents via the delegation broker.
    Delegate {
        /// Human-readable explanation of why the task is decomposed.
        reason: String,
        /// Sub-tasks created by the orchestrator for specialist agents.
        tasks: Vec<SubTask>,
    },
    /// Pure procedure — no LLM needed, orchestrator handles it directly.
    Procedural,
    /// Drop the event (e.g. noise, heartbeat-ok).
    Drop,
}

// ── orchestrator config ────────────────────────────────────────────────────────

/// Tuning knobs for the orchestrator.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CerebellumConfig {
    /// Maximum memories to recall per event.
    pub recall_limit: usize,
    /// Memory categories to exclude from autorecall.
    pub exclude_categories: Vec<String>,
    /// Whether to run the deep_reasoner at all. If false, all events go to
    /// standard only.
    pub enable_subconscious: bool,
    /// Complexity threshold (0.0–1.0). Events scoring above this trigger
    /// the deep_reasoner.
    pub complexity_threshold: f32,
    /// Token budget for autorecall context injection (number of tokens).
    pub context_token_budget: usize,
    /// Number of days after which a memory entry is considered stale.
    pub staleness_days: u32,
    /// Cosine similarity threshold above which two entries are considered
    /// duplicates during a orchestrator sweep.
    pub similarity_threshold: f32,
    /// Cosine similarity threshold below which a new message is treated as a
    /// topic shift for selective history management.
    pub topic_similarity_threshold: f32,
}

impl Default for CerebellumConfig {
    fn default() -> Self {
        Self {
            recall_limit: 10,
            exclude_categories: vec![],
            enable_subconscious: true,
            complexity_threshold: 0.7,
            context_token_budget: 4096,
            staleness_days: 30,
            similarity_threshold: 0.85,
            // Calibrated for mock + production embeddings so minor phrasing
            // changes stay in-topic while semantic shifts clear short-term history.
            topic_similarity_threshold: 0.72,
        }
    }
}

// ── orchestrator context ───────────────────────────────────────────────────────

/// Approval required for a destructive or external action (ADR-0012 level 4).
///
/// When this is `Some` in [`CerebellumContext`], the caller **must** obtain
/// explicit human approval before dispatching the action to a subagent.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    /// The action requiring approval (maps to the event kind).
    pub action: String,
    /// Human-readable rationale presented to the approver.
    pub rationale: String,
}

/// The enriched context the orchestrator produces for downstream agents.
#[derive(Debug, Clone)]
pub struct CerebellumContext {
    /// The original event.
    pub event: Event,
    /// Learned context (compressed memories) ready for prompt injection.
    pub learned_context: String,
    /// Routing decision.
    pub route: Route,
    /// Praxis ledger guidance entries, if any.
    pub guidance: Vec<String>,
    /// Whether short-term conversation history should be cleared for this turn
    /// because a topic shift was detected.
    pub clear_history: bool,
    /// When `Some`, the authorization gate (ADR-0012 level 4) requires explicit
    /// human approval before this action may be dispatched.
    pub approval_required: Option<ApprovalRequest>,
}

// ── orchestrator ───────────────────────────────────────────────────────────────

/// The Orchestrator orchestrator.
///
/// Stateless — all persistent state lives in PluresDB via the `PluresLm`
/// memory client. The orchestrator reads from memory and procedures, makes
/// routing decisions, and produces enriched contexts for downstream agents.
///
/// When `pluresdb` is `Some`, the orchestrator can delegate procedure pipelines
/// (VectorSearch, Transform, etc.) to the native PluresDB engine for
/// autorecall and compression.  When `None`, the pure-Rust implementations
/// are used as fallback.
pub struct Orchestrator {
    /// Tuning configuration for this orchestrator instance.
    pub config: CerebellumConfig,
    /// Optional PluresDB bridge for native procedure execution.
    pub pluresdb: Option<PluresDbBridge>,
    /// Last topic embedding seen per channel.
    topic_embeddings: Mutex<HashMap<String, Vec<f32>>>,
    /// Optional message classifier for intent/complexity routing.
    pub classifier: Option<classifier::CerebellumClassifier>,
    /// Persistent managed context window.
    context_items: Mutex<Vec<context_manager::ContextItem>>,
    /// Relevance scorer with learned weights.
    relevance_scorer: Mutex<context_manager::RelevanceScorer>,
    /// Optional conversation store for fallback context when autorecall has no hits.
    conversation_store: Option<Arc<dyn pares_radix_core::spine::conversation::ConversationStore>>,
    /// Optional .px bridge for calling .px procedures instead of hardcoded Rust logic.
    /// When loaded, classification and routing go through .px first, falling back to Rust.
    px_bridge: Option<Arc<PxBridge>>,
    /// Optional dataflow bridge for queue-driven procedures.
    /// When loaded, takes precedence over px_bridge (trigger-based).
    dataflow_bridge: Option<Arc<dataflow_bridge::DataflowBridge>>,
}

impl Orchestrator {
    /// Create a orchestrator without a PluresDB bridge (pure-Rust fallback).
    pub fn new(config: CerebellumConfig) -> Self {
        Self {
            config,
            pluresdb: None,
            topic_embeddings: Mutex::new(HashMap::new()),
            classifier: None,
            context_items: Mutex::new(Vec::new()),
            relevance_scorer: Mutex::new(context_manager::RelevanceScorer::default()),
            conversation_store: None,
            px_bridge: None,
            dataflow_bridge: None,
        }
    }

    /// Create a orchestrator with an attached [`PluresDbBridge`].
    pub fn with_bridge(config: CerebellumConfig, bridge: PluresDbBridge) -> Self {
        Self {
            config,
            pluresdb: Some(bridge),
            topic_embeddings: Mutex::new(HashMap::new()),
            classifier: None,
            context_items: Mutex::new(Vec::new()),
            relevance_scorer: Mutex::new(context_manager::RelevanceScorer::default()),
            conversation_store: None,
            px_bridge: None,
            dataflow_bridge: None,
        }
    }

    /// Attach a conversation store for fallback context when autorecall returns no hits.
    pub fn with_conversation_store(
        mut self,
        store: Arc<dyn pares_radix_core::spine::conversation::ConversationStore>,
    ) -> Self {
        self.conversation_store = Some(store);
        self
    }

    /// Attach a .px bridge for calling .px procedures instead of hardcoded Rust logic.
    ///
    /// When set, the orchestrator will try .px procedures for classification and routing
    /// FIRST, falling back to Rust implementations only when .px returns None or errors.
    pub fn with_px_bridge(mut self, bridge: Arc<PxBridge>) -> Self {
        self.px_bridge = Some(bridge);
        self
    }

    /// Attach a dataflow bridge for queue-driven procedure execution.
    /// When set, takes precedence over px_bridge (trigger-based).
    pub fn with_dataflow_bridge(mut self, bridge: Arc<dataflow_bridge::DataflowBridge>) -> Self {
        self.dataflow_bridge = Some(bridge);
        self
    }

    /// Attach a message classifier to this orchestrator.
    pub fn with_classifier(mut self, classifier: classifier::CerebellumClassifier) -> Self {
        self.classifier = Some(classifier);
        self
    }

    /// Record the outcome of a model interaction.
    ///
    /// Call this after the standard model responds. The orchestrator uses
    /// this to adjust relevance weights — context items that were present
    /// during successful interactions get boosted; failed ones get decayed.
    pub fn record_outcome(&self, success: bool) {
        let items = self.context_items.lock().unwrap();
        let mut scorer = self.relevance_scorer.lock().unwrap();
        scorer.record_outcome(&items, success);
        tracing::debug!(
            success,
            context_items = items.len(),
            "orchestrator outcome recorded"
        );
    }

    /// Main entry point: preprocess an event into an enriched context.
    ///
    /// 1. Autorecall — retrieve + compress memories
    /// 2. Authorization gate (ADR-0012) — evaluate 5-level gate
    /// 3. Route — decide standard / deep / procedural / drop
    /// 4. Package context for downstream agents
    #[instrument(skip(self, memory, _registry))]
    pub async fn preprocess(
        &self,
        event: &Event,
        memory: &PluresLm,
        _registry: &ProcedureRegistry,
    ) -> Result<CerebellumContext, CerebellumError> {
        let preprocess_start = std::time::Instant::now();

        // 0. Extract entities from the message (fast, no model)
        let query = extract_query(event);
        let entities = query
            .as_deref()
            .map(context_manager::EntityExtractor::extract)
            .unwrap_or_default();

        // 1. Recall relevant memories and convert to ContextItems
        let mut topic_shifted = false;
        let mut topic_shift_outcome = TopicShiftOutcome::SkippedShortMessage;
        let mut query_similarities = std::collections::HashMap::new();

        // Fast path: skip expensive embedding + recall for very short
        // contextual messages (≤3 words). These are almost always follow-ups
        // that rely on conversation history, not semantic memory.
        let skip_recall = query.as_deref().is_none_or(|q| {
            let word_count = q.split_whitespace().count();
            word_count <= 3 && word_count > 0
        });

        let recalled_items = if !skip_recall {
            if let Some(q) = &query {
            let embed_start = std::time::Instant::now();
            let query_embedding = memory
                .embed_text(q)
                .await
                .map_err(|e| CerebellumError::Memory(e.to_string()))?;
            let embed_elapsed = embed_start.elapsed();
            tracing::info!(embed_ms = embed_elapsed.as_millis(), "embedding computed");

            topic_shift_outcome = self.detect_topic_shift(event, &query_embedding);
            topic_shifted = topic_shift_outcome.is_shift();

            let recall_start = std::time::Instant::now();
            let exclude_categories = parse_excluded_categories(&self.config.exclude_categories);
            let memories = memory
                .recall(q, self.config.recall_limit, &exclude_categories)
                .await
                .map_err(|e| CerebellumError::Memory(e.to_string()))?;
            let recall_elapsed = recall_start.elapsed();
            tracing::info!(recall_ms = recall_elapsed.as_millis(), memories_found = memories.len(), "memory recall complete");

            // Convert recalled memories to ContextItems
            memories
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let id = format!("mem:{i}");
                    // Score decays with rank (top result = 1.0, #10 = 0.5)
                    let sim = 1.0 - (i as f32 * 0.05).min(0.5);
                    query_similarities.insert(id.clone(), sim);
                    context_manager::ContextItem {
                        id,
                        content: m.content.clone(),
                        tokens: m.content.len() / 4, // rough estimate
                        relevance: 0.0,              // scored by manager
                        source: context_manager::ContextSource::Memory,
                        age_turns: 0,
                        success_count: 0,
                        failure_count: 0,
                    }
                })
                .collect::<Vec<_>>()
            } else {
                vec![]
            }
        } else {
            // Fast path: skip embedding/recall for short contextual messages
            if let Some(q) = &query {
                tracing::info!(query = %q, "skipping recall for short message (fast path)");
            }
            vec![]
        };

        // 2. Manage context window — add new, score all, drop lowest
        let managed = {
            let mut items = self.context_items.lock().unwrap();
            let scorer = self.relevance_scorer.lock().unwrap();

            // A semantic topic shift invalidates recalled/context-manager items,
            // but must not erase the channel's explicit conversation history.
            // Users commonly switch from discussing a task to asking for its
            // status; that is semantically different while still referring to
            // the immediately preceding turn.
            if topic_shifted {
                items.clear();
            }

            // Add entity-derived context items
            for entity in &entities {
                let id = entity.context_key.clone();
                if !items.iter().any(|i| i.id == id) {
                    items.push(context_manager::ContextItem {
                        id,
                        content: format!("{:?}: {}", entity.kind, entity.value),
                        tokens: 10,
                        relevance: 0.0,
                        source: context_manager::ContextSource::Entity,
                        age_turns: 0,
                        success_count: 0,
                        failure_count: 0,
                    });
                }
            }

            context_manager::manage_context(
                &mut items,
                recalled_items,
                &entities,
                &scorer,
                &query_similarities,
                self.config.context_token_budget,
            )
        };

        // Build the context string from managed items
        // Fallback: if autorecall returned nothing, pull recent conversation
        // exchanges so contextual follow-ups ("do that", "yes", "continue") work.
        let learned_context = if managed.items.is_empty() {
            // No semantic memory hits — inject recent conversation as fallback
            if let Some(store) = &self.conversation_store {
                let chat_id = event.chat_id().unwrap_or_default();
                if !chat_id.is_empty() {
                    let history = store.get_history(chat_id).await;
                    let recent: Vec<_> = history.iter().rev().take(4).collect();
                    if !recent.is_empty() {
                        let mut ctx = String::from("## Recent Conversation\n");
                        for msg in recent.iter().rev() {
                            let role = msg.role_str();
                            let content = msg.content_preview(200);
                            ctx.push_str(&format!("- {}: {}\n", role, content));
                        }
                        ctx
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            let mut ctx = String::from("## Recalled Context\n");
            for item in &managed.items {
                ctx.push_str(&format!("- [rel:{:.2}] {}\n", item.relevance, item.content));
            }
            ctx
        };

        info!(
            event_kind = event.kind(),
            context_len = learned_context.len(),
            context_items = managed.items.len(),
            tokens_used = managed.tokens_used,
            token_budget = managed.token_budget,
            topic_shifted,
            topic_shift_outcome = topic_shift_outcome.as_str(),
            entities = entities.len(),
            "context managed"
        );

        // 2. Authorization gate (ADR-0012)
        let gate_ctx = build_authorization_context(event);
        let gate_result = AuthorizationGate.evaluate(&gate_ctx);

        // Level 1: hard constraint → block immediately
        if let RuleResult::Fail { reason } = &gate_result {
            return Err(CerebellumError::AuthorizationBlocked {
                reason: reason.clone(),
            });
        }

        // 3. Route — try dataflow first, then .px trigger-based, fall back to Rust
        //    Precedence: dataflow_bridge → px_bridge → router::decide()
        let mut route = if let Some(ref df_bridge) = self.dataflow_bridge {
            if df_bridge.is_active() {
                let event_type = event.kind().to_string();
                let content = extract_query(event).unwrap_or_default();
                match df_bridge
                    .process_event(&event_type, &content, &learned_context)
                    .await
                {
                    Ok(Some(val)) => {
                        parse_px_route(&val).unwrap_or_else(|| {
                            debug!(raw = %val, "dataflow route returned unparseable result, trying px_bridge");
                            Route::Standard // signal to try next tier
                        })
                    }
                    Ok(None) => {
                        // Dataflow didn't produce a route — fall through to px_bridge
                        self.try_px_bridge_route(event, &learned_context).await
                    }
                    Err(e) => {
                        warn!(error = %e, "dataflow bridge failed, falling back to px_bridge");
                        self.try_px_bridge_route(event, &learned_context).await
                    }
                }
            } else {
                self.try_px_bridge_route(event, &learned_context).await
            }
        } else {
            self.try_px_bridge_route(event, &learned_context).await
        };
        let mut guidance: Vec<String> = vec![];
        let mut approval_required: Option<ApprovalRequest> = None;

        match gate_result {
            // Level 2: skip duplicate — only suppress tool/action events, never user messages
            RuleResult::Warning { ref message } if message.starts_with("skip:") => {
                if !matches!(event, Event::Message { .. }) {
                    debug!(message, "authorization gate: duplicate action suppressed");
                    route = Route::Drop;
                } else {
                    debug!(message, "authorization gate: skip suppressed for user message (never drop messages)");
                }
                guidance.push(message.clone());
            }
            // Level 3: known failure — warn, keep original route
            RuleResult::Warning { ref message } => {
                warn!(message, "authorization gate: known failure warning");
                guidance.push(message.clone());
            }
            // Level 4: destructive/external → require approval
            RuleResult::Gate {
                ref action,
                ref rationale,
            } => {
                debug!(action, rationale, "authorization gate: approval required");
                approval_required = Some(ApprovalRequest {
                    action: action.clone(),
                    rationale: rationale.clone(),
                });
                guidance.push(format!("approval_required: {rationale}"));
            }
            // Level 5: auto-approve (Pass) or already handled (Fail above)
            _ => {}
        }

        debug!(?route, "routing decision");

        let preprocess_elapsed = preprocess_start.elapsed();
        tracing::info!(
            preprocess_ms = preprocess_elapsed.as_millis(),
            route = ?route,
            "orchestrator preprocess complete"
        );

        // 4. Package
        Ok(CerebellumContext {
            event: event.clone(),
            learned_context,
            route,
            guidance,
            // Conversation history is user/session state. Semantic similarity
            // is too weak a signal to discard it; only explicit session commands
            // should do that.
            clear_history: false,
            approval_required,
        })
    }

    /// Full dataflow pipeline: write to inbound → graph runs → delivery returned.
    ///
    /// This is the target architecture (unified-router.px). When the dataflow bridge
    /// is active and has procedures loaded, this runs the COMPLETE pipeline without
    /// the imperative preprocess/route/invoke cycle.
    ///
    /// Returns `Some(DeliveryResult)` if the graph produced a response, `None` to
    /// fall through to the legacy orchestrator path.
    pub async fn try_full_dataflow(
        &self,
        chat_id: i64,
        sender: &str,
        content: &str,
        message_id: Option<&str>,
        history: Option<&[pares_radix_core::model::ChatMessage]>,
    ) -> Option<dataflow_bridge::DeliveryResult> {
        let df_bridge = self.dataflow_bridge.as_ref()?;
        if !df_bridge.is_active() {
            return None;
        }

        match df_bridge.process_message(chat_id, sender, content, message_id, history).await {
            Ok(Some(delivery)) => {
                info!(
                    chat_id,
                    content_len = delivery.content.len(),
                    "full dataflow pipeline produced delivery"
                );
                Some(delivery)
            }
            Ok(None) => {
                debug!(chat_id, "dataflow pipeline quiesced without delivery — falling through");
                None
            }
            Err(e) => {
                warn!(error = %e, chat_id, "dataflow pipeline error — falling through to legacy");
                None
            }
        }
    }

    /// Build the factual classification input consumed by `route_dispatch`.
    /// Classification is an input/model boundary; the destination decision is
    /// made by the .px procedure.
    fn route_dispatch_classification(&self, event: &Event) -> serde_json::Value {
        match event {
            Event::Message { content, .. } => {
                let classification = self
                    .classifier
                    .as_ref()
                    .map(|classifier| classifier.classify(content))
                    .unwrap_or_else(|| {
                        classifier::CerebellumClassifier::heuristic_only(Vec::new()).classify(content)
                    });
                serde_json::to_value(classification).unwrap_or_else(|error| {
                    warn!(%error, "could not serialize message classification for .px routing");
                    serde_json::json!({
                        "intent": "chat",
                        "complexity": 1,
                        "needs_tools": false,
                        "needs_deep_model": false,
                    })
                })
            }
            _ => serde_json::json!({
                "intent": "system",
                "complexity": 0,
                "needs_tools": false,
                "needs_deep_model": false,
            }),
        }
    }

    /// Try routing via the px_bridge (trigger-based .px procedures).
    ///
    /// route_dispatch is the primary decision source once the bridge is
    /// loaded and active. The legacy Rust `router::decide()` remains the
    /// fallback until route_dispatch is proven equivalent via tests in
    /// production (bridge missing/inactive, call errors, or an unparseable
    /// result all fall back to the Rust router rather than silently
    /// defaulting to `Route::Standard`).
    async fn try_px_bridge_route(&self, event: &Event, learned_context: &str) -> Route {
        let Some(bridge) = &self.px_bridge else {
            return router::decide(event, learned_context, &self.config);
        };
        if !bridge.is_active() {
            return router::decide(event, learned_context, &self.config);
        }

        let event_type = event.kind().to_string();
        let classification = self.route_dispatch_classification(event);
        match bridge
            .route_dispatch(classification, learned_context, &event_type)
            .await
        {
            Some(Ok(val)) => parse_px_route(&val).unwrap_or_else(|| {
                debug!(raw = %val, "px route returned unparseable result, falling back to Rust");
                router::decide(event, learned_context, &self.config)
            }),
            Some(Err(e)) => {
                warn!(error = %e, "px route_dispatch failed, falling back to Rust");
                router::decide(event, learned_context, &self.config)
            }
            None => router::decide(event, learned_context, &self.config),
        }
    }

    fn detect_topic_shift(&self, event: &Event, current_embedding: &[f32]) -> TopicShiftOutcome {
        let Some(channel_key) = event_channel_key(event) else {
            return TopicShiftOutcome::NotShifted;
        };

        // Short messages (< 20 chars) are almost always follow-ups ("do that",
        // "yes", "continue", "no"). Never treat them as topic shifts.
        if let Event::Message { content, .. } = event {
            if content.trim().len() < 20 {
                // Still update the embedding cache for future comparisons
                match self.topic_embeddings.lock() {
                    Ok(mut embeddings) => {
                        embeddings.insert(channel_key, current_embedding.to_vec());
                    }
                    Err(e) => {
                        warn!(error = %e, "topic embedding cache poisoned while caching short reply; recovering");
                        let mut embeddings = e.into_inner();
                        embeddings.insert(channel_key, current_embedding.to_vec());
                    }
                }
                return TopicShiftOutcome::SkippedShortReply;
            }
        }

        let mut embeddings = match self.topic_embeddings.lock() {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error = %e, "topic embedding cache poisoned; recovering guard and continuing detection");
                e.into_inner()
            }
        };
        let had_prior = embeddings.contains_key(&channel_key);
        let outcome = if !had_prior {
            TopicShiftOutcome::SkippedNoPriorTurn
        } else {
            let shifted = embeddings
                .get(&channel_key)
                .map(|previous| {
                    cosine_similarity(previous, current_embedding)
                        < self.config.topic_similarity_threshold
                })
                .unwrap_or(false);
            if shifted {
                TopicShiftOutcome::Shifted
            } else {
                TopicShiftOutcome::NotShifted
            }
        };
        embeddings.insert(channel_key, current_embedding.to_vec());
        outcome
    }
}

/// Outcome of a topic-shift check, distinguishing an actual determination
/// (Shifted / NotShifted) from the various reasons detection was skipped
/// (ADR-0015 S4). Observability field on `preprocess`'s tracing span lets
/// suppressed-vs-detected be told apart in production telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicShiftOutcome {
    /// Cosine similarity to the channel's previous topic embedding fell
    /// below the configured threshold: a genuine topic shift.
    Shifted,
    /// Cosine similarity stayed above threshold: same topic.
    NotShifted,
    /// Message was <=3 words; recall/embedding was skipped entirely
    /// (`preprocess`'s fast path), so no comparison was possible.
    SkippedShortMessage,
    /// Message was >=4 words but <20 chars: treated as a conversational
    /// follow-up ('do that', 'yes'), never a topic shift by design.
    SkippedShortReply,
    /// The topic-embedding cache mutex was poisoned by a prior panic; the
    /// guard was recovered via `into_inner()` and detection continued.
    #[allow(dead_code)]
    SkippedCachePoisoned,
    /// No prior embedding cached for this channel (first turn) -- correct
    /// behavior, not a bug, but distinguishable from suppression.
    SkippedNoPriorTurn,
}

impl TopicShiftOutcome {
    /// Collapse to the boolean `preprocess` needs to decide whether to
    /// clear stale context items. Only `Shifted` clears context.
    pub fn is_shift(self) -> bool {
        matches!(self, TopicShiftOutcome::Shifted)
    }

    /// Stable string for tracing/log fields.
    pub fn as_str(self) -> &'static str {
        match self {
            TopicShiftOutcome::Shifted => "shifted",
            TopicShiftOutcome::NotShifted => "not_shifted",
            TopicShiftOutcome::SkippedShortMessage => "skipped_short_message",
            TopicShiftOutcome::SkippedShortReply => "skipped_short_reply",
            TopicShiftOutcome::SkippedCachePoisoned => "skipped_cache_poisoned",
            TopicShiftOutcome::SkippedNoPriorTurn => "skipped_no_prior_turn",
        }
    }
}

/// Orchestrator-level errors.
#[derive(Debug, thiserror::Error)]
pub enum CerebellumError {
    /// A memory subsystem operation failed.
    #[error("memory error: {0}")]
    Memory(String),
    /// A procedure execution step failed.
    #[error("procedure error: {0}")]
    Procedure(String),
    /// The authorization gate (ADR-0012 level 1) blocked the action.
    #[error("authorization blocked: {reason}")]
    AuthorizationBlocked {
        /// Human-readable reason returned by the gate.
        reason: String,
    },
}

// ── orchestrator as a Procedure ────────────────────────────────────────────────

/// Adapter that lets the orchestrator participate in the procedure registry
/// as a first-class procedure handling `"message"` events.
///
/// When constructed with [`CerebellumProcedure::with_cerebellum`], the procedure
/// delegates to [`Orchestrator::preprocess`] for autorecall, topic detection, and
/// context management.  The [`CerebellumProcedure::stub`] variant is available
/// for registration and dispatch testing without a live memory system.
pub struct CerebellumProcedure {
    orchestrator: Option<Arc<Orchestrator>>,
    memory: Option<Arc<PluresLm>>,
    registry: Option<Arc<ProcedureRegistry>>,
}

impl CerebellumProcedure {
    /// Create a stub procedure for registration and dispatch testing.
    pub fn stub() -> Self {
        Self {
            orchestrator: None,
            memory: None,
            registry: None,
        }
    }

    /// Create a fully-wired procedure that delegates to a live [`Orchestrator`].
    pub fn with_cerebellum(
        orchestrator: Arc<Orchestrator>,
        memory: Arc<PluresLm>,
        registry: Arc<ProcedureRegistry>,
    ) -> Self {
        Self {
            orchestrator: Some(orchestrator),
            memory: Some(memory),
            registry: Some(registry),
        }
    }
}

#[async_trait]
impl Procedure for CerebellumProcedure {
    fn name(&self) -> &str {
        "orchestrator"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let (orchestrator, memory, registry) = match (&self.orchestrator, &self.memory, &self.registry)
        {
            (Some(c), Some(m), Some(r)) => (c, m, r),
            _ => {
                debug!(
                    event_kind = event.kind(),
                    "orchestrator procedure stub (no live system)"
                );
                return vec![];
            }
        };

        match orchestrator.preprocess(event, memory, registry).await {
            Ok(ctx) => {
                info!(
                    context_len = ctx.learned_context.len(),
                    clear_history = ctx.clear_history,
                    route = ?ctx.route,
                    "orchestrator preprocessed message"
                );
                // Emit a StateChange event with the orchestrator context so
                // downstream procedures (standard agent) can consume it.
                vec![Event::StateChange {
                    key: "orchestrator:context".to_string(),
                    old_value: None,
                    new_value: serde_json::json!({
                        "learned_context": ctx.learned_context,
                        "clear_history": ctx.clear_history,
                        "route": format!("{:?}", ctx.route),
                        "guidance": ctx.guidance,
                    }),
                }]
            }
            Err(e) => {
                warn!(error = %e, "orchestrator preprocess failed");
                vec![]
            }
        }
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Extract a search query from an event for autorecall.
fn extract_query(event: &Event) -> Option<String> {
    match event {
        Event::Message { content, .. } => {
            if content.trim().is_empty() {
                None
            } else {
                Some(content.clone())
            }
        }
        Event::StateChange { key, new_value, .. } => Some(format!("{}: {}", key, new_value)),
        // Timer and tool results don't trigger autorecall
        _ => None,
    }
}

fn event_channel_key(event: &Event) -> Option<String> {
    match event {
        Event::Message { channel, .. } => Some(channel.clone()),
        _ => None,
    }
}

fn parse_excluded_categories(raw_categories: &[String]) -> Vec<MemoryCategory> {
    raw_categories
        .iter()
        .filter_map(|category| parse_memory_category(category))
        .collect()
}

fn parse_memory_category(category: &str) -> Option<MemoryCategory> {
    let normalized = category.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "conversation" => Some(MemoryCategory::Conversation),
        "code-pattern" => Some(MemoryCategory::CodePattern),
        "error-fix" => Some(MemoryCategory::ErrorFix),
        "preference" => Some(MemoryCategory::Preference),
        "decision" => Some(MemoryCategory::Decision),
        "fact" => Some(MemoryCategory::Fact),
        "procedure" => Some(MemoryCategory::Procedure),
        "ui-interaction" => Some(MemoryCategory::UiInteraction),
        "app-state" => Some(MemoryCategory::AppState),
        "screen-capture" => Some(MemoryCategory::ScreenCapture),
        "automation-trace" => Some(MemoryCategory::AutomationTrace),
        "build-result" => Some(MemoryCategory::BuildResult),
        "demo-checkpoint" => Some(MemoryCategory::DemoCheckpoint),
        "correction" => Some(MemoryCategory::Correction),
        _ => None,
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (dot, norm_a_sq, norm_b_sq) = a.iter().zip(b.iter()).fold(
        (0.0f32, 0.0f32, 0.0f32),
        |(dot, norm_a_sq, norm_b_sq), (&x, &y)| (dot + x * y, norm_a_sq + x * x, norm_b_sq + y * y),
    );
    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Build an [`RuleContext`] for the authorization gate from an event.
///
/// The orchestrator derives gate payload flags from what it can observe in the
/// event.  Flags that would require external queries (e.g. `completed_recently`,
/// `known_failure`) default to `false` here; the orchestration layer may enrich
/// the context further before re-evaluating the gate directly.
///
/// | Payload field | Source |
/// |---------------|--------|
/// | `blocked_by_constraint` | always `false` (handled by executor's PraxisGate) |
/// | `completed_recently` | always `false` (no dedup log in orchestrator) |
/// | `known_failure` | always `false` (no failure log in orchestrator) |
/// | `is_destructive` | `true` for `ToolResult` with destructive tool prefixes |
/// | `is_external` | `true` for `ToolResult` whose name suggests an external call |
fn build_authorization_context(event: &Event) -> RuleContext {
    let (is_destructive, is_external) = match event {
        Event::ToolResult { tool_name, .. } => {
            let destructive = tool_name.starts_with("delete_")
                || tool_name.starts_with("write_")
                || tool_name.starts_with("update_")
                || tool_name.starts_with("create_")
                || tool_name.starts_with("publish_");
            let external = tool_name.starts_with("send_")
                || tool_name.starts_with("post_")
                || tool_name.starts_with("email_")
                || tool_name.starts_with("webhook_")
                || tool_name.starts_with("http_");
            (destructive, external)
        }
        _ => (false, false),
    };

    RuleContext::new(
        event.kind(),
        serde_json::json!({
            "blocked_by_constraint": false,
            "completed_recently":    false,
            "known_failure":         false,
            "is_destructive":        is_destructive,
            "is_external":           is_external,
        }),
    )
}

/// Parse a .px procedure result (JSON Value) into a [`Route`] enum.
///
/// Expected .px output format:
/// ```json
/// {"route": "fast"}
/// {"route": "standard"}
/// {"route": "deep", "reason": "..."}
/// {"route": "delegate", "reason": "...", "tasks": [...]}
/// {"route": "procedural"}
/// {"route": "drop"}
/// ```
fn parse_px_route(val: &serde_json::Value) -> Option<Route> {
    let route_str = val.get("route")?.as_str()?;
    match route_str {
        "standard" => Some(Route::Standard),
        "fast" => Some(Route::Fast),
        "procedural" => Some(Route::Procedural),
        "drop" => Some(Route::Drop),
        "deep" => {
            let reason = val
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("px routing decided deep reasoning needed")
                .to_string();
            Some(Route::Deep { reason })
        }
        "delegate" => {
            let reason = val
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("px routing decided delegation needed")
                .to_string();
            let tasks = val
                .get("tasks")
                .and_then(|t| t.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|task| {
                            Some(SubTask {
                                agent_name: task
                                    .get("agent_name")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("general")
                                    .to_string(),
                                input: task.get("input")?.as_str()?.to_string(),
                                parent_context: task
                                    .get("parent_context")
                                    .and_then(|s| s.as_str())
                                    .map(String::from),
                                steering_rx: None,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Route::Delegate { reason, tasks })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        embed::{EmbeddingProvider, MockEmbedder},
        entry::{MemoryCategory, MemoryEntry},
        store::{InMemoryStore, MemoryStore as _},
    };
    use pares_radix_praxis::rule::RuleResult;
    use std::sync::Arc;

    #[test]
    fn extract_query_from_message() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "How does CRDT merging work?".into(),
        };
        assert_eq!(
            extract_query(&event),
            Some("How does CRDT merging work?".into())
        );
    }

    #[test]
    fn extract_query_empty_message_returns_none() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "   ".into(),
        };
        assert_eq!(extract_query(&event), None);
    }

    #[test]
    fn extract_query_from_timer_returns_none() {
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        assert_eq!(extract_query(&event), None);
    }

    #[test]
    fn parse_px_route_accepts_all_destination_shapes() {
        assert_eq!(parse_px_route(&serde_json::json!({"route": "fast"})), Some(Route::Fast));
        assert_eq!(
            parse_px_route(&serde_json::json!({"route": "standard"})),
            Some(Route::Standard)
        );
        assert_eq!(
            parse_px_route(&serde_json::json!({"route": "procedural"})),
            Some(Route::Procedural)
        );
        assert_eq!(parse_px_route(&serde_json::json!({"route": "drop"})), Some(Route::Drop));
        assert!(matches!(
            parse_px_route(&serde_json::json!({"route": "deep", "reason": "complex"})),
            Some(Route::Deep { reason }) if reason == "complex"
        ));
        assert!(matches!(
            parse_px_route(&serde_json::json!({
                "route": "delegate",
                "reason": "parallel work",
                "tasks": [{"agent_name": "worker", "input": "do work"}]
            })),
            Some(Route::Delegate { reason, tasks })
                if reason == "parallel work" && tasks.len() == 1 && tasks[0].agent_name == "worker"
        ));
    }

    #[test]
    fn default_config() {
        let cfg = CerebellumConfig::default();
        assert_eq!(cfg.recall_limit, 10);
        assert!(cfg.enable_subconscious);
        assert!((cfg.complexity_threshold - 0.7).abs() < f32::EPSILON);
        assert!((cfg.topic_similarity_threshold - 0.72).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn preprocess_preserves_conversation_history_on_topic_shift() {
        let store = Arc::new(InMemoryStore::new());
        let rust_embedding = MockEmbedder
            .embed("Use tokio channels to coordinate async Rust tasks")
            .await
            .expect("embedding should succeed");
        store
            .insert(MemoryEntry {
                id: "rust-1".into(),
                content: "Use tokio channels to coordinate async Rust tasks".into(),
                category: MemoryCategory::CodePattern,
                tags: vec![],
                embedding: rust_embedding,
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .await
            .expect("memory insert should succeed");

        let memory = PluresLm::new(store, Box::new(MockEmbedder), 128_000);
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let registry = ProcedureRegistry::new();

        let rust_msg = Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "How does async Rust work with tokio?".into(),
        };
        let rust_ctx = orchestrator
            .preprocess(&rust_msg, &memory, &registry)
            .await
            .expect("first preprocess should succeed");
        assert!(
            !rust_ctx.clear_history,
            "first topic should not clear history"
        );

        let cooking_msg = Event::Message {
            id: "2".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "What is the best way to bake sourdough bread?".into(),
        };
        let cooking_ctx = orchestrator
            .preprocess(&cooking_msg, &memory, &registry)
            .await
            .expect("second preprocess should succeed");
        assert!(
            !cooking_ctx.clear_history,
            "semantic topic changes must not discard explicit conversation history"
        );

        let rust_return_msg = Event::Message {
            id: "3".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "Back to Rust: when should I use async channels?".into(),
        };
        let rust_return_ctx = orchestrator
            .preprocess(&rust_return_msg, &memory, &registry)
            .await
            .expect("topic return preprocess should succeed");
        assert!(
            rust_return_ctx
                .learned_context
                .contains("Use tokio channels to coordinate async Rust tasks"),
            "returned topic should restore relevant long-term context from memory"
        );
    }

    // ── detect_topic_shift outcome table (ADR-0015 S6) ─────────────────────

    #[test]
    fn topic_shift_short_reply_is_skipped_not_shifted() {
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let event = Event::Message {
            id: "1".into(),
            channel: "chan-a".into(),
            sender: "u".into(),
            content: "yes".into(), // < 20 chars
        };
        let outcome = orchestrator.detect_topic_shift(&event, &[1.0, 0.0, 0.0]);
        assert_eq!(outcome, TopicShiftOutcome::SkippedShortReply);
        assert!(!outcome.is_shift());
    }

    #[test]
    fn topic_shift_first_turn_in_channel_is_skipped_no_prior() {
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let event = Event::Message {
            id: "1".into(),
            channel: "chan-b".into(),
            sender: "u".into(),
            content: "This is a long enough first message in the channel".into(),
        };
        let outcome = orchestrator.detect_topic_shift(&event, &[1.0, 0.0, 0.0]);
        assert_eq!(outcome, TopicShiftOutcome::SkippedNoPriorTurn);
        assert!(!outcome.is_shift());
    }

    #[test]
    fn topic_shift_similar_embedding_is_not_shifted() {
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let event = Event::Message {
            id: "1".into(),
            channel: "chan-c".into(),
            sender: "u".into(),
            content: "This is a long enough first message in the channel".into(),
        };
        let first = orchestrator.detect_topic_shift(&event, &[1.0, 0.0, 0.0]);
        assert_eq!(first, TopicShiftOutcome::SkippedNoPriorTurn);

        let follow_up = Event::Message {
            id: "2".into(),
            channel: "chan-c".into(),
            sender: "u".into(),
            content: "This is another long enough message about the same topic".into(),
        };
        let outcome = orchestrator.detect_topic_shift(&follow_up, &[0.99, 0.01, 0.0]);
        assert_eq!(outcome, TopicShiftOutcome::NotShifted);
        assert!(!outcome.is_shift());
    }

    #[test]
    fn topic_shift_dissimilar_embedding_is_shifted() {
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let event = Event::Message {
            id: "1".into(),
            channel: "chan-d".into(),
            sender: "u".into(),
            content: "This is a long enough first message about cooking recipes".into(),
        };
        let first = orchestrator.detect_topic_shift(&event, &[1.0, 0.0, 0.0]);
        assert_eq!(first, TopicShiftOutcome::SkippedNoPriorTurn);

        let unrelated = Event::Message {
            id: "2".into(),
            channel: "chan-d".into(),
            sender: "u".into(),
            content: "Completely different subject about rocket engine design".into(),
        };
        let outcome = orchestrator.detect_topic_shift(&unrelated, &[0.0, 1.0, 0.0]);
        assert_eq!(outcome, TopicShiftOutcome::Shifted);
        assert!(outcome.is_shift());
    }

    #[test]
    fn topic_shift_poisoned_cache_recovers_instead_of_permanently_disabling() {
        let orchestrator = Orchestrator::new(CerebellumConfig::default());
        let event = Event::Message {
            id: "1".into(),
            channel: "chan-e".into(),
            sender: "u".into(),
            content: "This is a long enough first message about astronomy".into(),
        };
        // Seed a prior embedding so the poisoned-lock branch (not the
        // no-prior-turn branch) is what gets exercised after recovery.
        let _ = orchestrator.detect_topic_shift(&event, &[1.0, 0.0, 0.0]);

        // Poison the mutex by panicking while holding the lock.
        let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = orchestrator.topic_embeddings.lock().unwrap();
            panic!("deliberate poison for test");
        }));
        assert!(poison_result.is_err(), "the panic should have poisoned the mutex");
        assert!(
            orchestrator.topic_embeddings.lock().is_err(),
            "mutex should now report poisoned"
        );

        // Detection must recover the guard and keep working, not permanently
        // return NotShifted/false forever (S5 recovery path).
        let follow_up = Event::Message {
            id: "2".into(),
            channel: "chan-e".into(),
            sender: "u".into(),
            content: "Unrelated topic entirely about deep sea biology now".into(),
        };
        let outcome = orchestrator.detect_topic_shift(&follow_up, &[0.0, 1.0, 0.0]);
        assert_eq!(
            outcome,
            TopicShiftOutcome::Shifted,
            "detection must recover from a poisoned lock and correctly detect the shift, not silently stay disabled"
        );
    }

    // ── ADR-0015 S1/S2 dead-code regression guards ──────────────────────────

    #[test]
    fn actions_rs_has_no_orphaned_detect_topic_shift_stub() {
        let contents = include_str!("actions.rs");
        assert!(
            !contents.contains("detect_topic_shift_action"),
            "detect_topic_shift_action was deleted as dead code per ADR-0015; do not re-add it"
        );
        assert!(
            !contents.contains("\"detect_topic_shift\" =>"),
            "the detect_topic_shift dispatch arm was deleted per ADR-0015; do not re-add it"
        );
    }

    #[test]
    fn classify_px_no_longer_references_deleted_detect_topic_shift_action() {
        let contents = include_str!("../../../../praxis/procedures/classify.px");
        assert!(
            !contents.contains("detect_topic_shift {"),
            "classify.px must not call the deleted detect_topic_shift action (ADR-0015)"
        );
    }

    // ── build_authorization_context ───────────────────────────────────────────

    #[test]
    fn auth_ctx_message_is_not_destructive_or_external() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "hello".into(),
        };
        let ctx = build_authorization_context(&event);
        assert!(!ctx.payload["is_destructive"].as_bool().unwrap_or(true));
        assert!(!ctx.payload["is_external"].as_bool().unwrap_or(true));
    }

    #[test]
    fn auth_ctx_destructive_tool_sets_is_destructive() {
        let event = Event::ToolResult {
            tool_call_id: "tc1".into(),
            tool_name: "delete_file".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ctx = build_authorization_context(&event);
        assert!(ctx.payload["is_destructive"].as_bool().unwrap_or(false));
        assert!(!ctx.payload["is_external"].as_bool().unwrap_or(true));
    }

    #[test]
    fn auth_ctx_external_tool_sets_is_external() {
        let event = Event::ToolResult {
            tool_call_id: "tc2".into(),
            tool_name: "send_email".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ctx = build_authorization_context(&event);
        assert!(ctx.payload["is_external"].as_bool().unwrap_or(false));
        assert!(!ctx.payload["is_destructive"].as_bool().unwrap_or(true));
    }

    #[test]
    fn auth_ctx_read_tool_is_neither_destructive_nor_external() {
        let event = Event::ToolResult {
            tool_call_id: "tc3".into(),
            tool_name: "read_config".into(),
            content: "{}".into(),
            is_error: false,
        };
        let ctx = build_authorization_context(&event);
        assert!(!ctx.payload["is_destructive"].as_bool().unwrap_or(true));
        assert!(!ctx.payload["is_external"].as_bool().unwrap_or(true));
    }

    // ── gate-level integration via build_authorization_context ─────────────

    #[test]
    fn gate_level5_for_message_event() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "tell me a joke".into(),
        };
        let ctx = build_authorization_context(&event);
        assert_eq!(AuthorizationGate.evaluate(&ctx), RuleResult::Pass);
    }

    #[test]
    fn gate_level4_for_destructive_tool_result() {
        let event = Event::ToolResult {
            tool_call_id: "tc".into(),
            tool_name: "delete_record".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ctx = build_authorization_context(&event);
        assert!(
            matches!(AuthorizationGate.evaluate(&ctx), RuleResult::Gate { .. }),
            "destructive tool should trigger approval gate"
        );
    }

    #[test]
    fn gate_level4_for_external_tool_result() {
        let event = Event::ToolResult {
            tool_call_id: "tc".into(),
            tool_name: "post_to_slack".into(),
            content: "ok".into(),
            is_error: false,
        };
        let ctx = build_authorization_context(&event);
        assert!(
            matches!(AuthorizationGate.evaluate(&ctx), RuleResult::Gate { .. }),
            "external tool should trigger approval gate"
        );
    }

    // ── CerebellumError display ───────────────────────────────────────────────

    #[test]
    fn authorization_blocked_error_displays_reason() {
        let err = CerebellumError::AuthorizationBlocked {
            reason: "hard constraint C-9999 violated".into(),
        };
        let msg = err.to_string();
        assert!(msg.contains("authorization blocked"), "got: {msg}");
        assert!(msg.contains("hard constraint C-9999"), "got: {msg}");
    }
}
pub mod context_manager;
