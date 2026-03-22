//! Mesh dashboard: aggregated view of all devices, loaded experts,
//! utilisation, and round-trip latency.
//!
//! [`MeshDashboard`] reads from the shared [`ExpertRegistry`] and produces a
//! [`MeshStats`] snapshot suitable for rendering in a UI.

use crate::device::ComputeClass;
use crate::registry::ExpertRegistry;
use crate::MeshError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

// ── DeviceSummary ─────────────────────────────────────────────────────────────

/// Aggregated statistics for a single device, suitable for display in the
/// mesh dashboard UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSummary {
    /// Stable device identifier.
    pub device_id: String,
    /// Human-readable label shown in the UI.
    pub display_name: String,
    /// Primary compute class.
    pub compute_class: ComputeClass,
    /// Available memory in megabytes.
    pub memory_mb: u64,
    /// Current utilisation (0.0–1.0).
    pub load_factor: f32,
    /// Number of experts currently loaded.
    pub loaded_expert_count: usize,
    /// Total parameter count across all loaded experts (billions).
    pub total_params_billions: f32,
    /// Estimated round-trip latency from the local node (ms).
    pub latency_ms: Option<u32>,
}

// ── MeshStats ─────────────────────────────────────────────────────────────────

/// Aggregated mesh-wide statistics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStats {
    /// Per-device summaries.
    pub devices: Vec<DeviceSummary>,
    /// Total parameter count across the whole mesh (billions).
    pub total_params_billions: f32,
    /// Average utilisation across all devices (0.0–1.0).
    pub average_load: f32,
    /// Number of unique expert IDs available somewhere in the mesh.
    pub unique_expert_count: usize,
}

// ── MeshDashboard ─────────────────────────────────────────────────────────────

/// Builds [`MeshStats`] snapshots from the live [`ExpertRegistry`].
///
/// Intended to be polled periodically by the UI layer (e.g. every second) to
/// refresh the dashboard view.
#[derive(Debug)]
pub struct MeshDashboard {
    registry: ExpertRegistry,
}

impl MeshDashboard {
    /// Create a dashboard backed by the given [`ExpertRegistry`].
    pub fn new(registry: ExpertRegistry) -> Self {
        Self { registry }
    }

    /// Compute a fresh [`MeshStats`] snapshot from the current registry state.
    ///
    /// # Errors
    ///
    /// Propagates registry lock errors as [`MeshError`].
    pub fn snapshot(&self) -> Result<MeshStats, MeshError> {
        let devices = self.registry.all_devices()?;

        let mut unique_experts: HashSet<String> = HashSet::new();
        let summaries: Vec<DeviceSummary> = devices
            .iter()
            .map(|d| {
                for e in &d.loaded_experts {
                    unique_experts.insert(e.expert_id.clone());
                }
                DeviceSummary {
                    device_id: d.device_id.clone(),
                    display_name: d.display_name.clone(),
                    compute_class: d.compute_class.clone(),
                    memory_mb: d.memory_mb,
                    load_factor: d.load_factor,
                    loaded_expert_count: d.loaded_experts.len(),
                    total_params_billions: d.total_params_billions(),
                    latency_ms: d.latency_ms,
                }
            })
            .collect();

        let total_params: f32 = summaries.iter().map(|s| s.total_params_billions).sum();
        let average_load = if summaries.is_empty() {
            0.0
        } else {
            summaries.iter().map(|s| s.load_factor).sum::<f32>() / summaries.len() as f32
        };

        Ok(MeshStats {
            devices: summaries,
            total_params_billions: total_params,
            average_load,
            unique_expert_count: unique_experts.len(),
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ComputeClass, DeviceCapabilities, ExpertSpec};
    use crate::registry::ExpertRegistry;
    use std::collections::HashMap;

    fn make_device(id: &str, experts: Vec<(&str, f32)>, load: f32) -> DeviceCapabilities {
        DeviceCapabilities {
            device_id: id.into(),
            display_name: id.into(),
            compute_class: ComputeClass::Gpu,
            memory_mb: 24_576,
            loaded_experts: experts
                .into_iter()
                .map(|(eid, params)| ExpertSpec {
                    expert_id: eid.into(),
                    model_name: eid.into(),
                    params_billions: params,
                })
                .collect(),
            load_factor: load,
            latency_ms: Some(10),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn snapshot_is_empty_when_registry_has_no_devices() {
        let dashboard = MeshDashboard::new(ExpertRegistry::new());
        let stats = dashboard.snapshot().unwrap();
        assert!(stats.devices.is_empty());
        assert_eq!(stats.total_params_billions, 0.0);
        assert_eq!(stats.average_load, 0.0);
        assert_eq!(stats.unique_expert_count, 0);
    }

    #[test]
    fn snapshot_totals_params_across_devices() {
        let registry = ExpertRegistry::new();
        registry
            .upsert(make_device(
                "desktop",
                vec![("code-30b", 30.0), ("math-8b", 8.0)],
                0.5,
            ))
            .unwrap();
        registry
            .upsert(make_device("laptop", vec![("chat-8b", 8.0)], 0.3))
            .unwrap();

        let stats = MeshDashboard::new(registry).snapshot().unwrap();
        assert!((stats.total_params_billions - 46.0).abs() < 1e-3);
    }

    #[test]
    fn snapshot_counts_unique_experts() {
        let registry = ExpertRegistry::new();
        // Two devices both host chat-8b (shared expert).
        registry
            .upsert(make_device("d1", vec![("chat-8b", 8.0)], 0.3))
            .unwrap();
        registry
            .upsert(make_device("d2", vec![("chat-8b", 8.0), ("code-30b", 30.0)], 0.5))
            .unwrap();

        let stats = MeshDashboard::new(registry).snapshot().unwrap();
        // chat-8b and code-30b = 2 unique experts.
        assert_eq!(stats.unique_expert_count, 2);
    }

    #[test]
    fn snapshot_computes_average_load() {
        let registry = ExpertRegistry::new();
        registry
            .upsert(make_device("d1", vec![], 0.2))
            .unwrap();
        registry
            .upsert(make_device("d2", vec![], 0.6))
            .unwrap();

        let stats = MeshDashboard::new(registry).snapshot().unwrap();
        assert!((stats.average_load - 0.4).abs() < 1e-3);
    }

    #[test]
    fn snapshot_includes_all_device_summaries() {
        let registry = ExpertRegistry::new();
        registry
            .upsert(make_device("desktop", vec![("code-30b", 30.0)], 0.7))
            .unwrap();
        registry
            .upsert(make_device("laptop", vec![("chat-8b", 8.0)], 0.2))
            .unwrap();

        let stats = MeshDashboard::new(registry).snapshot().unwrap();
        assert_eq!(stats.devices.len(), 2);
    }

    #[test]
    fn mesh_stats_roundtrips_serde() {
        let registry = ExpertRegistry::new();
        registry
            .upsert(make_device("d1", vec![("chat-8b", 8.0)], 0.4))
            .unwrap();
        let stats = MeshDashboard::new(registry).snapshot().unwrap();
        let json = serde_json::to_string(&stats).unwrap();
        let back: MeshStats = serde_json::from_str(&json).unwrap();
        assert_eq!(back.devices.len(), 1);
    }
}
