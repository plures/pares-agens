//! Distributed expert registry.
//!
//! Maintains a mesh-wide view of all available experts and which devices host
//! them.  Entries are upserted whenever a device broadcasts a
//! [`DeviceCapabilities`] advertisement and removed when a device goes offline.

use crate::device::{DeviceCapabilities, ExpertSpec};
use crate::MeshError;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

// ── LocatedExpert ─────────────────────────────────────────────────────────────

/// An expert that is hosted on a specific mesh device.
#[derive(Debug, Clone)]
pub struct LocatedExpert {
    /// The device hosting this expert.
    pub device_id: String,
    /// Expert specification.
    pub expert: ExpertSpec,
    /// Current load factor of the hosting device (0.0–1.0).
    pub device_load: f32,
    /// Estimated round-trip latency to the device in milliseconds.
    pub latency_ms: Option<u32>,
}

// ── Registry internals ────────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct RegistryInner {
    devices: HashMap<String, DeviceCapabilities>,
}

// ── ExpertRegistry ────────────────────────────────────────────────────────────

/// Thread-safe, mesh-wide expert registry.
///
/// Devices register themselves via [`ExpertRegistry::upsert`].  The router
/// queries available experts via [`ExpertRegistry::locate`].  Because the inner
/// state is wrapped in an `Arc<RwLock<…>>`, cloning an `ExpertRegistry` gives
/// a second handle to the **same** shared state — useful for handing both the
/// router and the load balancer access without an explicit dependency inversion.
#[derive(Debug, Default, Clone)]
pub struct ExpertRegistry {
    inner: Arc<RwLock<RegistryInner>>,
}

impl ExpertRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace the capability advertisement for a device.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::RegistryLockPoisoned`] if the internal lock was
    /// poisoned by a panicking writer.
    pub fn upsert(&self, caps: DeviceCapabilities) -> Result<(), MeshError> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| MeshError::RegistryLockPoisoned)?;
        guard.devices.insert(caps.device_id.clone(), caps);
        Ok(())
    }

    /// Remove a device from the registry (e.g. when it goes offline).
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::RegistryLockPoisoned`] if the internal lock was
    /// poisoned.
    pub fn remove(&self, device_id: &str) -> Result<(), MeshError> {
        let mut guard = self
            .inner
            .write()
            .map_err(|_| MeshError::RegistryLockPoisoned)?;
        guard.devices.remove(device_id);
        Ok(())
    }

    /// Return all registered device capability snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::RegistryLockPoisoned`] if the internal lock was
    /// poisoned.
    pub fn all_devices(&self) -> Result<Vec<DeviceCapabilities>, MeshError> {
        let guard = self
            .inner
            .read()
            .map_err(|_| MeshError::RegistryLockPoisoned)?;
        Ok(guard.devices.values().cloned().collect())
    }

    /// Find all devices that host the given expert, ordered by ascending
    /// latency (None treated as `u32::MAX`) then ascending load.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::RegistryLockPoisoned`] if the internal lock was
    /// poisoned.
    pub fn locate(&self, expert_id: &str) -> Result<Vec<LocatedExpert>, MeshError> {
        let guard = self
            .inner
            .read()
            .map_err(|_| MeshError::RegistryLockPoisoned)?;

        let mut located: Vec<LocatedExpert> = guard
            .devices
            .values()
            .filter_map(|dev| {
                dev.loaded_experts
                    .iter()
                    .find(|e| e.expert_id == expert_id)
                    .map(|expert| LocatedExpert {
                        device_id: dev.device_id.clone(),
                        expert: expert.clone(),
                        device_load: dev.load_factor,
                        latency_ms: dev.latency_ms,
                    })
            })
            .collect();

        // Primary sort: latency ascending (None → very high).
        // Secondary sort: load ascending.
        located.sort_by(|a, b| {
            let lat_a = a.latency_ms.unwrap_or(u32::MAX);
            let lat_b = b.latency_ms.unwrap_or(u32::MAX);
            lat_a.cmp(&lat_b).then_with(|| {
                a.device_load
                    .partial_cmp(&b.device_load)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        Ok(located)
    }

    /// Return the number of registered devices.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::RegistryLockPoisoned`] if the internal lock was
    /// poisoned.
    pub fn device_count(&self) -> Result<usize, MeshError> {
        let guard = self
            .inner
            .read()
            .map_err(|_| MeshError::RegistryLockPoisoned)?;
        Ok(guard.devices.len())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ComputeClass, ExpertSpec};
    use std::collections::HashMap;

    fn dev(id: &str, expert_id: &str, load: f32, latency: Option<u32>) -> DeviceCapabilities {
        DeviceCapabilities {
            device_id: id.into(),
            display_name: id.into(),
            compute_class: ComputeClass::Gpu,
            memory_mb: 16_384,
            loaded_experts: vec![ExpertSpec {
                expert_id: expert_id.into(),
                model_name: expert_id.into(),
                params_billions: 8.0,
            }],
            load_factor: load,
            latency_ms: latency,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn upsert_and_locate_single_expert() {
        let registry = ExpertRegistry::new();
        registry.upsert(dev("d1", "chat-8b", 0.3, Some(10))).unwrap();
        let located = registry.locate("chat-8b").unwrap();
        assert_eq!(located.len(), 1);
        assert_eq!(located[0].device_id, "d1");
    }

    #[test]
    fn locate_returns_empty_when_no_device_has_expert() {
        let registry = ExpertRegistry::new();
        registry.upsert(dev("d1", "code-30b", 0.3, Some(10))).unwrap();
        let located = registry.locate("chat-8b").unwrap();
        assert!(located.is_empty());
    }

    #[test]
    fn remove_device_clears_its_experts() {
        let registry = ExpertRegistry::new();
        registry.upsert(dev("d1", "chat-8b", 0.3, Some(10))).unwrap();
        registry.remove("d1").unwrap();
        let located = registry.locate("chat-8b").unwrap();
        assert!(located.is_empty());
    }

    #[test]
    fn locate_orders_by_latency_then_load() {
        let registry = ExpertRegistry::new();
        // d1: high latency, low load
        registry.upsert(dev("d1", "chat-8b", 0.1, Some(100))).unwrap();
        // d2: low latency, high load
        registry.upsert(dev("d2", "chat-8b", 0.8, Some(5))).unwrap();
        // d3: medium latency, medium load
        registry.upsert(dev("d3", "chat-8b", 0.4, Some(50))).unwrap();

        let located = registry.locate("chat-8b").unwrap();
        assert_eq!(located.len(), 3);
        // d2 (5 ms) should be first despite high load
        assert_eq!(located[0].device_id, "d2");
        // d3 (50 ms) second
        assert_eq!(located[1].device_id, "d3");
        // d1 (100 ms) last
        assert_eq!(located[2].device_id, "d1");
    }

    #[test]
    fn device_count_reflects_upserts_and_removes() {
        let registry = ExpertRegistry::new();
        assert_eq!(registry.device_count().unwrap(), 0);
        registry.upsert(dev("d1", "chat-8b", 0.3, None)).unwrap();
        registry.upsert(dev("d2", "code-30b", 0.5, None)).unwrap();
        assert_eq!(registry.device_count().unwrap(), 2);
        registry.remove("d1").unwrap();
        assert_eq!(registry.device_count().unwrap(), 1);
    }

    #[test]
    fn upsert_replaces_existing_entry() {
        let registry = ExpertRegistry::new();
        registry.upsert(dev("d1", "chat-8b", 0.3, Some(10))).unwrap();
        // Update load factor by upserting with the same device_id and new value.
        let updated = dev("d1", "chat-8b", 0.9, Some(10));
        registry.upsert(updated).unwrap();
        let located = registry.locate("chat-8b").unwrap();
        assert_eq!(located.len(), 1);
        assert!((located[0].device_load - 0.9).abs() < 1e-3);
    }

    #[test]
    fn clone_shares_state() {
        let registry = ExpertRegistry::new();
        let clone = registry.clone();
        registry.upsert(dev("d1", "chat-8b", 0.3, None)).unwrap();
        // The clone should see the same update because they share the Arc.
        assert_eq!(clone.device_count().unwrap(), 1);
    }
}
