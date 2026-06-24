//! TUI channel threading adapter.
//!
//! Supports native tab-based threading with split-pane concurrent display.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChannelAnchor, ChannelThreading, ThreadCapabilities, ThreadError, ThreadInfo};

/// TUI-specific threading adapter.
///
/// The terminal UI uses tabs for thread isolation and supports split-pane
/// views for concurrent thread display.
pub struct TuiThreading;

impl TuiThreading {
    /// Create a new TUI threading adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TuiThreading {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelThreading for TuiThreading {
    fn capabilities(&self) -> ThreadCapabilities {
        ThreadCapabilities {
            indicators: true,
            reply_chains: false,
            native_threads: true,
            thread_switcher: true,
            concurrent_display: true,
        }
    }

    async fn on_thread_created(
        &self,
        thread_id: &str,
        topic: &str,
        _chat_id: &str,
    ) -> Result<ChannelAnchor, ThreadError> {
        // TUI creates a new tab; the anchor carries the tab index hint.
        Ok(json!({
            "thread_id": thread_id,
            "topic": topic,
            "type": "tab",
        }))
    }

    async fn on_thread_switched(
        &self,
        from_topic: &str,
        to_topic: &str,
        _chat_id: &str,
    ) -> Result<(), ThreadError> {
        tracing::debug!(from_topic, to_topic, "TUI tab switched");
        Ok(())
    }

    async fn format_message_in_thread(&self, _topic: &str, content: &str) -> String {
        // Tabs handle isolation — no prefix needed.
        content.to_string()
    }

    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String> {
        // TUI messages carry a tab_id in metadata.
        metadata
            .get("tab_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String {
        // Tab-bar style: [1: topic] [2: topic*] where * marks active.
        threads
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let active = if t.is_active { "*" } else { "" };
                format!("[{}: {}{}]", i + 1, t.topic, active)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn format_message_returns_content_unchanged() {
        let threading = TuiThreading::new();
        let result = threading
            .format_message_in_thread("debug", "hello world")
            .await;
        assert_eq!(result, "hello world");
    }

    #[tokio::test]
    async fn format_thread_list_tab_style() {
        let threading = TuiThreading::new();
        let threads = vec![
            ThreadInfo {
                id: "t1".to_string(),
                topic: "main".to_string(),
                message_count: 5,
                is_active: false,
            },
            ThreadInfo {
                id: "t2".to_string(),
                topic: "debug".to_string(),
                message_count: 3,
                is_active: true,
            },
            ThreadInfo {
                id: "t3".to_string(),
                topic: "research".to_string(),
                message_count: 1,
                is_active: false,
            },
        ];
        let result = threading.format_thread_list(&threads).await;
        assert_eq!(result, "[1: main] [2: debug*] [3: research]");
    }

    #[tokio::test]
    async fn capabilities_supports_concurrent_display() {
        let threading = TuiThreading::new();
        let caps = threading.capabilities();
        assert!(caps.native_threads);
        assert!(caps.concurrent_display);
        assert!(!caps.reply_chains);
    }

    #[tokio::test]
    async fn resolve_thread_from_tab_metadata() {
        let threading = TuiThreading::new();
        let metadata = json!({ "tab_id": "thread_abc" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("thread_abc".to_string()));
    }
}
