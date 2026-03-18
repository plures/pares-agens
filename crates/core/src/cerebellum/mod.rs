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

pub mod pipeline;
pub mod router;

use crate::event::Event;
use crate::memory::PluresLm;
use crate::procedure::{Procedure, ProcedureRegistry};

use async_trait::async_trait;
use tracing::{debug, info, instrument};

// ── routing decision ─────────────────────────────────────────────────────────

/// Where the cerebellum decides to send an event.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Conscious agent only (fast path).
    Conscious,
    /// Both conscious and subconscious in parallel.
    /// The `reason` field is injected into the subconscious prompt.
    Deep { reason: String },
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
        }
    }
}

// ── cerebellum context ───────────────────────────────────────────────────────

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
}

// ── cerebellum ───────────────────────────────────────────────────────────────

/// The Cerebellum orchestrator.
///
/// Stateless — all persistent state lives in PluresDB via the `PluresLm`
/// memory client. The cerebellum reads from memory and procedures, makes
/// routing decisions, and produces enriched contexts for downstream agents.
pub struct Cerebellum {
    pub config: CerebellumConfig,
}

impl Cerebellum {
    pub fn new(config: CerebellumConfig) -> Self {
        Self { config }
    }

    /// Main entry point: preprocess an event into an enriched context.
    ///
    /// 1. Autorecall — retrieve + compress memories
    /// 2. Route — decide conscious / deep / procedural / drop
    /// 3. Package context for downstream agents
    #[instrument(skip(self, memory, _registry))]
    pub async fn preprocess(
        &self,
        event: &Event,
        memory: &PluresLm,
        _registry: &ProcedureRegistry,
    ) -> Result<CerebellumContext, CerebellumError> {
        // 1. Autorecall
        let query = extract_query(event);
        let learned_context = if let Some(q) = &query {
            let memories = memory
                .recall(q, self.config.recall_limit, &[])
                .await
                .map_err(|e| CerebellumError::Memory(e.to_string()))?;
            memory.inject_context(&memories, None)
        } else {
            String::new()
        };

        info!(
            event_kind = event.kind(),
            context_len = learned_context.len(),
            "autorecall complete"
        );

        // 2. Route
        let route = router::decide(event, &learned_context, &self.config);

        debug!(?route, "routing decision");

        // 3. Package
        Ok(CerebellumContext {
            event: event.clone(),
            learned_context,
            route,
            guidance: vec![],
        })
    }
}

/// Cerebellum-level errors.
#[derive(Debug, thiserror::Error)]
pub enum CerebellumError {
    #[error("memory error: {0}")]
    Memory(String),
    #[error("procedure error: {0}")]
    Procedure(String),
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
        Event::StateChange { key, new_value, .. } => {
            Some(format!("{}: {}", key, new_value))
        }
        // Timer and tool results don't trigger autorecall
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
