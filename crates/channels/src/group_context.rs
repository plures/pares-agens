//! Rolling message buffer for group chat context tracking.
//!
//! Stores recent messages per chat ID so the bot can inject conversational
//! context when it is triggered in a group chat, even if it didn't respond to
//! the preceding messages.

use std::collections::{HashMap, VecDeque};

/// A single observed group message.
#[derive(Debug, Clone)]
pub struct GroupMessage {
    /// Display name of the sender.
    pub sender: String,
    /// Text content of the message.
    pub text: String,
    /// Unix timestamp (seconds).
    pub timestamp: i64,
}

/// Per-chat rolling buffer of recent messages.
#[derive(Debug)]
pub struct GroupContextBuffer {
    /// Maximum messages to retain per chat.
    capacity: usize,
    /// chat_id → ring buffer of messages.
    buffers: HashMap<i64, VecDeque<GroupMessage>>,
}

impl GroupContextBuffer {
    /// Create a new buffer with the given per-chat capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            buffers: HashMap::new(),
        }
    }

    /// Record an observed message in the given chat.
    pub fn push(&mut self, chat_id: i64, msg: GroupMessage) {
        let buf = self.buffers.entry(chat_id).or_insert_with(|| VecDeque::with_capacity(self.capacity));
        if buf.len() >= self.capacity {
            buf.pop_front();
        }
        buf.push_back(msg);
    }

    /// Format recent context for prompt injection.
    ///
    /// Returns `None` if there are no buffered messages for this chat.
    pub fn format_context(&self, chat_id: i64) -> Option<String> {
        let buf = self.buffers.get(&chat_id)?;
        if buf.is_empty() {
            return None;
        }
        let mut lines = Vec::with_capacity(buf.len() + 1);
        lines.push("Recent group messages:".to_string());
        for m in buf.iter() {
            lines.push(format!("[{}]: {}", m.sender, m.text));
        }
        Some(lines.join("\n"))
    }

    /// Clear the buffer for a specific chat.
    #[allow(dead_code)]
    pub fn clear_chat(&mut self, chat_id: i64) {
        self.buffers.remove(&chat_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_format_context() {
        let mut buf = GroupContextBuffer::new(3);
        buf.push(1, GroupMessage { sender: "Alice".into(), text: "hello".into(), timestamp: 100 });
        buf.push(1, GroupMessage { sender: "Bob".into(), text: "hi".into(), timestamp: 101 });

        let ctx = buf.format_context(1).unwrap();
        assert!(ctx.contains("[Alice]: hello"));
        assert!(ctx.contains("[Bob]: hi"));
    }

    #[test]
    fn capacity_eviction() {
        let mut buf = GroupContextBuffer::new(2);
        buf.push(1, GroupMessage { sender: "A".into(), text: "1".into(), timestamp: 1 });
        buf.push(1, GroupMessage { sender: "B".into(), text: "2".into(), timestamp: 2 });
        buf.push(1, GroupMessage { sender: "C".into(), text: "3".into(), timestamp: 3 });

        let ctx = buf.format_context(1).unwrap();
        assert!(!ctx.contains("[A]: 1"), "oldest should be evicted");
        assert!(ctx.contains("[B]: 2"));
        assert!(ctx.contains("[C]: 3"));
    }

    #[test]
    fn empty_chat_returns_none() {
        let buf = GroupContextBuffer::new(10);
        assert!(buf.format_context(999).is_none());
    }
}
