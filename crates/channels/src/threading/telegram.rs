//! Telegram channel threading adapter.
//!
//! Supports reply-chain threading in normal groups and native forum topics in
//! supergroups with topics enabled.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{ChannelAnchor, ChannelThreading, ThreadCapabilities, ThreadError, ThreadInfo};

/// Telegram-specific threading adapter.
///
/// In forum groups (`is_forum_group = true`), threads map to native Telegram
/// topics. In regular groups/DMs, threads use reply chains anchored to a
/// specific message.
pub struct TelegramThreading {
    /// Whether the target chat is a forum-enabled supergroup.
    pub is_forum_group: bool,
}

impl TelegramThreading {
    /// Create a new Telegram threading adapter.
    pub fn new(is_forum_group: bool) -> Self {
        Self { is_forum_group }
    }
}

#[async_trait]
impl ChannelThreading for TelegramThreading {
    fn capabilities(&self) -> ThreadCapabilities {
        ThreadCapabilities {
            indicators: true,
            reply_chains: true,
            native_threads: self.is_forum_group,
            thread_switcher: true,
            concurrent_display: false,
        }
    }

    async fn on_thread_created(
        &self,
        thread_id: &str,
        topic: &str,
        chat_id: &str,
    ) -> Result<ChannelAnchor, ThreadError> {
        // Return an anchor with metadata for establishing reply chains.
        // In forum groups this would reference a topic_id; in regular chats,
        // a message_id that subsequent replies will target.
        Ok(json!({
            "thread_id": thread_id,
            "topic": topic,
            "chat_id": chat_id,
            "type": if self.is_forum_group { "forum_topic" } else { "reply_chain" },
        }))
    }

    async fn on_thread_switched(
        &self,
        _from_topic: &str,
        to_topic: &str,
        _chat_id: &str,
    ) -> Result<(), ThreadError> {
        // In Telegram, thread switching is implicit via reply targets.
        // For forum groups, the bot would switch which topic it posts to.
        // No channel-side state to update here; the caller handles routing.
        tracing::debug!(to_topic, "Telegram thread switched");
        Ok(())
    }

    async fn format_message_in_thread(&self, topic: &str, content: &str) -> String {
        format!("[📎 {topic}] {content}")
    }

    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String> {
        // Check for reply_to_message thread anchor or forum topic_id.
        if let Some(topic_id) = metadata.get("message_thread_id").and_then(|v| v.as_str()) {
            return Some(topic_id.to_string());
        }
        if let Some(reply_id) = metadata
            .get("reply_to_message_id")
            .and_then(|v| v.as_i64())
        {
            // The reply target message_id can be used to look up which thread
            // owns this anchor. Return it as a string for the core to resolve.
            return Some(format!("reply:{reply_id}"));
        }
        None
    }

    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String {
        let mut output = String::from("🧵 *Threads:*\n");
        for (i, t) in threads.iter().enumerate() {
            let active_marker = if t.is_active { " ✓" } else { "" };
            output.push_str(&format!(
                "{}. {} ({} msgs){}\n",
                i + 1,
                t.topic,
                t.message_count,
                active_marker
            ));
        }
        // Hint for inline keyboard rendering.
        output.push_str("\n_Use /thread <number> to switch_");
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn format_message_prepends_topic() {
        let threading = TelegramThreading::new(false);
        let result = threading
            .format_message_in_thread("debug", "hello world")
            .await;
        assert_eq!(result, "[📎 debug] hello world");
    }

    #[tokio::test]
    async fn format_thread_list_shows_numbered_list() {
        let threading = TelegramThreading::new(false);
        let threads = vec![
            ThreadInfo {
                id: "t1".to_string(),
                topic: "main".to_string(),
                message_count: 5,
                is_active: true,
            },
            ThreadInfo {
                id: "t2".to_string(),
                topic: "debug".to_string(),
                message_count: 3,
                is_active: false,
            },
        ];
        let result = threading.format_thread_list(&threads).await;
        assert!(result.contains("1. main (5 msgs) ✓"));
        assert!(result.contains("2. debug (3 msgs)"));
    }

    #[tokio::test]
    async fn resolve_thread_from_forum_metadata() {
        let threading = TelegramThreading::new(true);
        let metadata = json!({ "message_thread_id": "topic_42" });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("topic_42".to_string()));
    }

    #[tokio::test]
    async fn resolve_thread_from_reply_metadata() {
        let threading = TelegramThreading::new(false);
        let metadata = json!({ "reply_to_message_id": 123 });
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, Some("reply:123".to_string()));
    }

    #[tokio::test]
    async fn resolve_thread_returns_none_for_empty_metadata() {
        let threading = TelegramThreading::new(false);
        let metadata = json!({});
        let result = threading.resolve_thread_from_metadata(&metadata).await;
        assert_eq!(result, None);
    }

    #[tokio::test]
    async fn capabilities_forum_group() {
        let threading = TelegramThreading::new(true);
        let caps = threading.capabilities();
        assert!(caps.native_threads);
        assert!(caps.reply_chains);
    }

    #[tokio::test]
    async fn capabilities_regular_chat() {
        let threading = TelegramThreading::new(false);
        let caps = threading.capabilities();
        assert!(!caps.native_threads);
        assert!(caps.reply_chains);
    }
}
