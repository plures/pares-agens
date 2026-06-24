//! HTTP channel threading adapter.
//!
//! The HTTP/REST API channel is inherently concurrent — each request carries
//! a `thread_id` parameter and the API exposes a `/v1/threads` endpoint for
//! thread management.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChannelAnchor, ChannelThreading, ThreadCapabilities, ThreadError, ThreadInfo};

/// HTTP-specific threading adapter.
///
/// Thread isolation is handled via request parameters (`thread_id` header or
/// query param). The API is stateless per-request so concurrent access is native.
pub struct HttpThreading;

impl HttpThreading {
    /// Create a new HTTP threading adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for HttpThreading {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelThreading for HttpThreading {
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
        // HTTP threads are created via POST /v1/threads; the anchor is the
        // thread resource URL/ID for subsequent requests.
        Ok(json!({
            "thread_id": thread_id,
            "topic": topic,
            "type": "http_resource",
            "endpoint": format!("/v1/threads/{thread_id}"),
        }))
    }

    async fn on_thread_switched(
        &self,
        _from_topic: &str,
        _to_topic: &str,
        _chat_id: &str,
    ) -> Result<(), ThreadError> {
        // HTTP is stateless — no transition action needed. Clients simply
        // change which thread_id they include in requests.
        Ok(())
    }

    async fn format_message_in_thread(&self, _topic: &str, content: &str) -> String {
        // No modification needed — thread metadata is carried in HTTP headers/params.
        content.to_string()
    }

    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String> {
        // HTTP requests carry thread_id explicitly.
        metadata
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String {
        // Return as JSON array for API consumers.
        let items: Vec<Value> = threads
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "topic": t.topic,
                    "message_count": t.message_count,
                    "is_active": t.is_active,
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn format_message_returns_content_unchanged() {
        let threading = HttpThreading::new();
        let result = threading
            .format_message_in_thread("research", "data payload")
            .await;
        assert_eq!(result, "data payload");
    }

    #[tokio::test]
    async fn format_thread_list_returns_json() {
        let threading = HttpThreading::new();
        let threads = vec![
            ThreadInfo {
                id: "t1".to_string(),
                topic: "main".to_string(),
                message_count: 10,
                is_active: true,
            },
            ThreadInfo {
                id: "t2".to_string(),
                topic: "background".to_string(),
                message_count: 2,
                is_active: false,
            },
        ];
        let result = threading.format_thread_list(&threads).await;
        let parsed: Vec<Value> = serde_json::from_str(&result).expect("should be valid JSON");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["topic"], "main");
        assert_eq!(parsed[1]["message_count"], 2);
    }

    #[tokio::test]
    async fn resolve_thread_from_request_metadata() {
        let threading = HttpThreading::new();
        let metadata = json!({ "thread_id": "thread_xyz" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("thread_xyz".to_string()));
    }

    #[tokio::test]
    async fn capabilities_concurrent() {
        let threading = HttpThreading::new();
        let caps = threading.capabilities();
        assert!(caps.native_threads);
        assert!(caps.concurrent_display);
        assert!(!caps.reply_chains);
    }
}
