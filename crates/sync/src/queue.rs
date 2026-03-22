//! Offline-first change queue — buffers [`ChangeEvent`]s while peers are
//! unreachable, then delivers them as a batch when the peer reconnects.
//!
//! The queue is purely in-memory.  Persistence across restarts is handled by
//! the PluresDB-backed state store in the `tauri-app` crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::SyncTopic;

// ── ChangeEvent ───────────────────────────────────────────────────────────────

/// A single local state change that must be delivered to remote peers.
///
/// Change events are the unit of sync.  They carry a diff payload (not the
/// full state) to keep bandwidth usage minimal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEvent {
    /// Unique event identifier (UUID v4 string).
    pub id: String,

    /// The sync topic this change belongs to.
    pub topic: SyncTopic,

    /// JSON payload representing the diff / delta for this change.
    ///
    /// The exact schema is domain-specific and interpreted by the conflict
    /// resolver.  Callers should prefer minimal diffs over full state snapshots.
    pub payload: serde_json::Value,

    /// UTC timestamp when the change was created locally.
    pub created_at: DateTime<Utc>,

    /// Number of delivery attempts made so far.
    pub attempt_count: u32,
}

impl ChangeEvent {
    /// Create a new [`ChangeEvent`] with zero delivery attempts.
    #[must_use]
    pub fn new(topic: SyncTopic, payload: serde_json::Value) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            topic,
            payload,
            created_at: Utc::now(),
            attempt_count: 0,
        }
    }

    /// Increment the delivery attempt counter.
    pub fn record_attempt(&mut self) {
        self.attempt_count += 1;
    }
}

// ── OfflineQueue ──────────────────────────────────────────────────────────────

/// A FIFO queue of [`ChangeEvent`]s accumulated while the local device is
/// offline or while no peers are reachable.
///
/// When a peer reconnects, call [`OfflineQueue::drain`] to consume all queued
/// events and transmit them.  Events for a specific topic can be filtered
/// with [`OfflineQueue::drain_topic`].
///
/// # Bandwidth-awareness
///
/// The queue stores diff payloads, not full state snapshots.  It is the
/// caller's responsibility to produce minimal diffs before calling
/// [`OfflineQueue::push`].
#[derive(Debug, Default)]
pub struct OfflineQueue {
    events: Vec<ChangeEvent>,
}

impl OfflineQueue {
    /// Create a new, empty `OfflineQueue`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a [`ChangeEvent`] to the tail of the queue.
    pub fn push(&mut self, event: ChangeEvent) {
        self.events.push(event);
    }

    /// Convenience wrapper: create and enqueue a new event in one call.
    pub fn enqueue(&mut self, topic: SyncTopic, payload: serde_json::Value) -> &ChangeEvent {
        let event = ChangeEvent::new(topic, payload);
        self.events.push(event);
        self.events.last().expect("just pushed")
    }

    /// Drain all queued events, returning them in FIFO order.
    ///
    /// The queue is empty after this call.
    pub fn drain(&mut self) -> Vec<ChangeEvent> {
        std::mem::take(&mut self.events)
    }

    /// Drain only the events belonging to `topic`, leaving others intact.
    pub fn drain_topic(&mut self, topic: SyncTopic) -> Vec<ChangeEvent> {
        let mut drained = Vec::new();
        let mut remaining = Vec::new();
        for event in std::mem::take(&mut self.events) {
            if event.topic == topic {
                drained.push(event);
            } else {
                remaining.push(event);
            }
        }
        self.events = remaining;
        drained
    }

    /// Peek at the next event without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&ChangeEvent> {
        self.events.first()
    }

    /// Return the number of queued events.
    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Return `true` when no events are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Remove all events from the queue.
    pub fn clear(&mut self) {
        self.events.clear();
    }

    /// Return the count of events for a specific topic.
    #[must_use]
    pub fn count_for_topic(&self, topic: SyncTopic) -> usize {
        self.events.iter().filter(|e| e.topic == topic).count()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn queue_starts_empty() {
        let q = OfflineQueue::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn enqueue_increases_length() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::MemoryEntries, json!({"id": "m1"}));
        q.enqueue(SyncTopic::AgentConfig, json!({"key": "v"}));
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn drain_returns_all_events_in_fifo_order() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::MemoryEntries, json!({"seq": 1}));
        q.enqueue(SyncTopic::MemoryEntries, json!({"seq": 2}));
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].payload["seq"], 1);
        assert_eq!(drained[1].payload["seq"], 2);
        assert!(q.is_empty());
    }

    #[test]
    fn drain_topic_only_removes_matching_topic() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::MemoryEntries, json!({"id": "m1"}));
        q.enqueue(SyncTopic::AgentConfig, json!({"id": "c1"}));
        q.enqueue(SyncTopic::MemoryEntries, json!({"id": "m2"}));

        let drained = q.drain_topic(SyncTopic::MemoryEntries);
        assert_eq!(drained.len(), 2);
        assert_eq!(q.len(), 1);
        assert_eq!(q.peek().unwrap().topic, SyncTopic::AgentConfig);
    }

    #[test]
    fn clear_empties_the_queue() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::Procedures, json!({}));
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn count_for_topic_returns_correct_count() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::ConversationHistory, json!({}));
        q.enqueue(SyncTopic::ConversationHistory, json!({}));
        q.enqueue(SyncTopic::MemoryEntries, json!({}));
        assert_eq!(q.count_for_topic(SyncTopic::ConversationHistory), 2);
        assert_eq!(q.count_for_topic(SyncTopic::MemoryEntries), 1);
        assert_eq!(q.count_for_topic(SyncTopic::AgentConfig), 0);
    }

    #[test]
    fn change_event_record_attempt_increments_counter() {
        let mut event = ChangeEvent::new(SyncTopic::Procedures, json!({"op": "add"}));
        assert_eq!(event.attempt_count, 0);
        event.record_attempt();
        event.record_attempt();
        assert_eq!(event.attempt_count, 2);
    }

    #[test]
    fn peek_returns_first_event_without_removing() {
        let mut q = OfflineQueue::new();
        q.enqueue(SyncTopic::MemoryEntries, json!({"seq": 1}));
        q.enqueue(SyncTopic::MemoryEntries, json!({"seq": 2}));
        let peeked = q.peek().unwrap();
        assert_eq!(peeked.payload["seq"], 1);
        assert_eq!(q.len(), 2);
    }
}
