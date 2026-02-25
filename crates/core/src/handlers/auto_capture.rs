use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    event::Event,
    memory::{detect_category, passes_quality_gate, Exchange, MemoryStore},
    procedure::Procedure,
};

/// Procedure that fires on every [`Event::ModelResponse`] and captures
/// meaningful exchanges into long-term memory via PluresLM.
///
/// # Behaviour
///
/// 1. Runs the quality gate — rejects echoes, git noise, `HEARTBEAT_OK`, and
///    short content.
/// 2. Detects the memory category from content signals (preference, decision,
///    entity, other).
/// 3. Stores the exchange via the [`MemoryStore`].
/// 4. Always returns no follow-up events.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use pares_agens_core::handlers::auto_capture::AutoCapture;
/// // let store: Arc<dyn MemoryStore> = Arc::new(MyStore);
/// // let procedure = AutoCapture::new(store);
/// ```
pub struct AutoCapture {
    store: Arc<dyn MemoryStore>,
}

impl AutoCapture {
    /// Create an `AutoCapture` procedure backed by `store`.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Procedure for AutoCapture {
    fn name(&self) -> &str {
        "auto_capture"
    }

    fn handles(&self) -> &str {
        "model_response"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let Event::ModelResponse {
            request_id,
            model,
            content,
        } = event
        else {
            return vec![];
        };

        if !passes_quality_gate(content) {
            tracing::debug!(
                request_id,
                model,
                "auto_capture: content failed quality gate, skipping"
            );
            return vec![];
        }

        let category = detect_category(content);
        tracing::info!(
            request_id,
            model,
            ?category,
            "auto_capture: storing exchange"
        );

        let exchange = Exchange {
            // The ModelResponse does not carry the original user message; a
            // future enhancement will correlate request_id → user message via
            // PluresDB.  For now we store the agent response content only.
            user_message: String::new(),
            agent_response: content.clone(),
        };

        self.store.capture(&exchange).await;

        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{Exchange, Memory, MemoryCategory, MemoryStore},
        procedure::Procedure,
    };
    use std::sync::Mutex;

    struct RecordingStore {
        captured: Mutex<Vec<Exchange>>,
    }

    impl RecordingStore {
        fn new() -> Self {
            Self {
                captured: Mutex::new(vec![]),
            }
        }

        fn captured_count(&self) -> usize {
            self.captured.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl MemoryStore for RecordingStore {
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
            _exclude_categories: &[MemoryCategory],
        ) -> Vec<Memory> {
            vec![]
        }

        async fn capture(&self, exchange: &Exchange) {
            self.captured.lock().unwrap().push(exchange.clone());
        }
    }

    fn make_model_response(content: &str) -> Event {
        Event::ModelResponse {
            request_id: "req-1".into(),
            model: "qwen3".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn auto_capture_stores_meaningful_response() {
        let store = Arc::new(RecordingStore::new());
        let procedure = AutoCapture::new(Arc::clone(&store) as Arc<dyn MemoryStore>);

        let event = make_model_response(
            "I prefer to use Rust for all systems programming work because of its safety guarantees.",
        );
        let result = procedure.execute(&event).await;

        assert!(result.is_empty(), "auto_capture emits no follow-up events");
        assert_eq!(store.captured_count(), 1, "one exchange should be captured");
    }

    #[tokio::test]
    async fn auto_capture_rejects_heartbeat() {
        let store = Arc::new(RecordingStore::new());
        let procedure = AutoCapture::new(Arc::clone(&store) as Arc<dyn MemoryStore>);

        let result = procedure.execute(&make_model_response("HEARTBEAT_OK")).await;

        assert!(result.is_empty());
        assert_eq!(store.captured_count(), 0, "heartbeat should not be captured");
    }

    #[tokio::test]
    async fn auto_capture_rejects_short_content() {
        let store = Arc::new(RecordingStore::new());
        let procedure = AutoCapture::new(Arc::clone(&store) as Arc<dyn MemoryStore>);

        let result = procedure.execute(&make_model_response("ok")).await;

        assert!(result.is_empty());
        assert_eq!(
            store.captured_count(),
            0,
            "short content should not be captured"
        );
    }

    #[tokio::test]
    async fn auto_capture_rejects_git_noise() {
        let store = Arc::new(RecordingStore::new());
        let procedure = AutoCapture::new(Arc::clone(&store) as Arc<dyn MemoryStore>);

        let git_output = "commit abc123def456\nAuthor: Alice <alice@example.com>\nDate:   Mon Jan 1 00:00:00 2026 +0000\n\n    feat: initial commit";
        let result = procedure.execute(&make_model_response(git_output)).await;

        assert!(result.is_empty());
        assert_eq!(
            store.captured_count(),
            0,
            "git noise should not be captured"
        );
    }

    #[tokio::test]
    async fn auto_capture_ignores_non_model_response_events() {
        let store = Arc::new(RecordingStore::new());
        let procedure = AutoCapture::new(Arc::clone(&store) as Arc<dyn MemoryStore>);

        let msg = Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: "hello".into(),
        };
        let result = procedure.execute(&msg).await;

        assert!(result.is_empty());
        assert_eq!(store.captured_count(), 0);
    }
}
