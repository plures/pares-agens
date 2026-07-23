//! Tauri IPC channel threading adapter.
//!
//! The desktop app supports rich threading via a sidebar panel, pop-out windows,
//! and reply chains within threads.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChannelAnchor, ChannelThreading, ThreadCapabilities, ThreadError, ThreadInfo};

/// Tauri-specific threading adapter.
///
/// The Tauri desktop UI has full threading support: a sidebar for thread
/// navigation, native isolation via separate panels, and pop-out windows
/// for concurrent display.
pub struct TauriThreading;

impl TauriThreading {
    /// Create a new Tauri threading adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TauriThreading {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelThreading for TauriThreading {
    fn capabilities(&self) -> ThreadCapabilities {
        ThreadCapabilities {
            indicators: true,
            reply_chains: true,
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
        // Return structured anchor for the frontend to create a sidebar entry.
        Ok(json!({
            "thread_id": thread_id,
            "topic": topic,
            "type": "sidebar_panel",
            "ui_hint": "create_panel",
        }))
    }

    async fn on_thread_switched(
        &self,
        from_topic: &str,
        to_topic: &str,
        _chat_id: &str,
    ) -> Result<(), ThreadError> {
        tracing::debug!(from_topic, to_topic, "Tauri panel switched");
        Ok(())
    }

    async fn format_message_in_thread(&self, topic: &str, content: &str) -> String {
        // Return JSON with thread metadata for frontend rendering.
        json!({
            "topic": topic,
            "content": content,
            "render_hint": "threaded_message",
        })
        .to_string()
    }

    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String> {
        // Tauri frontend sends panel_id or thread_id in IPC messages.
        metadata
            .get("thread_id")
            .or_else(|| metadata.get("panel_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String {
        // Full JSON array with metadata for the sidebar component.
        let items: Vec<Value> = threads
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "topic": t.topic,
                    "message_count": t.message_count,
                    "is_active": t.is_active,
                    "ui_type": "sidebar_entry",
                })
            })
            .collect();
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn format_message_returns_json() {
        let threading = TauriThreading::new();
        let result = threading
            .format_message_in_thread("research", "some content")
            .await;
        let parsed: Value = serde_json::from_str(&result).expect("should be valid JSON");
        assert_eq!(parsed["topic"], "research");
        assert_eq!(parsed["content"], "some content");
        assert_eq!(parsed["render_hint"], "threaded_message");
    }

    #[tokio::test]
    async fn format_thread_list_returns_json_array() {
        let threading = TauriThreading::new();
        let threads = vec![
            ThreadInfo {
                id: "t1".to_string(),
                topic: "main".to_string(),
                message_count: 7,
                is_active: true,
            },
            ThreadInfo {
                id: "t2".to_string(),
                topic: "planning".to_string(),
                message_count: 2,
                is_active: false,
            },
        ];
        let result = threading.format_thread_list(&threads).await;
        let parsed: Vec<Value> = serde_json::from_str(&result).expect("should be valid JSON array");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["topic"], "main");
        assert_eq!(parsed[0]["ui_type"], "sidebar_entry");
        assert!(parsed[0]["is_active"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn resolve_thread_from_thread_id() {
        let threading = TauriThreading::new();
        let metadata = json!({ "thread_id": "thread_abc" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("thread_abc".to_string()));
    }

    #[tokio::test]
    async fn resolve_thread_from_panel_id_fallback() {
        let threading = TauriThreading::new();
        let metadata = json!({ "panel_id": "panel_42" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("panel_42".to_string()));
    }

    #[tokio::test]
    async fn capabilities_full_support() {
        let threading = TauriThreading::new();
        let caps = threading.capabilities();
        assert!(caps.indicators);
        assert!(caps.reply_chains);
        assert!(caps.native_threads);
        assert!(caps.thread_switcher);
        assert!(caps.concurrent_display);
    }
}
