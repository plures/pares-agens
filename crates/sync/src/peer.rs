//! Peer and device registry — tracks paired devices and their per-topic sync state.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{SyncError, SyncTopic};

// ── DeviceStatus ──────────────────────────────────────────────────────────────

/// Connection and activity status of a paired peer device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatus {
    /// The device is connected and actively exchanging sync messages.
    Active,
    /// The device was previously connected but has since gone offline.
    Disconnected,
    /// A sync operation is currently in progress with this device.
    Syncing,
}

// ── TopicSyncState ────────────────────────────────────────────────────────────

/// Per-topic synchronisation state for a single paired device.
///
/// Tracks the sequence number of the last successfully synced entry and the
/// number of locally-queued changes that are still pending delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicSyncState {
    /// The Hyperswarm topic key this state belongs to.
    pub topic: SyncTopic,

    /// Logical sequence number of the most recently synced entry.
    /// `None` means no sync has occurred yet for this topic.
    pub last_seq: Option<u64>,

    /// Number of local changes queued and not yet delivered to this peer.
    pub pending_count: usize,

    /// UTC timestamp of the last successful sync for this topic.
    pub last_synced_at: Option<DateTime<Utc>>,
}

impl TopicSyncState {
    /// Create a fresh (never-synced) state for the given topic.
    #[must_use]
    pub fn new(topic: SyncTopic) -> Self {
        Self {
            topic,
            last_seq: None,
            pending_count: 0,
            last_synced_at: None,
        }
    }

    /// Record a successful sync up to `seq`, clearing pending count.
    pub fn mark_synced(&mut self, seq: u64) {
        self.last_seq = Some(seq);
        self.pending_count = 0;
        self.last_synced_at = Some(Utc::now());
    }

    /// Increment the pending change counter.
    pub fn increment_pending(&mut self) {
        self.pending_count += 1;
    }
}

// ── PairedDevice ──────────────────────────────────────────────────────────────

/// A remote device that has been paired with the local instance.
///
/// Stores connection metadata and per-topic sync state so the settings UI can
/// display a live device list with granular sync status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairedDevice {
    /// Unique device identifier (UUID v4 string).
    pub id: String,

    /// Human-readable device name chosen during pairing.
    pub name: String,

    /// Current connection status.
    pub status: DeviceStatus,

    /// UTC timestamp when the device was first paired.
    pub paired_at: DateTime<Utc>,

    /// UTC timestamp of the most recent connection event.
    pub last_seen_at: Option<DateTime<Utc>>,

    /// Per-topic synchronisation state, keyed by topic.
    pub topic_states: HashMap<String, TopicSyncState>,
}

impl PairedDevice {
    /// Create a new `PairedDevice` with `Disconnected` status and no sync history.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::InvalidPairingMaterial`] when `name` is empty.
    pub fn new(name: impl Into<String>) -> Result<Self, SyncError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(SyncError::InvalidPairingMaterial(
                "device name must not be empty".into(),
            ));
        }
        Ok(Self {
            id: Uuid::new_v4().to_string(),
            name,
            status: DeviceStatus::Disconnected,
            paired_at: Utc::now(),
            last_seen_at: None,
            topic_states: HashMap::new(),
        })
    }

    /// Update the device status and set `last_seen_at` to now when the device
    /// transitions to `Active` or `Syncing`.
    pub fn set_status(&mut self, status: DeviceStatus) {
        if matches!(status, DeviceStatus::Active | DeviceStatus::Syncing) {
            self.last_seen_at = Some(Utc::now());
        }
        self.status = status;
    }

    /// Return the [`TopicSyncState`] for `topic`, creating a default entry if
    /// none exists yet.
    pub fn topic_state_mut(&mut self, topic: SyncTopic) -> &mut TopicSyncState {
        self.topic_states
            .entry(topic.as_key().to_owned())
            .or_insert_with(|| TopicSyncState::new(topic))
    }

    /// Return the [`TopicSyncState`] for `topic` if it exists.
    #[must_use]
    pub fn topic_state(&self, topic: SyncTopic) -> Option<&TopicSyncState> {
        self.topic_states.get(topic.as_key())
    }
}

// ── DeviceRegistry ────────────────────────────────────────────────────────────

/// In-memory registry of all paired peer devices.
///
/// Provides CRUD access and lookup helpers for the settings UI device list.
#[derive(Debug, Default)]
pub struct DeviceRegistry {
    devices: HashMap<String, PairedDevice>,
}

impl DeviceRegistry {
    /// Create a new, empty `DeviceRegistry`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new paired device and return its assigned ID.
    pub fn register(&mut self, device: PairedDevice) -> String {
        let id = device.id.clone();
        self.devices.insert(id.clone(), device);
        id
    }

    /// Retrieve a device by ID.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::UnknownDevice`] when no device with `id` exists.
    pub fn get(&self, id: &str) -> Result<&PairedDevice, SyncError> {
        self.devices
            .get(id)
            .ok_or_else(|| SyncError::UnknownDevice(id.to_string()))
    }

    /// Retrieve a mutable reference to a device by ID.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::UnknownDevice`] when no device with `id` exists.
    pub fn get_mut(&mut self, id: &str) -> Result<&mut PairedDevice, SyncError> {
        self.devices
            .get_mut(id)
            .ok_or_else(|| SyncError::UnknownDevice(id.to_string()))
    }

    /// Remove a device from the registry.  Returns `true` if it existed.
    pub fn remove(&mut self, id: &str) -> bool {
        self.devices.remove(id).is_some()
    }

    /// Return an iterator over all registered devices.
    pub fn list(&self) -> impl Iterator<Item = &PairedDevice> {
        self.devices.values()
    }

    /// Return the total number of registered devices.
    #[must_use]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    /// Return `true` when no devices are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_device_new_rejects_empty_name() {
        assert!(matches!(
            PairedDevice::new(""),
            Err(SyncError::InvalidPairingMaterial(_))
        ));
    }

    #[test]
    fn paired_device_new_accepts_valid_name() {
        let device = PairedDevice::new("laptop").unwrap();
        assert_eq!(device.name, "laptop");
        assert_eq!(device.status, DeviceStatus::Disconnected);
        assert!(device.topic_states.is_empty());
    }

    #[test]
    fn set_status_to_active_updates_last_seen() {
        let mut device = PairedDevice::new("phone").unwrap();
        assert!(device.last_seen_at.is_none());
        device.set_status(DeviceStatus::Active);
        assert!(device.last_seen_at.is_some());
        assert_eq!(device.status, DeviceStatus::Active);
    }

    #[test]
    fn set_status_to_disconnected_does_not_update_last_seen() {
        let mut device = PairedDevice::new("tablet").unwrap();
        device.set_status(DeviceStatus::Disconnected);
        assert!(device.last_seen_at.is_none());
    }

    #[test]
    fn topic_state_mut_creates_default_on_first_access() {
        let mut device = PairedDevice::new("workstation").unwrap();
        let state = device.topic_state_mut(SyncTopic::MemoryEntries);
        assert!(state.last_seq.is_none());
        assert_eq!(state.pending_count, 0);
    }

    #[test]
    fn topic_state_mark_synced_updates_fields() {
        let mut device = PairedDevice::new("server").unwrap();
        let state = device.topic_state_mut(SyncTopic::Procedures);
        state.mark_synced(42);
        assert_eq!(state.last_seq, Some(42));
        assert_eq!(state.pending_count, 0);
        assert!(state.last_synced_at.is_some());
    }

    #[test]
    fn topic_state_increment_pending_increases_count() {
        let mut device = PairedDevice::new("server").unwrap();
        let state = device.topic_state_mut(SyncTopic::AgentConfig);
        state.increment_pending();
        state.increment_pending();
        assert_eq!(state.pending_count, 2);
    }

    #[test]
    fn registry_register_and_get_round_trip() {
        let mut registry = DeviceRegistry::new();
        let device = PairedDevice::new("laptop").unwrap();
        let id = registry.register(device);
        let retrieved = registry.get(&id).unwrap();
        assert_eq!(retrieved.name, "laptop");
    }

    #[test]
    fn registry_get_unknown_returns_error() {
        let registry = DeviceRegistry::new();
        assert!(matches!(
            registry.get("no-such-id"),
            Err(SyncError::UnknownDevice(_))
        ));
    }

    #[test]
    fn registry_remove_returns_true_for_existing() {
        let mut registry = DeviceRegistry::new();
        let device = PairedDevice::new("old-phone").unwrap();
        let id = registry.register(device);
        assert!(registry.remove(&id));
        assert!(registry.is_empty());
    }

    #[test]
    fn registry_list_returns_all_devices() {
        let mut registry = DeviceRegistry::new();
        registry.register(PairedDevice::new("a").unwrap());
        registry.register(PairedDevice::new("b").unwrap());
        assert_eq!(registry.list().count(), 2);
        assert_eq!(registry.len(), 2);
    }
}
