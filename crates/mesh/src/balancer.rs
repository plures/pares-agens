//! Load balancing and fault tolerance.
//!
//! When the preferred device is overloaded or goes offline,
//! [`LoadBalancer`] automatically re-routes to the next available candidate
//! and tracks failed devices in a circuit-breaker set so they are skipped on
//! subsequent requests.

use crate::router::{MeshRouter, RouteDecision, RouteRequest};
use crate::MeshError;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

// ── LoadBalancer ──────────────────────────────────────────────────────────────

/// Wraps a [`MeshRouter`] with automatic retry-and-spill logic.
///
/// When a routing decision targets a device that has been marked unavailable,
/// the balancer re-routes to the next best candidate and sets `is_fallback =
/// true` on the resulting [`RouteDecision`].
#[derive(Debug)]
pub struct LoadBalancer {
    router: MeshRouter,
    /// Arc-shared set of device IDs that are currently circuit-broken.
    unavailable: Arc<Mutex<HashSet<String>>>,
}

impl LoadBalancer {
    /// Create a load balancer over the given router.
    pub fn new(router: MeshRouter) -> Self {
        Self {
            router,
            unavailable: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Mark a device as unavailable (e.g. after an inference failure).
    ///
    /// Subsequent calls to [`LoadBalancer::route`] will skip this device.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::LockPoisoned`] if the internal mutex was poisoned.
    pub fn mark_unavailable(&self, device_id: &str) -> Result<(), MeshError> {
        let mut guard = self
            .unavailable
            .lock()
            .map_err(|_| MeshError::LockPoisoned)?;
        guard.insert(device_id.to_string());
        Ok(())
    }

    /// Clear the unavailable set (e.g. after periodic reconnect probes succeed).
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::LockPoisoned`] if the internal mutex was poisoned.
    pub fn clear_unavailable(&self) -> Result<(), MeshError> {
        let mut guard = self
            .unavailable
            .lock()
            .map_err(|_| MeshError::LockPoisoned)?;
        guard.clear();
        Ok(())
    }

    /// Route a request, automatically skipping any unavailable devices.
    ///
    /// The balancer fetches all candidates for the requested expert, removes
    /// those in the unavailable set, and delegates to the router's core
    /// selection logic.  If all known-good candidates are exhausted it falls
    /// back to any remaining candidate (including unavailable ones as a last
    /// resort), marking `is_fallback = true`.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::NoDeviceForExpert`] when no device hosts the
    /// expert at all.
    pub fn route(&self, req: &RouteRequest) -> Result<RouteDecision, MeshError> {
        let unavailable_snapshot = {
            let guard = self
                .unavailable
                .lock()
                .map_err(|_| MeshError::LockPoisoned)?;
            guard.clone()
        };

        let all_candidates = self.router.registry.locate(&req.expert_id)?;
        if all_candidates.is_empty() {
            return Err(MeshError::NoDeviceForExpert(req.expert_id.clone()));
        }

        // Filter out unavailable devices.
        let healthy: Vec<_> = all_candidates
            .iter()
            .filter(|c| !unavailable_snapshot.contains(&c.device_id))
            .cloned()
            .collect();

        // If healthy candidates exist, route among them.
        if !healthy.is_empty() {
            return self.router.route_from_candidates(healthy, req);
        }

        // All known devices are unavailable — fall back to the full list with
        // is_fallback forced to true.
        let mut decision = self
            .router
            .route_from_candidates(all_candidates, req)?;
        decision.is_fallback = true;
        Ok(decision)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{ComputeClass, DeviceCapabilities, ExpertSpec};
    use crate::registry::ExpertRegistry;
    use crate::router::{MeshRouter, RequestUrgency, RouteRequest};
    use std::collections::HashMap;

    fn device(id: &str, expert: &str, load: f32) -> DeviceCapabilities {
        DeviceCapabilities {
            device_id: id.into(),
            display_name: id.into(),
            compute_class: ComputeClass::Gpu,
            memory_mb: 16_384,
            loaded_experts: vec![ExpertSpec {
                expert_id: expert.into(),
                model_name: expert.into(),
                params_billions: 8.0,
            }],
            load_factor: load,
            latency_ms: Some(10),
            metadata: HashMap::new(),
        }
    }

    fn req(expert: &str) -> RouteRequest {
        RouteRequest {
            expert_id: expert.into(),
            urgency: RequestUrgency::Interactive,
            max_latency_ms: None,
        }
    }

    fn balancer_with(devices: Vec<DeviceCapabilities>) -> LoadBalancer {
        let registry = ExpertRegistry::new();
        for d in devices {
            registry.upsert(d).unwrap();
        }
        LoadBalancer::new(MeshRouter::new(registry))
    }

    #[test]
    fn routes_normally_when_no_unavailable_devices() {
        let lb = balancer_with(vec![device("d1", "chat-8b", 0.3)]);
        let decision = lb.route(&req("chat-8b")).unwrap();
        assert_eq!(decision.device_id, "d1");
        assert!(!decision.is_fallback);
    }

    #[test]
    fn skips_marked_unavailable_device() {
        let lb = balancer_with(vec![
            device("d1", "chat-8b", 0.3),
            device("d2", "chat-8b", 0.4),
        ]);
        lb.mark_unavailable("d1").unwrap();
        let decision = lb.route(&req("chat-8b")).unwrap();
        assert_eq!(decision.device_id, "d2");
        assert!(!decision.is_fallback);
    }

    #[test]
    fn falls_back_when_all_devices_unavailable() {
        let lb = balancer_with(vec![device("d1", "chat-8b", 0.3)]);
        lb.mark_unavailable("d1").unwrap();
        let decision = lb.route(&req("chat-8b")).unwrap();
        // Only d1 exists; still routes to it with is_fallback = true.
        assert_eq!(decision.device_id, "d1");
        assert!(decision.is_fallback);
    }

    #[test]
    fn clear_unavailable_restores_routing() {
        let lb = balancer_with(vec![
            device("d1", "chat-8b", 0.3),
            device("d2", "chat-8b", 0.4),
        ]);
        lb.mark_unavailable("d1").unwrap();
        lb.clear_unavailable().unwrap();
        // Both devices available again; d1 and d2 both qualify.
        let decision = lb.route(&req("chat-8b")).unwrap();
        assert!(!decision.is_fallback);
    }

    #[test]
    fn errors_when_no_device_hosts_expert() {
        let lb = balancer_with(vec![]);
        let err = lb.route(&req("missing")).unwrap_err();
        assert!(matches!(err, MeshError::NoDeviceForExpert(_)));
    }
}
