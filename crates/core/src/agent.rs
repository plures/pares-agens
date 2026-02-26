//! High-level agent abstraction and in-memory storage for testing/development.
//!
//! [`Agent`] is the top-level entry point used by channel adapters (stdin,
//! Telegram) to process inbound [`Event`]s and produce an optional response.
//!
//! [`Memory`] is the trait implemented by storage backends.  [`InMemory`]
//! provides a simple in-process implementation suitable for tests and the
//! first-run experience before a persistent store is configured.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::event::Event;

// ---------------------------------------------------------------------------
// Memory trait
// ---------------------------------------------------------------------------

/// Trait for agent memory storage.
///
/// Implementations persist conversation content and support fuzzy recall.
#[async_trait]
pub trait Memory: Send + Sync {
    /// Persist `content` to memory.
    async fn capture(&self, content: &str);

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
/// All entries are stored in a `Vec<String>` guarded by a `Mutex`.
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
    async fn capture(&self, content: &str) {
        self.entries
            .lock()
            .expect("lock poisoned")
            .push(content.to_string());
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, String> {
        let q = query.to_lowercase();
        let entries = self.entries.lock().map_err(|e| e.to_string())?;
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
/// # Current behaviour
///
/// For [`Event::Message`] events the agent:
/// 1. Captures the message content in memory.
/// 2. Returns an [`Event::ModelResponse`] with `"Echo: {content}"`.
///
/// All other event kinds return `None`.
pub struct Agent {
    memory: Arc<dyn Memory + Send + Sync>,
}

impl Agent {
    /// Create a new agent backed by `memory`.
    pub fn new(memory: Arc<dyn Memory + Send + Sync>) -> Self {
        Self { memory }
    }

    /// Handle a single event and optionally return a response event.
    pub async fn handle_event(&self, event: Event) -> Option<Event> {
        match event {
            Event::Message { ref id, ref content, .. } => {
                self.memory.capture(content).await;
                Some(Event::ModelResponse {
                    request_id: id.clone(),
                    model: "echo".into(),
                    content: format!("Echo: {content}"),
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
        mem.capture("hello world").await;
        mem.capture("goodbye world").await;
        mem.capture("unrelated").await;
        let results = mem.recall("hello").await.unwrap();
        assert_eq!(results, vec!["hello world"]);
    }

    #[tokio::test]
    async fn in_memory_recall_case_insensitive() {
        let mem = InMemory::new();
        mem.capture("Hello World").await;
        let results = mem.recall("hello").await.unwrap();
        assert_eq!(results.len(), 1);
    }

    #[tokio::test]
    async fn in_memory_recall_empty_when_no_match() {
        let mem = InMemory::new();
        mem.capture("something else").await;
        let results = mem.recall("hello").await.unwrap();
        assert!(results.is_empty());
    }
}
