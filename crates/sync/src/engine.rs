//! SyncEngine — top-level orchestrator for P2P device sync.
//!
//! The engine coordinates the lifecycle of topic subscriptions, device
//! pairing, the offline change queue, and conflict resolution.  It is the
//! primary entry-point for higher-level crates such as `tauri-app`.
//!
//! # Responsibilities
//!
//! | Area | Delegated to |
//! |------|-------------|
//! | Topic subscriptions | [`TopicManager`] |
//! | Device registry | [`DeviceRegistry`] |
//! | Pairing flow | [`PairingSession`] |
//! | Offline buffering | [`OfflineQueue`] |
//! | Conflict resolution | [`ConflictResolution`] |

use serde_json::Value;

use crate::{
    conflict::ConflictResolution,
    pairing::PairingSession,
    peer::{DeviceRegistry, DeviceStatus, PairedDevice},
    queue::{ChangeEvent, OfflineQueue},
    topic::TopicManager,
    SyncError, SyncTopic,
};

// ── SyncEngine ────────────────────────────────────────────────────────────────

/// Top-level coordinator for Hyperswarm-backed P2P device sync.
///
/// Create one `SyncEngine` per process and drive it from the application
/// layer.  The engine is intentionally transport-agnostic: callers are
/// expected to wire the actual Hyperswarm socket to the
/// [`SyncEngine::apply_remote_change`] and [`SyncEngine::drain_queue`] APIs.
///
/// # Example
///
/// ```rust
/// use pares_agens_sync::{SyncEngine, SyncTopic};
///
/// let mut engine = SyncEngine::new("Alice's MacBook");
/// engine.subscribe_topic(SyncTopic::MemoryEntries);
///
/// // Local change while offline
/// engine.enqueue_change(SyncTopic::MemoryEntries, serde_json::json!({"id": "m1"}));
///
/// // Simulate peer reconnect — drain and send
/// let pending = engine.drain_queue();
/// assert_eq!(pending.len(), 1);
/// ```
#[derive(Debug)]
pub struct SyncEngine {
    /// Human-readable name of the local device.
    pub device_name: String,

    topics: TopicManager,
    devices: DeviceRegistry,
    queue: OfflineQueue,
    conflict: ConflictResolution,
}

impl SyncEngine {
    /// Create a new `SyncEngine` for the local device identified by
    /// `device_name`.
    ///
    /// Uses the default [`ConflictResolution`] (CRDT last-write-wins).
    #[must_use]
    pub fn new(device_name: impl Into<String>) -> Self {
        Self {
            device_name: device_name.into(),
            topics: TopicManager::new(),
            devices: DeviceRegistry::new(),
            queue: OfflineQueue::new(),
            conflict: ConflictResolution::default_crdt(),
        }
    }

    // ── Topic management ──────────────────────────────────────────────────────

    /// Subscribe to a sync topic.
    ///
    /// Returns `true` when the subscription was newly created; `false` when
    /// the topic was already subscribed.
    pub fn subscribe_topic(&mut self, topic: SyncTopic) -> bool {
        let was_subscribed = self.topics.is_subscribed(topic);
        self.topics.subscribe(topic);
        !was_subscribed
    }

    /// Unsubscribe from a sync topic.  Returns `true` when the topic existed.
    pub fn unsubscribe_topic(&mut self, topic: SyncTopic) -> bool {
        self.topics.unsubscribe(topic)
    }

    /// Return `true` when `topic` is currently subscribed.
    #[must_use]
    pub fn is_subscribed(&self, topic: SyncTopic) -> bool {
        self.topics.is_subscribed(topic)
    }

    /// Return the number of currently active topic subscriptions.
    #[must_use]
    pub fn active_topic_count(&self) -> usize {
        self.topics.active_count()
    }

    /// Update the peer count reported by the Hyperswarm layer for a topic.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::TopicNotSubscribed`] when the topic is not active.
    pub fn set_topic_peer_count(
        &mut self,
        topic: SyncTopic,
        count: usize,
    ) -> Result<(), SyncError> {
        self.topics.set_peer_count(topic, count)
    }

    // ── Offline queue ─────────────────────────────────────────────────────────

    /// Enqueue a local change event for deferred delivery to peers.
    ///
    /// The payload should be a minimal diff, not a full state snapshot, to
    /// keep bandwidth usage low.
    pub fn enqueue_change(&mut self, topic: SyncTopic, payload: Value) {
        self.queue.enqueue(topic, payload);
    }

    /// Drain all queued change events for delivery to peers.
    ///
    /// The queue is empty after this call.  Callers are expected to transmit
    /// the returned events over Hyperswarm and re-enqueue on failure.
    pub fn drain_queue(&mut self) -> Vec<ChangeEvent> {
        self.queue.drain()
    }

    /// Drain queued events for a specific topic only.
    pub fn drain_queue_for_topic(&mut self, topic: SyncTopic) -> Vec<ChangeEvent> {
        self.queue.drain_topic(topic)
    }

    /// Return the number of events currently in the offline queue.
    #[must_use]
    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    // ── Device registry ───────────────────────────────────────────────────────

    /// Register a newly paired device and return its assigned ID.
    ///
    /// # Errors
    ///
    /// Propagates [`SyncError::InvalidPairingMaterial`] from
    /// [`PairedDevice::new`].
    pub fn register_device(&mut self, name: impl Into<String>) -> Result<String, SyncError> {
        let device = PairedDevice::new(name)?;
        Ok(self.devices.register(device))
    }

    /// Update the connection status of a paired device.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::UnknownDevice`] when no device with `id` exists.
    pub fn set_device_status(
        &mut self,
        device_id: &str,
        status: DeviceStatus,
    ) -> Result<(), SyncError> {
        let device = self.devices.get_mut(device_id)?;
        device.set_status(status);
        Ok(())
    }

    /// Remove a paired device from the registry.  Returns `true` if it existed.
    pub fn remove_device(&mut self, device_id: &str) -> bool {
        self.devices.remove(device_id)
    }

    /// Return a snapshot of all paired devices as owned values.
    #[must_use]
    pub fn list_devices(&self) -> Vec<PairedDevice> {
        self.devices.list().cloned().collect()
    }

    /// Return the number of paired devices.
    #[must_use]
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    // ── Pairing flow ──────────────────────────────────────────────────────────

    /// Initiate a new pairing session from this device.
    ///
    /// Returns the session so the caller can display the pairing code and
    /// sync key to the user.
    ///
    /// # Errors
    ///
    /// Propagates [`SyncError::InvalidPairingMaterial`] from
    /// [`PairingSession::new`].
    pub fn begin_pairing(&self) -> Result<PairingSession, SyncError> {
        PairingSession::new(&self.device_name)
    }

    // ── Conflict resolution ───────────────────────────────────────────────────

    /// Apply an incoming remote change payload, resolving any conflict with
    /// the given local value.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::TopicNotSubscribed`] when `topic` is not active.
    /// Propagates [`SyncError::ConflictResolution`] on merge failure.
    pub fn apply_remote_change(
        &self,
        topic: SyncTopic,
        local: &Value,
        remote: &Value,
    ) -> Result<Value, SyncError> {
        if !self.topics.is_subscribed(topic) {
            return Err(SyncError::TopicNotSubscribed(topic.as_key().to_string()));
        }
        self.conflict.resolve(local, remote)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn engine() -> SyncEngine {
        SyncEngine::new("test-device")
    }

    #[test]
    fn subscribe_topic_returns_true_on_new_subscription() {
        let mut e = engine();
        assert!(e.subscribe_topic(SyncTopic::MemoryEntries));
    }

    #[test]
    fn subscribe_topic_returns_false_when_already_subscribed() {
        let mut e = engine();
        e.subscribe_topic(SyncTopic::MemoryEntries);
        assert!(!e.subscribe_topic(SyncTopic::MemoryEntries));
    }

    #[test]
    fn unsubscribe_topic_returns_true_when_subscribed() {
        let mut e = engine();
        e.subscribe_topic(SyncTopic::AgentConfig);
        assert!(e.unsubscribe_topic(SyncTopic::AgentConfig));
        assert!(!e.is_subscribed(SyncTopic::AgentConfig));
    }

    #[test]
    fn active_topic_count_reflects_subscriptions() {
        let mut e = engine();
        e.subscribe_topic(SyncTopic::MemoryEntries);
        e.subscribe_topic(SyncTopic::Procedures);
        assert_eq!(e.active_topic_count(), 2);
        e.unsubscribe_topic(SyncTopic::Procedures);
        assert_eq!(e.active_topic_count(), 1);
    }

    #[test]
    fn enqueue_and_drain_queue_round_trip() {
        let mut e = engine();
        e.enqueue_change(SyncTopic::MemoryEntries, json!({"id": "m1"}));
        e.enqueue_change(SyncTopic::MemoryEntries, json!({"id": "m2"}));
        assert_eq!(e.queued_count(), 2);
        let events = e.drain_queue();
        assert_eq!(events.len(), 2);
        assert_eq!(e.queued_count(), 0);
    }

    #[test]
    fn drain_queue_for_topic_only_removes_matching_events() {
        let mut e = engine();
        e.enqueue_change(SyncTopic::MemoryEntries, json!({}));
        e.enqueue_change(SyncTopic::AgentConfig, json!({}));
        let drained = e.drain_queue_for_topic(SyncTopic::MemoryEntries);
        assert_eq!(drained.len(), 1);
        assert_eq!(e.queued_count(), 1);
    }

    #[test]
    fn register_device_and_list_devices() {
        let mut e = engine();
        let id = e.register_device("laptop").unwrap();
        let devices = e.list_devices();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, id);
    }

    #[test]
    fn register_device_rejects_empty_name() {
        let mut e = engine();
        assert!(matches!(
            e.register_device(""),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn set_device_status_updates_device() {
        let mut e = engine();
        let id = e.register_device("phone").unwrap();
        e.set_device_status(&id, DeviceStatus::Active).unwrap();
        let devices = e.list_devices();
        assert_eq!(devices[0].status, DeviceStatus::Active);
    }

    #[test]
    fn set_device_status_unknown_device_returns_error() {
        let mut e = engine();
        assert!(matches!(
            e.set_device_status("no-such-id", DeviceStatus::Active),
            Err(SyncError::UnknownDevice(_))
        ));
    }

    #[test]
    fn remove_device_decrements_count() {
        let mut e = engine();
        let id = e.register_device("tablet").unwrap();
        assert!(e.remove_device(&id));
        assert_eq!(e.device_count(), 0);
    }

    #[test]
    fn begin_pairing_creates_pending_session() {
        let e = engine();
        let session = e.begin_pairing().unwrap();
        assert_eq!(session.initiator_name, "test-device");
        assert_eq!(session.state, crate::pairing::ApprovalState::Pending);
    }

    #[test]
    fn apply_remote_change_errors_when_topic_not_subscribed() {
        let e = engine();
        let local = json!({"v": 1});
        let remote = json!({"v": 2});
        assert!(matches!(
            e.apply_remote_change(SyncTopic::MemoryEntries, &local, &remote),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn apply_remote_change_resolves_conflict_when_subscribed() {
        let mut e = engine();
        e.subscribe_topic(SyncTopic::MemoryEntries);
        let local = json!({"updated_at": "2024-01-01T00:00:00Z", "v": "old"});
        let remote = json!({"updated_at": "2024-01-02T00:00:00Z", "v": "new"});
        let result = e
            .apply_remote_change(SyncTopic::MemoryEntries, &local, &remote)
            .unwrap();
        assert_eq!(result["v"], "new");
    }

    #[test]
    fn set_topic_peer_count_errors_when_not_subscribed() {
        let mut e = engine();
        assert!(matches!(
            e.set_topic_peer_count(SyncTopic::Procedures, 5),
            Err(SyncError::TopicNotSubscribed(_))
        ));
    }

    #[test]
    fn set_topic_peer_count_updates_when_subscribed() {
        let mut e = engine();
        e.subscribe_topic(SyncTopic::Procedures);
        e.set_topic_peer_count(SyncTopic::Procedures, 7).unwrap();
        let sub = e.topics.get(SyncTopic::Procedures).unwrap();
        assert_eq!(sub.peer_count, 7);
    }
}
