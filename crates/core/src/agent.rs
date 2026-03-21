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

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tracing::{debug, error};

use crate::cerebellum::{Cerebellum, Route};
use crate::event::Event;
use crate::memory::PluresLm;
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
    entries: Arc<Mutex<Vec<String>>>,
}

impl InMemory {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
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
/// 2. Captures the message content in the simple [`Memory`] store.
/// 3. Returns an [`Event::ModelResponse`] whose content is augmented with
///    any recalled context when a cerebellum is present.
///
/// All other event kinds return `None`.
pub struct Agent {
    memory: Arc<dyn Memory + Send + Sync>,
    /// Optional cerebellum for autorecall and routing.
    cerebellum: Option<Cerebellum>,
    /// PluresLM memory client passed to the cerebellum's `preprocess()`.
    plures_lm: Option<Arc<PluresLm>>,
    /// Procedure registry passed to `cerebellum.preprocess()`.
    ///
    /// Stored as a field to avoid re-allocating an empty registry on every
    /// message.  Currently unused by `preprocess()` (parameter is `_registry`)
    /// but kept here so the call site is forward-compatible.
    procedure_registry: ProcedureRegistry,
}

impl Agent {
    /// Create a basic agent backed by `memory` (no cerebellum).
    pub fn new(memory: Arc<dyn Memory + Send + Sync>) -> Self {
        Self {
            memory,
            cerebellum: None,
            plures_lm: None,
            procedure_registry: ProcedureRegistry::new(),
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
        }
    }

    /// Handle a single event and optionally return a response event.
    pub async fn handle_event(&self, event: Event) -> Option<Event> {
        // ── Cerebellum: autorecall + routing ─────────────────────────────
        let learned_context = if let (Some(cerebellum), Some(plures_lm)) =
            (&self.cerebellum, &self.plures_lm)
        {
            match cerebellum
                .preprocess(&event, plures_lm, &self.procedure_registry)
                .await
            {
                Ok(ctx) => {
                    debug!(route = ?ctx.route, context_len = ctx.learned_context.len(), "cerebellum preprocessed event");
                    // Drop events the cerebellum determined are noise.
                    if ctx.route == Route::Drop {
                        debug!(event_kind = event.kind(), "cerebellum dropped event (Route::Drop)");
                        return None;
                    }
                    ctx.learned_context
                }
                Err(e) => {
                    error!(error = %e, "agent: cerebellum preprocess failed, continuing without context");
                    String::new()
                }
            }
        } else {
            String::new()
        };

        // ── Event dispatch ────────────────────────────────────────────────
        match event {
            Event::Message { ref id, ref content, .. } => {
                if let Err(e) = self.memory.capture(content).await {
                    error!(error = %e, "agent: failed to capture message in memory");
                }
                // Augment the response with recalled context when available.
                let response_content = if learned_context.is_empty() {
                    format!("Echo: {content}")
                } else {
                    format!("Echo: {content}\n\n## Recalled Context\n{learned_context}")
                };
                Some(Event::ModelResponse {
                    request_id: id.clone(),
                    model: "echo".into(),
                    content: response_content,
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

    fn msg(content: &str) -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn agent_echoes_message() {
        let agent = Agent::new(Arc::new(InMemory::new()));
        let response = agent.handle_event(msg("hello")).await;
        assert!(
            matches!(response, Some(Event::ModelResponse { ref content, .. }) if content == "Echo: hello")
        );
    }

    #[tokio::test]
    async fn agent_captures_message_content() {
        let memory = Arc::new(InMemory::new());
        let agent = Agent::new(Arc::clone(&memory) as Arc<dyn Memory + Send + Sync>);
        agent.handle_event(msg("remember this")).await;
        let recalled = memory.recall("remember").await.unwrap();
        assert_eq!(recalled, vec!["remember this"]);
    }

    #[tokio::test]
    async fn agent_ignores_non_message_events() {
        let agent = Agent::new(Arc::new(InMemory::new()));
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
        Agent::with_cerebellum(Arc::new(InMemory::new()), cerebellum, plures_lm)
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
        let agent =
            Agent::with_cerebellum(Arc::new(InMemory::new()), cerebellum, plures_lm);

        let event = Event::Message {
            id: "q1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: "How do I use async in Rust?".into(),
        };
        let response = agent.handle_event(event).await;
        if let Some(Event::ModelResponse { content, .. }) = response {
            assert!(
                content.contains("Recalled Context"),
                "expected recalled context injected into response, got: {content}"
            );
        } else {
            panic!("expected ModelResponse with recalled context");
        }
    }
}

