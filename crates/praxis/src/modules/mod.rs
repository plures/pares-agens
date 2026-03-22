//! Four concrete [`PraxisModule`] implementations for Pares Agens.
//!
//! | Module | Description |
//! |--------|-------------|
//! | [`agent_lifecycle`] | Spawn/retire gates, health validation, capability registration |
//! | [`task_routing`] | Assignment rules, priority logic, load balancing |
//! | [`coordination`] | Multi-agent collaboration, conflict resolution, consensus |
//! | [`safety`] | Action boundaries, resource limits, escalation triggers |

pub mod agent_lifecycle;
pub mod coordination;
pub mod safety;
pub mod task_routing;
