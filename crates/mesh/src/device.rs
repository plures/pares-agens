//! Device capability advertisement.
//!
//! Each node in the neural mesh advertises its compute capabilities,
//! loaded experts, and current load so the cerebellum can route queries
//! to the optimal device.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── ComputeClass ──────────────────────────────────────────────────────────────

/// Class of compute hardware available on a device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ComputeClass {
    /// NVIDIA/AMD discrete GPU.
    Gpu,
    /// Apple Silicon (M-series) integrated Neural Engine / GPU.
    Npu,
    /// CPU-only execution.
    Cpu,
}

// ── ExpertSpec ────────────────────────────────────────────────────────────────

/// A single expert (model) loaded and ready on a device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertSpec {
    /// Unique expert identifier (e.g. `"code-30b"`, `"chat-8b"`).
    pub expert_id: String,
    /// Human-readable model name (e.g. `"Llama-3-30B-Code"`).
    pub model_name: String,
    /// Approximate parameter count in billions.
    pub params_billions: f32,
}

// ── DeviceCapabilities ────────────────────────────────────────────────────────

/// Snapshot of a device's capabilities and current state.
///
/// Devices broadcast this advertisement on join and whenever utilisation
/// changes by more than a configurable threshold so the rest of the mesh can
/// make accurate routing decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Stable unique identifier for this device (UUID-v4 assigned at first boot).
    pub device_id: String,
    /// Human-readable display name (e.g. `"Desktop RTX 4070"`).
    pub display_name: String,
    /// Primary compute class of this device.
    pub compute_class: ComputeClass,
    /// Available VRAM (GPU/NPU) or RAM (CPU) in megabytes.
    pub memory_mb: u64,
    /// Experts currently loaded and ready for inference.
    pub loaded_experts: Vec<ExpertSpec>,
    /// Current utilisation averaged over the last 10 seconds (0.0–1.0).
    pub load_factor: f32,
    /// Mesh-local round-trip latency in milliseconds measured from the
    /// advertising node.  `None` when not yet measured.
    pub latency_ms: Option<u32>,
    /// Arbitrary key-value metadata (e.g. OS, driver version, Pares version).
    pub metadata: HashMap<String, String>,
}

impl DeviceCapabilities {
    /// Return `true` when the device has spare capacity for an additional
    /// inference request (load < 90 %).
    #[must_use]
    pub fn has_capacity(&self) -> bool {
        self.load_factor < 0.9
    }

    /// Return the total parameter count (in billions) across all loaded experts.
    #[must_use]
    pub fn total_params_billions(&self) -> f32 {
        self.loaded_experts.iter().map(|e| e.params_billions).sum()
    }

    /// Return `true` when this device currently hosts the given expert.
    #[must_use]
    pub fn has_expert(&self, expert_id: &str) -> bool {
        self.loaded_experts.iter().any(|e| e.expert_id == expert_id)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_device(load: f32, experts: Vec<(&str, f32)>) -> DeviceCapabilities {
        DeviceCapabilities {
            device_id: "dev-1".into(),
            display_name: "Test Device".into(),
            compute_class: ComputeClass::Gpu,
            memory_mb: 16_384,
            loaded_experts: experts
                .into_iter()
                .map(|(id, params)| ExpertSpec {
                    expert_id: id.into(),
                    model_name: id.into(),
                    params_billions: params,
                })
                .collect(),
            load_factor: load,
            latency_ms: Some(5),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn has_capacity_returns_true_when_load_below_threshold() {
        let dev = make_device(0.5, vec![]);
        assert!(dev.has_capacity());
    }

    #[test]
    fn has_capacity_returns_false_when_load_at_or_above_threshold() {
        let dev = make_device(0.9, vec![]);
        assert!(!dev.has_capacity());

        let dev2 = make_device(1.0, vec![]);
        assert!(!dev2.has_capacity());
    }

    #[test]
    fn total_params_billions_sums_all_experts() {
        let dev = make_device(0.0, vec![("code-30b", 30.0), ("chat-8b", 8.0)]);
        assert!((dev.total_params_billions() - 38.0).abs() < 1e-3);
    }

    #[test]
    fn total_params_billions_is_zero_when_no_experts() {
        let dev = make_device(0.0, vec![]);
        assert_eq!(dev.total_params_billions(), 0.0);
    }

    #[test]
    fn has_expert_returns_true_when_expert_is_loaded() {
        let dev = make_device(0.0, vec![("code-30b", 30.0)]);
        assert!(dev.has_expert("code-30b"));
    }

    #[test]
    fn has_expert_returns_false_when_expert_is_not_loaded() {
        let dev = make_device(0.0, vec![("code-30b", 30.0)]);
        assert!(!dev.has_expert("math-8b"));
    }

    #[test]
    fn device_capabilities_roundtrips_serde() {
        let dev = make_device(0.3, vec![("chat-8b", 8.0)]);
        let json = serde_json::to_string(&dev).unwrap();
        let back: DeviceCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back.device_id, dev.device_id);
        assert_eq!(back.load_factor, dev.load_factor);
    }
}
