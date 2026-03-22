//! Auto-optimise expert placement across the mesh.
//!
//! Given the current mesh topology (device capabilities, loaded experts,
//! available memory), [`PlacementOptimizer`] suggests the optimal assignment
//! of experts to devices to maximise total throughput while respecting each
//! device's memory budget.
//!
//! ## Algorithm
//!
//! 1. Sort experts by parameter count descending (largest-first greedy
//!    assignment reduces fragmentation).
//! 2. For each expert, score candidate devices by remaining memory and compute
//!    class (GPU > NPU > CPU).
//! 3. Assign the expert to the highest-scoring device that still has room.
//! 4. Emit a [`PlacementSuggestion`] for every expert that would need to move
//!    from its current device to the recommended one.

use crate::device::{ComputeClass, DeviceCapabilities, ExpertSpec};
use crate::MeshError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── PlacementSuggestion ───────────────────────────────────────────────────────

/// A recommended move for a single expert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementSuggestion {
    /// The expert that should be moved.
    pub expert_id: String,
    /// Where the expert currently lives.  `None` if it is not yet loaded on
    /// any device.
    pub current_device_id: Option<String>,
    /// Device recommended to host this expert.
    pub recommended_device_id: String,
    /// Human-readable rationale for the suggestion.
    pub rationale: String,
}

// ── PlacementOptimizer ────────────────────────────────────────────────────────

/// Recommends expert-to-device assignments for the given mesh topology.
///
/// The optimizer is stateless — call [`PlacementOptimizer::suggest`] whenever
/// the mesh topology changes (device joins/leaves, expert loads/unloads) to
/// obtain a fresh set of recommendations.
#[derive(Debug, Default)]
pub struct PlacementOptimizer;

impl PlacementOptimizer {
    /// Suggest optimal expert placements given the current device capabilities.
    ///
    /// Returns only the suggestions that require an actual move or initial
    /// placement; experts that are already on their recommended device are
    /// omitted.
    ///
    /// # Arguments
    ///
    /// * `devices` — current snapshot of all registered devices.
    /// * `all_experts` — complete list of expert specs that should be placed.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::InsufficientMeshCapacity`] when there are no
    /// devices in the mesh.
    pub fn suggest(
        &self,
        devices: &[DeviceCapabilities],
        all_experts: &[ExpertSpec],
    ) -> Result<Vec<PlacementSuggestion>, MeshError> {
        if devices.is_empty() {
            return Err(MeshError::InsufficientMeshCapacity);
        }

        // Build a map from expert_id → current device_id.
        let mut current: HashMap<String, String> = HashMap::new();
        for dev in devices {
            for exp in &dev.loaded_experts {
                current.insert(exp.expert_id.clone(), dev.device_id.clone());
            }
        }

        // Sort experts by parameter count descending (biggest first).
        let mut sorted_experts: Vec<&ExpertSpec> = all_experts.iter().collect();
        sorted_experts.sort_by(|a, b| {
            b.params_billions
                .partial_cmp(&a.params_billions)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Remaining memory per device (MB).  We estimate model memory as
        // params_billions × 2_000 MB (rough 2-byte quantisation baseline).
        let mut remaining_mb: HashMap<String, u64> = devices
            .iter()
            .map(|d| (d.device_id.clone(), d.memory_mb))
            .collect();

        let mut suggestions = Vec::new();

        for expert in &sorted_experts {
            let required_mb = (expert.params_billions * 2_000.0) as u64;

            // Find the device with the most remaining capacity that can fit
            // this expert.  Ties broken by compute class (GPU > NPU > CPU).
            let best = devices
                .iter()
                .filter_map(|d| {
                    let rem = *remaining_mb.get(&d.device_id)?;
                    if rem >= required_mb {
                        Some((d, rem))
                    } else {
                        None
                    }
                })
                .max_by(|(da, ra), (db, rb)| {
                    ra.cmp(rb)
                        .then_with(|| compute_score(da).cmp(&compute_score(db)))
                });

            if let Some((device, _)) = best {
                *remaining_mb.entry(device.device_id.clone()).or_default() -= required_mb;

                let current_device = current.get(&expert.expert_id).cloned();
                let needs_move = current_device.as_deref() != Some(device.device_id.as_str());

                if needs_move {
                    suggestions.push(PlacementSuggestion {
                        expert_id: expert.expert_id.clone(),
                        current_device_id: current_device,
                        recommended_device_id: device.device_id.clone(),
                        rationale: format!(
                            "Place {} ({:.1}B params) on {} for optimal memory utilisation",
                            expert.expert_id, expert.params_billions, device.display_name,
                        ),
                    });
                }
            }
        }

        Ok(suggestions)
    }
}

/// Return a numeric score for the compute class so better hardware is
/// preferred in placement decisions.
fn compute_score(d: &DeviceCapabilities) -> u8 {
    match d.compute_class {
        ComputeClass::Gpu => 3,
        ComputeClass::Npu => 2,
        ComputeClass::Cpu => 1,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ComputeClass;
    use std::collections::HashMap;

    fn gpu_device(id: &str, memory_mb: u64, experts: Vec<(&str, f32)>) -> DeviceCapabilities {
        DeviceCapabilities {
            device_id: id.into(),
            display_name: id.into(),
            compute_class: ComputeClass::Gpu,
            memory_mb,
            loaded_experts: experts
                .into_iter()
                .map(|(eid, p)| ExpertSpec {
                    expert_id: eid.into(),
                    model_name: eid.into(),
                    params_billions: p,
                })
                .collect(),
            load_factor: 0.0,
            latency_ms: None,
            metadata: HashMap::new(),
        }
    }

    fn expert(id: &str, params: f32) -> ExpertSpec {
        ExpertSpec {
            expert_id: id.into(),
            model_name: id.into(),
            params_billions: params,
        }
    }

    #[test]
    fn suggest_errors_when_no_devices() {
        let opt = PlacementOptimizer;
        let err = opt.suggest(&[], &[expert("chat-8b", 8.0)]).unwrap_err();
        assert!(matches!(err, MeshError::InsufficientMeshCapacity));
    }

    #[test]
    fn suggest_returns_empty_when_already_optimal() {
        // Device already hosts the expert; no move needed.
        let devices = vec![gpu_device("d1", 20_000, vec![("chat-8b", 8.0)])];
        let experts = vec![expert("chat-8b", 8.0)];
        let opt = PlacementOptimizer;
        let suggestions = opt.suggest(&devices, &experts).unwrap();
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggest_recommends_device_with_enough_memory() {
        // d1 has 4 GB — too small for an 8B model (requires ~16 GB).
        // d2 has 24 GB — fits.
        let devices = vec![
            gpu_device("d1", 4_000, vec![]),
            gpu_device("d2", 24_000, vec![]),
        ];
        let experts = vec![expert("chat-8b", 8.0)];
        let opt = PlacementOptimizer;
        let suggestions = opt.suggest(&devices, &experts).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].recommended_device_id, "d2");
    }

    #[test]
    fn suggest_moves_expert_from_wrong_device() {
        // chat-8b is on d1 but d2 has more memory.
        let devices = vec![
            gpu_device("d1", 24_000, vec![("chat-8b", 8.0)]),
            gpu_device("d2", 48_000, vec![]),
        ];
        let experts = vec![expert("chat-8b", 8.0)];
        let opt = PlacementOptimizer;
        let suggestions = opt.suggest(&devices, &experts).unwrap();
        // d2 should be preferred (more remaining memory).
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].current_device_id, Some("d1".to_string()));
        assert_eq!(suggestions[0].recommended_device_id, "d2");
    }

    #[test]
    fn suggest_places_multiple_experts_within_budget() {
        // Single device with 80 GB — fits both a 30B and an 8B model.
        let devices = vec![gpu_device("big-gpu", 80_000, vec![])];
        let experts = vec![expert("code-30b", 30.0), expert("chat-8b", 8.0)];
        let opt = PlacementOptimizer;
        let suggestions = opt.suggest(&devices, &experts).unwrap();
        // Both need to be placed (neither is currently on big-gpu).
        assert_eq!(suggestions.len(), 2);
        for s in &suggestions {
            assert_eq!(s.recommended_device_id, "big-gpu");
        }
    }

    #[test]
    fn suggestion_roundtrips_serde() {
        let s = PlacementSuggestion {
            expert_id: "code-30b".into(),
            current_device_id: Some("desktop".into()),
            recommended_device_id: "laptop".into(),
            rationale: "test".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: PlacementSuggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(back.expert_id, s.expert_id);
    }
}
