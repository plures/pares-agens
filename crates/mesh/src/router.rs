//! Latency-aware mesh router — the cerebellum.
//!
//! Routes inference requests to the optimal device based on:
//! - Loaded expert availability (via [`ExpertRegistry`])
//! - Round-trip latency (prefer low latency for interactive workloads)
//! - Current device load (skip overloaded devices when an alternative exists)
//! - Request urgency ([`RequestUrgency::Interactive`] vs
//!   [`RequestUrgency::Background`])

use crate::registry::{ExpertRegistry, LocatedExpert};
use crate::MeshError;
use serde::{Deserialize, Serialize};

// ── RequestUrgency ────────────────────────────────────────────────────────────

/// Urgency class for an inference request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RequestUrgency {
    /// User-facing, latency-sensitive (prefer < 200 ms round-trip).
    Interactive,
    /// Background processing — throughput matters more than latency.
    Background,
}

// ── RouteRequest ──────────────────────────────────────────────────────────────

/// Parameters for a single routing decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteRequest {
    /// The expert required to process this request.
    pub expert_id: String,
    /// How latency-sensitive this request is.
    pub urgency: RequestUrgency,
    /// Maximum acceptable round-trip latency (ms) for
    /// [`RequestUrgency::Interactive`] requests.  Ignored for background
    /// requests.
    pub max_latency_ms: Option<u32>,
}

// ── RouteDecision ─────────────────────────────────────────────────────────────

/// The result of a routing decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteDecision {
    /// Selected device identifier.
    pub device_id: String,
    /// The expert on the chosen device.
    pub expert_id: String,
    /// Estimated round-trip latency to the selected device (ms).
    pub estimated_latency_ms: Option<u32>,
    /// `true` when the preferred candidate was overloaded and this is a
    /// best-effort fallback selection.
    pub is_fallback: bool,
}

// ── MeshRouter ────────────────────────────────────────────────────────────────

/// Cerebellum: routes inference queries to the optimal mesh node.
///
/// The router uses the shared [`ExpertRegistry`] as its source of truth.
/// Because `ExpertRegistry` is `Clone` (it wraps an `Arc`), you can share the
/// same registry with multiple routers or with the [`LoadBalancer`](crate::balancer::LoadBalancer).
#[derive(Debug)]
pub struct MeshRouter {
    /// The registry this router consults for candidate devices.
    pub registry: ExpertRegistry,
    /// Load factor above which a device is considered busy and the router will
    /// prefer an alternative.  Defaults to `0.8`.
    pub load_threshold: f32,
}

impl MeshRouter {
    /// Create a router backed by the given [`ExpertRegistry`].
    pub fn new(registry: ExpertRegistry) -> Self {
        Self {
            registry,
            load_threshold: 0.8,
        }
    }

    /// Route an inference request to the best available device.
    ///
    /// ## Selection algorithm
    ///
    /// 1. Locate all devices hosting the requested expert (ordered by latency
    ///    then load).
    /// 2. For **interactive** requests, prefer devices within `max_latency_ms`
    ///    that are not overloaded.
    /// 3. For **background** requests, prefer the least-loaded device
    ///    regardless of latency.
    /// 4. Fall back to the overall best candidate if no non-overloaded device
    ///    qualifies, setting `is_fallback = true`.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::NoDeviceForExpert`] when no device in the registry
    /// hosts the requested expert.
    pub fn route(&self, req: &RouteRequest) -> Result<RouteDecision, MeshError> {
        let candidates = self.registry.locate(&req.expert_id)?;
        self.route_from_candidates(candidates, req)
    }

    /// Route using a pre-fetched (and optionally pre-filtered) candidate list.
    ///
    /// This is called internally by [`MeshRouter::route`] and exposed for use
    /// by the [`LoadBalancer`](crate::balancer::LoadBalancer) which may need to
    /// exclude unavailable devices before routing.
    ///
    /// # Errors
    ///
    /// Returns [`MeshError::NoDeviceForExpert`] when `candidates` is empty.
    pub fn route_from_candidates(
        &self,
        mut candidates: Vec<LocatedExpert>,
        req: &RouteRequest,
    ) -> Result<RouteDecision, MeshError> {
        if candidates.is_empty() {
            return Err(MeshError::NoDeviceForExpert(req.expert_id.clone()));
        }

        // For background requests, re-sort by load ascending so throughput is
        // maximised (latency is irrelevant).  Interactive requests keep the
        // registry's latency-first order.
        if req.urgency == RequestUrgency::Background {
            candidates.sort_by(|a, b| {
                a.device_load
                    .partial_cmp(&b.device_load)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let use_latency_filter = req.urgency == RequestUrgency::Interactive;
        let budget_ms = req.max_latency_ms;

        // Preferred: within latency budget AND not overloaded.
        let preferred = candidates.iter().find(|c| {
            let load_ok = c.device_load <= self.load_threshold;
            let latency_ok = !use_latency_filter
                || budget_ms
                    .map(|b| c.latency_ms.unwrap_or(u32::MAX) <= b)
                    .unwrap_or(true);
            load_ok && latency_ok
        });

        let (selected, is_fallback) = match preferred {
            Some(c) => (c, false),
            // Fallback: best overall given sorted order.
            None => (
                candidates.first().expect("candidates is non-empty"),
                true,
            ),
        };

        Ok(RouteDecision {
            device_id: selected.device_id.clone(),
            expert_id: req.expert_id.clone(),
            estimated_latency_ms: selected.latency_ms,
            is_fallback,
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

    fn make_registry(devices: Vec<DeviceCapabilities>) -> ExpertRegistry {
        let reg = ExpertRegistry::new();
        for d in devices {
            reg.upsert(d).unwrap();
        }
        reg
    }

    fn device(id: &str, expert: &str, load: f32, latency: Option<u32>) -> DeviceCapabilities {
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
            latency_ms: latency,
            metadata: HashMap::new(),
        }
    }

    fn interactive(expert: &str) -> RouteRequest {
        RouteRequest {
            expert_id: expert.into(),
            urgency: RequestUrgency::Interactive,
            max_latency_ms: Some(200),
        }
    }

    fn background(expert: &str) -> RouteRequest {
        RouteRequest {
            expert_id: expert.into(),
            urgency: RequestUrgency::Background,
            max_latency_ms: None,
        }
    }

    #[test]
    fn route_selects_only_available_device() {
        let router = MeshRouter::new(make_registry(vec![device(
            "d1", "chat-8b", 0.3, Some(10),
        )]));
        let decision = router.route(&interactive("chat-8b")).unwrap();
        assert_eq!(decision.device_id, "d1");
        assert!(!decision.is_fallback);
    }

    #[test]
    fn route_errors_when_no_device_has_expert() {
        let router = MeshRouter::new(make_registry(vec![]));
        let err = router.route(&interactive("missing")).unwrap_err();
        assert!(matches!(err, MeshError::NoDeviceForExpert(_)));
    }

    #[test]
    fn route_prefers_low_latency_non_overloaded_device() {
        let router = MeshRouter::new(make_registry(vec![
            // Overloaded but close.
            device("d1", "chat-8b", 0.95, Some(5)),
            // Not overloaded, slightly further.
            device("d2", "chat-8b", 0.3, Some(50)),
        ]));
        let decision = router.route(&interactive("chat-8b")).unwrap();
        // d1 is overloaded so d2 should be preferred.
        assert_eq!(decision.device_id, "d2");
        assert!(!decision.is_fallback);
    }

    #[test]
    fn route_falls_back_when_all_devices_are_overloaded() {
        let router = MeshRouter::new(make_registry(vec![
            device("d1", "chat-8b", 0.95, Some(5)),
            device("d2", "chat-8b", 0.92, Some(50)),
        ]));
        let decision = router.route(&interactive("chat-8b")).unwrap();
        // Both overloaded → fallback to lowest latency.
        assert_eq!(decision.device_id, "d1");
        assert!(decision.is_fallback);
    }

    #[test]
    fn route_background_prefers_least_loaded_device() {
        let router = MeshRouter::new(make_registry(vec![
            device("d1", "chat-8b", 0.3, Some(500)),
            device("d2", "chat-8b", 0.5, Some(5)),
        ]));
        let decision = router.route(&background("chat-8b")).unwrap();
        // Background requests sort by load, so d1 (0.3) is preferred over
        // d2 (0.5) even though d2 has lower latency.
        assert_eq!(decision.device_id, "d1");
        assert!(!decision.is_fallback);
    }

    #[test]
    fn route_rejects_device_outside_latency_budget() {
        let router = MeshRouter::new(make_registry(vec![
            // Both within latency budget; d1 has lower load.
            device("d1", "chat-8b", 0.2, Some(150)),
            device("d2", "chat-8b", 0.6, Some(50)),
        ]));
        // Budget = 200 ms; both qualify.  d2 sorts first (lower latency).
        let decision = router
            .route(&RouteRequest {
                expert_id: "chat-8b".into(),
                urgency: RequestUrgency::Interactive,
                max_latency_ms: Some(200),
            })
            .unwrap();
        // d2 has latency 50 ms < budget 200 ms and load 0.6 < threshold 0.8.
        assert_eq!(decision.device_id, "d2");
    }
}
