use std::sync::Arc;

use async_trait::async_trait;

use crate::{
    event::Event,
    memory::{format_context, MemoryCategory, MemoryStore, DEFAULT_BUDGET_CHARS, DEFAULT_RECALL_LIMIT},
    procedure::Procedure,
};

/// Procedure that fires on every inbound [`Event::Message`] and injects
/// relevant memories as context before the message reaches the model.
///
/// # Behaviour
///
/// 1. Performs a vector search of the memory store against the message content.
/// 2. Excludes configured categories (e.g. `ProjectContext`).
/// 3. Formats the results within a character budget.
/// 4. Returns an enriched [`Event::Message`] with the recall block prepended
///    to the original content.  If no relevant memories are found the original
///    event is returned unchanged.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use pares_agens_core::{
///     handlers::auto_recall::AutoRecall,
///     memory::MemoryCategory,
/// };
/// // let store: Arc<dyn MemoryStore> = Arc::new(MyStore);
/// // let procedure = AutoRecall::new(store)
/// //     .exclude(vec![MemoryCategory::ProjectContext])
/// //     .budget(4_000);
/// ```
pub struct AutoRecall {
    store: Arc<dyn MemoryStore>,
    excluded_categories: Vec<MemoryCategory>,
    recall_limit: usize,
    budget_chars: usize,
}

impl AutoRecall {
    /// Create an `AutoRecall` procedure backed by `store` with sensible defaults.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self {
            store,
            excluded_categories: vec![MemoryCategory::ProjectContext],
            recall_limit: DEFAULT_RECALL_LIMIT,
            budget_chars: DEFAULT_BUDGET_CHARS,
        }
    }

    /// Override the list of memory categories to exclude from recall.
    pub fn exclude(mut self, categories: Vec<MemoryCategory>) -> Self {
        self.excluded_categories = categories;
        self
    }

    /// Override the character budget for the injected context block.
    pub fn budget(mut self, chars: usize) -> Self {
        self.budget_chars = chars;
        self
    }

    /// Override the maximum number of memories fetched from the store.
    pub fn limit(mut self, n: usize) -> Self {
        self.recall_limit = n;
        self
    }
}

#[async_trait]
impl Procedure for AutoRecall {
    fn name(&self) -> &str {
        "auto_recall"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let Event::Message {
            id,
            channel,
            sender,
            content,
        } = event
        else {
            return vec![];
        };

        let memories = self
            .store
            .recall(content, self.recall_limit, &self.excluded_categories)
            .await;

        if memories.is_empty() {
            tracing::debug!("auto_recall: no relevant memories found");
            return vec![event.clone()];
        }

        tracing::info!(
            count = memories.len(),
            "auto_recall: injecting recalled context"
        );

        let context_block = format_context(&memories, self.budget_chars);
        let enriched_content = format!("{context_block}\n---\n\n{content}");

        vec![Event::Message {
            id: id.clone(),
            channel: channel.clone(),
            sender: sender.clone(),
            content: enriched_content,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{Exchange, Memory, MemoryCategory, MemoryStore},
        procedure::Procedure,
    };

    struct MockStore {
        memories: Vec<Memory>,
    }

    #[async_trait]
    impl MemoryStore for MockStore {
        async fn recall(
            &self,
            _query: &str,
            limit: usize,
            exclude_categories: &[MemoryCategory],
        ) -> Vec<Memory> {
            self.memories
                .iter()
                .filter(|m| !exclude_categories.contains(&m.category))
                .take(limit)
                .cloned()
                .collect()
        }

        async fn capture(&self, _exchange: &Exchange) {}
    }

    fn make_message(content: &str) -> Event {
        Event::Message {
            id: "1".into(),
            channel: "test".into(),
            sender: "user".into(),
            content: content.into(),
        }
    }

    #[tokio::test]
    async fn auto_recall_injects_context_when_memories_found() {
        let store = Arc::new(MockStore {
            memories: vec![Memory {
                id: "m1".into(),
                content: "I prefer Rust for systems work.".into(),
                category: MemoryCategory::Preference,
                score: 0.9,
            }],
        });
        let procedure = AutoRecall::new(store);
        let result = procedure.execute(&make_message("What language should I use?")).await;

        assert_eq!(result.len(), 1);
        if let Event::Message { content, .. } = &result[0] {
            assert!(
                content.contains("I prefer Rust for systems work."),
                "recalled memory should appear in enriched content"
            );
            assert!(
                content.contains("What language should I use?"),
                "original message should be preserved"
            );
        } else {
            panic!("expected Message event");
        }
    }

    #[tokio::test]
    async fn auto_recall_returns_original_when_no_memories() {
        let store = Arc::new(MockStore { memories: vec![] });
        let procedure = AutoRecall::new(store);
        let event = make_message("Hello there.");
        let result = procedure.execute(&event).await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], event);
    }

    #[tokio::test]
    async fn auto_recall_excludes_project_context_by_default() {
        let store = Arc::new(MockStore {
            memories: vec![
                Memory {
                    id: "m1".into(),
                    content: "I prefer Rust for systems work.".into(),
                    category: MemoryCategory::Preference,
                    score: 0.9,
                },
                Memory {
                    id: "m2".into(),
                    content: "This is a long project context entry that should be excluded.".into(),
                    category: MemoryCategory::ProjectContext,
                    score: 0.8,
                },
            ],
        });
        let procedure = AutoRecall::new(store);
        let result = procedure
            .execute(&make_message("Tell me about the project."))
            .await;

        if let Event::Message { content, .. } = &result[0] {
            assert!(
                content.contains("I prefer Rust"),
                "preference memory should be included"
            );
            assert!(
                !content.contains("project context entry"),
                "project-context memory should be excluded"
            );
        }
    }

    #[tokio::test]
    async fn auto_recall_ignores_non_message_events() {
        let store = Arc::new(MockStore { memories: vec![] });
        let procedure = AutoRecall::new(store);
        let timer = Event::Timer {
            id: "t1".into(),
            name: "tick".into(),
            recurring: false,
        };
        let result = procedure.execute(&timer).await;
        assert!(result.is_empty());
    }
}
