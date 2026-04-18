//! Cerebellum — the orchestrator layer of the Three-Agent Cognitive Architecture.
//!
//! The cerebellum receives every inbound event **first**, before the conscious or
//! subconscious agents. It:
//!
//! 1. Runs **autorecall** — retrieves and compresses relevant memories into
//!    learned context.
//! 2. **Routes** the event — decides whether the conscious agent can handle it
//!    alone, or whether the subconscious should also be spawned for deep
//!    analysis.
//! 3. **Assembles** the final response from one or more agent outputs.
//!
//! The cerebellum itself uses a cheap/fast model (or no model at all for
//! pure-procedure routing). Expensive reasoning is delegated to the
//! subconscious.
//!
//! # Design
//!
//! ```text
//! User ──► Cerebellum ──┬──► Conscious  (directed executor)
//!                       └──► Subconscious (deep reasoner, optional)
//!                ▲                │
//!                └────────────────┘  (results flow back)
//! ```

pub mod bridge;
pub mod invoke;
pub mod pipeline;
pub mod router;

use crate::cerebellum::bridge::PluresDbBridge;
use crate::delegation::broker::SubTask;
use crate::event::Event;
use crate::memory::entry::MemoryCategory;
use crate::memory::PluresLm;
use crate::praxis::constraints::AuthorizationGate;
use crate::procedure::{Procedure, ProcedureRegistry};

use async_trait::async_trait;
use pares_agens_praxis::rule::{Rule, RuleContext, RuleResult};
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::{debug, info, instrument, warn};

// ── routing decision ─────────────────────────────────────────────────────────

/// Where the cerebellum decides to send an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Conscious agent only (fast path).
    Conscious,
    /// Both conscious and subconscious in parallel.
    /// The `reason` field is injected into the subconscious prompt.
    Deep {
        /// Human-readable explanation of why the subconscious is being invoked.
        reason: String,
    },
    /// Delegate to specialist sub-agents via the delegation broker.
    Delegate {
        /// Human-readable explanation of why the task is decomposed.
        reason: String,
        /// Sub-tasks created by the cerebellum for specialist agents.
        tasks: Vec<SubTask>,
    },
    /// Pure procedure — no LLM needed, cerebellum handles it directly.
    Procedural,
    /// Drop the event (e.g. noise, heartbeat-ok).
    Drop,
}

// ── cerebellum config ────────────────────────────────────────────────────────

/// Tuning knobs for the cerebellum.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CerebellumConfig {
    /// Maximum memories to recall per event.
    pub recall_limit: usize,
    /// Memory categories to exclude from autorecall.
    pub exclude_categories: Vec<String>,
    /// Whether to run the subconscious at all. If false, all events go to
    /// conscious only.
    pub enable_subconscious: bool,
    /// Complexity threshold (0.0–1.0). Events scoring above this trigger
    /// the subconscious.
    pub complexity_threshold: f32,
    /// Token budget for autorecall context injection (number of tokens).
    pub context_token_budget: usize,
    /// Number of days after which a memory entry is considered stale.
    pub staleness_days: u32,
    /// Cosine similarity threshold above which two entries are considered
    /// duplicates during a cerebellum sweep.
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

// ── cerebellum context ───────────────────────────────────────────────────────

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

/// The enriched context the cerebellum produces for downstream agents.
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

// ── cerebellum ───────────────────────────────────────────────────────────────

/// The Cerebellum orchestrator.
///
/// Stateless — all persistent state lives in PluresDB via the `PluresLm`
/// memory client. The cerebellum reads from memory and procedures, makes
/// routing decisions, and produces enriched contexts for downstream agents.
///
/// When `pluresdb` is `Some`, the cerebellum can delegate procedure pipelines
/// (VectorSearch, Transform, etc.) to the native PluresDB engine for
/// autorecall and compression.  When `None`, the pure-Rust implementations
/// are used as fallback.
pub struct Cerebellum {
    /// Tuning configuration for this cerebellum instance.
    pub config: CerebellumConfig,
    /// Optional PluresDB bridge for native procedure execution.
    pub pluresdb: Option<PluresDbBridge>,
    /// Last topic embedding seen per channel.
    topic_embeddings: Mutex<HashMap<String, Vec<f32>>>,
}

impl Cerebellum {
    /// Create a cerebellum without a PluresDB bridge (pure-Rust fallback).
    pub fn new(config: CerebellumConfig) -> Self {
        Self {
            config,
            pluresdb: None,
            topic_embeddings: Mutex::new(HashMap::new()),
        }
    }

    /// Create a cerebellum with an attached [`PluresDbBridge`].
    pub fn with_bridge(config: CerebellumConfig, bridge: PluresDbBridge) -> Self {
        Self {
            config,
            pluresdb: Some(bridge),
            topic_embeddings: Mutex::new(HashMap::new()),
        }
    }

    /// Main entry point: preprocess an event into an enriched context.
    ///
    /// 1. Autorecall — retrieve + compress memories
    /// 2. Authorization gate (ADR-0012) — evaluate 5-level gate
    /// 3. Route — decide conscious / deep / procedural / drop
    /// 4. Package context for downstream agents
    #[instrument(skip(self, memory, _registry))]
    pub async fn preprocess(
        &self,
        event: &Event,
        memory: &PluresLm,
        _registry: &ProcedureRegistry,
    ) -> Result<CerebellumContext, CerebellumError> {
        // 1. Autorecall
        let query = extract_query(event);
        let mut clear_history = false;
        let learned_context = if let Some(q) = &query {
            let query_embedding = memory
                .embed_text(q)
                .await
                .map_err(|e| CerebellumError::Memory(e.to_string()))?;
            clear_history = self.detect_topic_shift(event, &query_embedding);

            let exclude_categories = parse_excluded_categories(&self.config.exclude_categories);
            let memories = memory
                .recall(q, self.config.recall_limit, &exclude_categories)
                .await
                .map_err(|e| CerebellumError::Memory(e.to_string()))?;
            memory.inject_context(&memories, Some(self.config.context_token_budget))
        } else {
            String::new()
        };

        info!(
            event_kind = event.kind(),
            context_len = learned_context.len(),
            topic_shifted = clear_history,
            "autorecall complete"
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

        // 3. Route (may be overridden by gate levels 2–4)
        let mut route = router::decide(event, &learned_context, &self.config);
        let mut guidance: Vec<String> = vec![];
        let mut approval_required: Option<ApprovalRequest> = None;

        match gate_result {
            // Level 2: skip duplicate — override route to Drop
            RuleResult::Warning { ref message } if message.starts_with("skip:") => {
                debug!(message, "authorization gate: duplicate action suppressed");
                route = Route::Drop;
                guidance.push(message.clone());
            }
            // Level 3: known failure — warn, keep original route
            RuleResult::Warning { ref message } => {
                warn!(message, "authorization gate: known failure warning");
                guidance.push(message.clone());
            }
            // Level 4: destructive/external → require approval
            RuleResult::Gate { ref action, ref rationale } => {
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

        // 4. Package
        Ok(CerebellumContext {
            event: event.clone(),
            learned_context,
            route,
            guidance,
            clear_history,
            approval_required,
        })
    }

    fn detect_topic_shift(&self, event: &Event, current_embedding: &[f32]) -> bool {
        let Some(channel_key) = event_channel_key(event) else {
            return false;
        };

        let mut embeddings = match self.topic_embeddings.lock() {
            Ok(guard) => guard,
            Err(e) => {
                warn!(error = %e, "topic embedding cache poisoned; skipping topic-shift detection");
                return false;
            }
        };
        let shifted = embeddings
            .get(&channel_key)
            .map(|previous| {
                cosine_similarity(previous, current_embedding) < self.config.topic_similarity_threshold
            })
            .unwrap_or(false);
        embeddings.insert(channel_key, current_embedding.to_vec());
        shifted
    }
}

/// Cerebellum-level errors.
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

// ── cerebellum as a Procedure ────────────────────────────────────────────────

/// Adapter that lets the cerebellum participate in the procedure registry
/// as a first-class procedure handling `"message"` events.
pub struct CerebellumProcedure;

#[async_trait]
impl Procedure for CerebellumProcedure {
    fn name(&self) -> &str {
        "cerebellum"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        // In the full system, this is wired through Cerebellum::preprocess.
        // This stub enables registration and dispatch testing.
        debug!(event_kind = event.kind(), "cerebellum procedure stub");
        vec![]
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
        |(dot, norm_a_sq, norm_b_sq), (&x, &y)| {
            (dot + x * y, norm_a_sq + x * x, norm_b_sq + y * y)
        },
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
/// The cerebellum derives gate payload flags from what it can observe in the
/// event.  Flags that would require external queries (e.g. `completed_recently`,
/// `known_failure`) default to `false` here; the orchestration layer may enrich
/// the context further before re-evaluating the gate directly.
///
/// | Payload field | Source |
/// |---------------|--------|
/// | `blocked_by_constraint` | always `false` (handled by executor's PraxisGate) |
/// | `completed_recently` | always `false` (no dedup log in cerebellum) |
/// | `known_failure` | always `false` (no failure log in cerebellum) |
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{
        embed::{EmbeddingProvider, MockEmbedder},
        entry::{MemoryCategory, MemoryEntry},
        store::{InMemoryStore, MemoryStore as _},
    };
    use std::sync::Arc;
    use pares_agens_praxis::rule::RuleResult;

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
    fn default_config() {
        let cfg = CerebellumConfig::default();
        assert_eq!(cfg.recall_limit, 10);
        assert!(cfg.enable_subconscious);
        assert!((cfg.complexity_threshold - 0.7).abs() < f32::EPSILON);
        assert!((cfg.topic_similarity_threshold - 0.72).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn preprocess_clears_history_on_topic_shift_and_restores_on_return() {
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
        let cerebellum = Cerebellum::new(CerebellumConfig::default());
        let registry = ProcedureRegistry::new();

        let rust_msg = Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "How does async Rust work with tokio?".into(),
        };
        let rust_ctx = cerebellum
            .preprocess(&rust_msg, &memory, &registry)
            .await
            .expect("first preprocess should succeed");
        assert!(!rust_ctx.clear_history, "first topic should not clear history");

        let cooking_msg = Event::Message {
            id: "2".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "What is the best way to bake sourdough bread?".into(),
        };
        let cooking_ctx = cerebellum
            .preprocess(&cooking_msg, &memory, &registry)
            .await
            .expect("second preprocess should succeed");
        assert!(cooking_ctx.clear_history, "different topic should clear history");

        let rust_return_msg = Event::Message {
            id: "3".into(),
            channel: "test".into(),
            sender: "u".into(),
            content: "Back to Rust: when should I use async channels?".into(),
        };
        let rust_return_ctx = cerebellum
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
