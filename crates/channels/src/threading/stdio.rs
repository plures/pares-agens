//! Stdio channel threading adapter.
//!
//! The simplest channel — single-stream text I/O. Threading is indicated via
//! text prefixes and managed through slash commands.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChannelAnchor, ChannelThreading, ThreadCapabilities, ThreadError, ThreadInfo};

/// Stdio-specific threading adapter.
///
/// Since stdio is a single sequential stream, threads are indicated by text
/// prefixes on each line. Thread switching is done via `/thread` commands.
pub struct StdioThreading;

impl StdioThreading {
    /// Create a new Stdio threading adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdioThreading {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelThreading for StdioThreading {
    fn capabilities(&self) -> ThreadCapabilities {
        ThreadCapabilities {
            indicators: true,
            reply_chains: false,
            native_threads: false,
            thread_switcher: true,
            concurrent_display: false,
        }
    }

    async fn on_thread_created(
        &self,
        thread_id: &str,
        topic: &str,
        _chat_id: &str,
    ) -> Result<ChannelAnchor, ThreadError> {
        // Stdio has no persistent anchor — just acknowledge creation.
        Ok(json!({
            "thread_id": thread_id,
            "topic": topic,
            "type": "stdio_label",
        }))
    }

    async fn on_thread_switched(
        &self,
        from_topic: &str,
        to_topic: &str,
        _chat_id: &str,
    ) -> Result<(), ThreadError> {
        tracing::debug!(from_topic, to_topic, "Stdio thread switched");
        Ok(())
    }

    async fn format_message_in_thread(&self, topic: &str, content: &str) -> String {
        // Prepend topic to each line for visual threading in a single stream.
        content
            .lines()
            .map(|line| format!("[{topic}] {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String> {
        // Stdio messages may carry a thread hint from slash command context.
        metadata
            .get("active_thread")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String {
        // Numbered plaintext list suitable for terminal display.
        threads
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let active = if t.is_active { ", active" } else { "" };
                format!("{}. {} ({} msgs{})", i + 1, t.topic, t.message_count, active)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn format_message_prepends_topic_to_each_line() {
        let threading = StdioThreading::new();
        let content = "line one\nline two\nline three";
        let result = threading
            .format_message_in_thread("debug", content)
            .await;
        assert_eq!(
            result,
            "[debug] line one\n[debug] line two\n[debug] line three"
        );
    }

    #[tokio::test]
    async fn format_message_single_line() {
        let threading = StdioThreading::new();
        let result = threading
            .format_message_in_thread("main", "hello")
            .await;
        assert_eq!(result, "[main] hello");
    }

    #[tokio::test]
    async fn format_thread_list_numbered() {
        let threading = StdioThreading::new();
        let threads = vec![
            ThreadInfo {
                id: "t1".to_string(),
                topic: "main".to_string(),
                message_count: 3,
                is_active: true,
            },
            ThreadInfo {
                id: "t2".to_string(),
                topic: "research".to_string(),
                message_count: 5,
                is_active: false,
            },
        ];
        let result = threading.format_thread_list(&threads).await;
        assert_eq!(result, "1. main (3 msgs, active)\n2. research (5 msgs)");
    }

    #[tokio::test]
    async fn capabilities_no_native_threads() {
        let threading = StdioThreading::new();
        let caps = threading.capabilities();
        assert!(!caps.native_threads);
        assert!(!caps.concurrent_display);
        assert!(caps.thread_switcher);
    }

    #[tokio::test]
    async fn resolve_thread_from_active_thread_metadata() {
        let threading = StdioThreading::new();
        let metadata = json!({ "active_thread": "thread_main" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("thread_main".to_string()));
    }
}
