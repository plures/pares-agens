//! Distributed inference routing across Hyperswarm-connected nodes.
//!
//! Each node advertises which experts it serves. The cerebellum can call
//! [`DistributedInferenceRouter::route_for_cerebellum`] to resolve a prompt to
//! a `(node, expert)` target before dispatch.

use std::collections::HashMap;

use tracing::info;

use crate::{CpuExpert, CpuExpertPool, InferenceError};

/// Inference capability advertisement for one node in the cluster.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeInferenceCapability {
    /// Stable node identifier (typically hostname or device ID).
    pub node_id: String,
    /// Hostname or IP used to reach this node.
    pub host: String,
    /// Port exposed by this node's inference service.
    pub port: u16,
    /// Experts currently served by this node's local expert pool.
    pub experts: Vec<CpuExpert>,
    /// Estimated free KV cache capacity (MiB) used as a routing hint.
    #[serde(default)]
    pub kv_cache_available_mb: u64,
}

impl NodeInferenceCapability {
    fn supports(&self, expert: CpuExpert) -> bool {
        self.experts.contains(&expert)
    }
}

/// Routing decision containing target node + expert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeExpertRoute {
    /// Selected node identifier.
    pub node_id: String,
    /// Selected node host.
    pub host: String,
    /// Selected node port.
    pub port: u16,
    /// Selected expert role.
    pub expert: CpuExpert,
}

/// In-memory distributed router for node capability advertisements.
#[derive(Debug, Default)]
pub struct DistributedInferenceRouter {
    nodes: HashMap<String, NodeInferenceCapability>,
}

impl DistributedInferenceRouter {
    /// Create an empty router.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a node capability advertisement.
    pub fn upsert_node(&mut self, capability: NodeInferenceCapability) {
        info!(
            node_id = capability.node_id,
            host = capability.host,
            port = capability.port,
            experts = capability.experts.len(),
            "registered distributed inference node capability"
        );
        self.nodes.insert(capability.node_id.clone(), capability);
    }

    /// Remove a node capability advertisement.
    pub fn remove_node(&mut self, node_id: &str) -> Option<NodeInferenceCapability> {
        self.nodes.remove(node_id)
    }

    /// Return all known node capabilities.
    pub fn list_nodes(&self) -> impl Iterator<Item = &NodeInferenceCapability> {
        self.nodes.values()
    }

    /// Route a cerebellum prompt to the best `(node, expert)` target.
    pub fn route_for_cerebellum(&self, prompt: &str) -> Result<NodeExpertRoute, InferenceError> {
        let expert = CpuExpertPool::route_query(prompt);
        self.route_expert(expert)
    }

    /// Route a specific expert to the best node advertising it.
    pub fn route_expert(&self, expert: CpuExpert) -> Result<NodeExpertRoute, InferenceError> {
        let selected = self
            .nodes
            .values()
            .filter(|node| node.supports(expert))
            .max_by(|left, right| {
                left.kv_cache_available_mb
                    .cmp(&right.kv_cache_available_mb)
                    .then_with(|| right.node_id.cmp(&left.node_id))
            })
            .ok_or_else(|| InferenceError::NoNodeForExpert {
                expert: expert.as_str().to_string(),
            })?;

        info!(
            node_id = selected.node_id,
            host = selected.host,
            port = selected.port,
            expert = expert.as_str(),
            "distributed inference route resolved"
        );

        Ok(NodeExpertRoute {
            node_id: selected.node_id.clone(),
            host: selected.host.clone(),
            port: selected.port,
            expert,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        node_id: &str,
        host: &str,
        port: u16,
        experts: Vec<CpuExpert>,
        kv_cache_available_mb: u64,
    ) -> NodeInferenceCapability {
        NodeInferenceCapability {
            node_id: node_id.to_string(),
            host: host.to_string(),
            port,
            experts,
            kv_cache_available_mb,
        }
    }

    #[test]
    fn route_for_cerebellum_selects_matching_node_and_expert() {
        let mut router = DistributedInferenceRouter::new();
        router.upsert_node(node(
            "node-a",
            "10.0.0.11",
            8081,
            vec![CpuExpert::Monitoring, CpuExpert::Routing],
            512,
        ));
        router.upsert_node(node(
            "node-b",
            "10.0.0.12",
            8082,
            vec![CpuExpert::Compliance],
            384,
        ));

        let route = router
            .route_for_cerebellum("Need SOC2 compliance audit guidance")
            .unwrap();
        assert_eq!(route.node_id, "node-b");
        assert_eq!(route.expert, CpuExpert::Compliance);
        assert_eq!(route.host, "10.0.0.12");
        assert_eq!(route.port, 8082);
    }

    #[test]
    fn route_expert_prefers_node_with_more_kv_capacity() {
        let mut router = DistributedInferenceRouter::new();
        router.upsert_node(node(
            "node-a",
            "10.0.0.21",
            8081,
            vec![CpuExpert::Routing],
            128,
        ));
        router.upsert_node(node(
            "node-b",
            "10.0.0.22",
            8082,
            vec![CpuExpert::Routing],
            768,
        ));

        let route = router.route_expert(CpuExpert::Routing).unwrap();
        assert_eq!(route.node_id, "node-b");
    }

    #[test]
    fn route_expert_errors_when_no_node_supports_expert() {
        let mut router = DistributedInferenceRouter::new();
        router.upsert_node(node(
            "node-a",
            "10.0.0.31",
            8081,
            vec![CpuExpert::Monitoring],
            512,
        ));

        let err = router.route_expert(CpuExpert::Capacity).unwrap_err();
        assert!(matches!(err, InferenceError::NoNodeForExpert { .. }));
    }

    #[test]
    fn remove_node_excludes_it_from_future_routes() {
        let mut router = DistributedInferenceRouter::new();
        router.upsert_node(node(
            "node-a",
            "10.0.0.41",
            8081,
            vec![CpuExpert::Deployment],
            512,
        ));
        router.remove_node("node-a");

        let err = router.route_expert(CpuExpert::Deployment).unwrap_err();
        assert!(matches!(err, InferenceError::NoNodeForExpert { .. }));
    }
}
