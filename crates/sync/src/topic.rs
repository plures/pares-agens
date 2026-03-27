//! Topic subscriptions — track which Hyperswarm DHT topics this device has
//! joined and manage per-topic metadata.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{SyncError, SyncTopic};

// ── TopicSubscription ─────────────────────────────────────────────────────────

/// Metadata for a single active Hyperswarm DHT topic subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicSubscription {
    /// The subscribed topic.
    pub topic: SyncTopic,

    /// UTC timestamp when this device joined the topic.
    pub joined_at: DateTime<Utc>,

    /// Number of peers currently known on this topic.
    pub peer_count: usize,

    /// Whether the subscription is active.
    pub active: bool,
}

impl TopicSubscription {
    /// Create a new active subscription for `topic`.
    #[must_use]
    pub fn new(topic: SyncTopic) -> Self {
        Self {
            topic,
            joined_at: Utc::now(),
            peer_count: 0,
            active: true,
        }
    }

    /// Deactivate the subscription.
    pub fn leave(&mut self) {
        self.active = false;
        self.peer_count = 0;
    }

    /// Update the peer count for this topic.
    pub fn set_peer_count(&mut self, count: usize) {
        self.peer_count = count;
    }
}

// ── TopicManager ──────────────────────────────────────────────────────────────

/// Manages the set of active Hyperswarm DHT topic subscriptions.
///
/// The manager ensures idempotent subscribe/unsubscribe semantics and provides
/// read access to current subscription metadata.
#[derive(Debug, Default)]
pub struct TopicManager {
    subscriptions: HashMap<SyncTopic, TopicSubscription>,
}

impl TopicManager {
    /// Create a new, empty `TopicManager`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to `topic`.  Returns a reference to the (new or reactivated)
    /// subscription.
    ///
    /// If an active subscription already exists this is a no-op and the
    /// existing subscription is returned.  If an inactive subscription exists
    /// (i.e. the topic was previously left) it is reactivated: `active` is
    /// reset to `true`, `peer_count` is cleared, and `joined_at` is refreshed.
    pub fn subscribe(&mut self, topic: SyncTopic) -> &TopicSubscription {
        match self.subscriptions.entry(topic) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let sub = entry.get_mut();
                if !sub.active {
                    sub.active = true;
                    sub.peer_count = 0;
                    sub.joined_at = Utc::now();
                }
                entry.into_mut()
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let topic_key = *entry.key();
                entry.insert(TopicSubscription::new(topic_key))
            }
        }
    }

    /// Unsubscribe from `topic`.
    ///
    /// Marks the subscription as inactive.  Returns `true` when the
    /// subscription existed, `false` when the topic was not subscribed.
    pub fn unsubscribe(&mut self, topic: SyncTopic) -> bool {
        if let Some(sub) = self.subscriptions.get_mut(&topic) {
            sub.leave();
            true
        } else {
            false
        }
    }

    /// Return a reference to the active subscription for `topic`.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::TopicNotSubscribed`] when the topic has not been
    /// subscribed or when the subscription is inactive.
    pub fn get(&self, topic: SyncTopic) -> Result<&TopicSubscription, SyncError> {
        match self.subscriptions.get(&topic) {
            Some(sub) if sub.active => Ok(sub),
            _ => Err(SyncError::TopicNotSubscribed(topic.as_key().to_string())),
        }
    }

    /// Return `true` when `topic` is currently subscribed (and active).
    #[must_use]
    pub fn is_subscribed(&self, topic: SyncTopic) -> bool {
        self.subscriptions.get(&topic).is_some_and(|s| s.active)
    }

    /// Return an iterator over all active subscriptions.
    pub fn active_subscriptions(&self) -> impl Iterator<Item = &TopicSubscription> {
        self.subscriptions.values().filter(|s| s.active)
    }

    /// Return the count of active subscriptions.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_subscriptions().count()
    }

    /// Update the peer count for `topic`.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::TopicNotSubscribed`] when the topic is not
    /// subscribed or when the subscription is inactive.
    pub fn set_peer_count(&mut self, topic: SyncTopic, count: usize) -> Result<(), SyncError> {
        match self.subscriptions.get_mut(&topic) {
            Some(sub) if sub.active => {
                sub.set_peer_count(count);
                Ok(())
            }
            _ => Err(SyncError::TopicNotSubscribed(topic.as_key().to_string())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_creates_active_subscription() {
        let mut mgr = TopicManager::new();
        let sub = mgr.subscribe(SyncTopic::MemoryEntries);
        assert!(sub.active);
        assert_eq!(sub.topic, SyncTopic::MemoryEntries);
    }

    #[test]
    fn subscribe_reactivates_inactive_subscription() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::Procedures);
        mgr.unsubscribe(SyncTopic::Procedures);
        assert!(!mgr.is_subscribed(SyncTopic::Procedures));
        // Re-subscribe should reactivate.
        let sub = mgr.subscribe(SyncTopic::Procedures);
        assert!(sub.active);
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn subscribe_is_idempotent() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::Procedures);
        let joined_at = mgr.get(SyncTopic::Procedures).unwrap().joined_at;
        // Second subscribe should not create a new entry.
        mgr.subscribe(SyncTopic::Procedures);
        assert_eq!(mgr.get(SyncTopic::Procedures).unwrap().joined_at, joined_at);
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn unsubscribe_marks_subscription_inactive() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::AgentConfig);
        let removed = mgr.unsubscribe(SyncTopic::AgentConfig);
        assert!(removed);
        assert!(!mgr.is_subscribed(SyncTopic::AgentConfig));
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn unsubscribe_returns_false_for_unknown_topic() {
        let mut mgr = TopicManager::new();
        assert!(!mgr.unsubscribe(SyncTopic::ConversationHistory));
    }

    #[test]
    fn get_returns_error_for_inactive_subscription() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::MemoryEntries);
        mgr.unsubscribe(SyncTopic::MemoryEntries);
        assert!(matches!(
            mgr.get(SyncTopic::MemoryEntries),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn get_returns_error_for_unsubscribed_topic() {
        let mgr = TopicManager::new();
        assert!(matches!(
            mgr.get(SyncTopic::MemoryEntries),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn set_peer_count_updates_subscription() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::Procedures);
        mgr.set_peer_count(SyncTopic::Procedures, 3).unwrap();
        assert_eq!(mgr.get(SyncTopic::Procedures).unwrap().peer_count, 3);
    }

    #[test]
    fn set_peer_count_errors_on_inactive_subscription() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::AgentConfig);
        mgr.unsubscribe(SyncTopic::AgentConfig);
        assert!(matches!(
            mgr.set_peer_count(SyncTopic::AgentConfig, 3),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn set_peer_count_errors_on_unsubscribed_topic() {
        let mut mgr = TopicManager::new();
        assert!(matches!(
            mgr.set_peer_count(SyncTopic::AgentConfig, 1),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn active_subscriptions_excludes_left_topics() {
        let mut mgr = TopicManager::new();
        mgr.subscribe(SyncTopic::MemoryEntries);
        mgr.subscribe(SyncTopic::AgentConfig);
        mgr.unsubscribe(SyncTopic::AgentConfig);
        assert_eq!(mgr.active_count(), 1);
        let active: Vec<_> = mgr.active_subscriptions().collect();
        assert_eq!(active[0].topic, SyncTopic::MemoryEntries);
    }
}
