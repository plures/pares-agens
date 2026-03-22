//! `pares-agens-mesh` — neural mesh for distributed expert routing.
//!
//! All of a user's devices form a **neural mesh**: laptop GPU runs heavy
//! experts, phone CPU runs small experts, desktop GPU runs the biggest models.
//! The cerebellum (see [`router::MeshRouter`]) routes queries to the optimal
//! device based on available compute, latency, and expert specialisation.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    NEURAL MESH                          │
//! │                                                         │
//! │  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
//! │  │ Desktop GPU │  │  Laptop CPU  │  │   Phone NPU   │  │
//! │  │ RTX 4070    │  │  M3 Pro      │  │   Snapdragon  │  │
//! │  │             │  │              │  │               │  │
//! │  │ code-30B    │  │ chat-8B      │  │ triage-2B     │  │
//! │  │ reason-30B  │  │ write-8B     │  │ classify-2B   │  │
//! │  │ math-8B     │  │ search-8B    │  │               │  │
//! │  │ analyze-8B  │  │              │  │               │  │
//! │  └──────┬──────┘  └──────┬───────┘  └───────┬───────┘  │
//! │         │                │                   │          │
//! │         └────── Hyperswarm P2P ──────────────┘          │
//! │                          │                              │
//! │                   ┌──────┴──────┐                       │
//! │                   │ Cerebellum  │                       │
//! │                   │ (any node)  │                       │
//! │                   └─────────────┘                       │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Modules
//!
//! | Module | Purpose |
//! |--------|---------|
//! | [`device`] | Device capability advertisement (`DeviceCapabilities`, `ComputeClass`, `ExpertSpec`) |
//! | [`registry`] | Thread-safe distributed expert registry (`ExpertRegistry`) |
//! | [`router`] | Latency-aware request routing — the cerebellum (`MeshRouter`) |
//! | [`protocol`] | P2P inference protocol: prompt → token stream (`InferenceProtocol`, `PeerTransport`) |
//! | [`balancer`] | Load balancing and fault tolerance with circuit-breaker (`LoadBalancer`) |
//! | [`migration`] | Expert migration between devices (`MigrationTransport`) |
//! | [`dashboard`] | Mesh-wide utilisation and latency dashboard (`MeshDashboard`, `MeshStats`) |
//! | [`placement`] | Auto-optimise expert placement across the mesh (`PlacementOptimizer`) |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_mesh::{
//!     device::{ComputeClass, DeviceCapabilities, ExpertSpec},
//!     registry::ExpertRegistry,
//!     router::{MeshRouter, RequestUrgency, RouteRequest},
//!     dashboard::MeshDashboard,
//! };
//! use std::collections::HashMap;
//!
//! // 1. Create the shared expert registry.
//! let registry = ExpertRegistry::new();
//!
//! // 2. Advertise a device.
//! registry.upsert(DeviceCapabilities {
//!     device_id: "desktop-rtx4070".into(),
//!     display_name: "Desktop RTX 4070".into(),
//!     compute_class: ComputeClass::Gpu,
//!     memory_mb: 12_288,
//!     loaded_experts: vec![ExpertSpec {
//!         expert_id: "code-30b".into(),
//!         model_name: "Llama-3-30B-Code".into(),
//!         params_billions: 30.0,
//!     }],
//!     load_factor: 0.3,
//!     latency_ms: Some(2),
//!     metadata: HashMap::new(),
//! }).unwrap();
//!
//! // 3. Route a request to the best device for an expert.
//! let router = MeshRouter::new(registry.clone());
//! let decision = router.route(&RouteRequest {
//!     expert_id: "code-30b".into(),
//!     urgency: RequestUrgency::Interactive,
//!     max_latency_ms: Some(200),
//! }).unwrap();
//! assert_eq!(decision.device_id, "desktop-rtx4070");
//!
//! // 4. Inspect mesh-wide stats.
//! let stats = MeshDashboard::new(registry).snapshot().unwrap();
//! assert_eq!(stats.unique_expert_count, 1);
//! ```

pub mod balancer;
pub mod dashboard;
pub mod device;
pub mod migration;
pub mod placement;
pub mod protocol;
pub mod registry;
pub mod router;

use thiserror::Error;

// ── MeshError ─────────────────────────────────────────────────────────────────

/// Errors that can occur in the neural mesh.
#[derive(Debug, Error)]
pub enum MeshError {
    /// No device in the registry currently hosts the requested expert.
    #[error("no device hosts expert: {0}")]
    NoDeviceForExpert(String),

    /// The registry's internal `RwLock` was poisoned by a panicking writer.
    #[error("expert registry lock was poisoned")]
    RegistryLockPoisoned,

    /// A load balancer `Mutex` was poisoned by a panicking thread.
    #[error("load balancer lock was poisoned")]
    LockPoisoned,

    /// The mesh has insufficient total capacity to place all requested experts.
    #[error("insufficient mesh capacity to place all experts")]
    InsufficientMeshCapacity,

    /// Transport-level error when communicating with a peer device.
    #[error("peer transport error: {0}")]
    Transport(String),

    /// The target device is not reachable over the mesh network.
    #[error("device unreachable: {0}")]
    DeviceUnreachable(String),

    /// JSON (de)serialisation failed.
    #[error("serialisation error: {0}")]
    Json(#[from] serde_json::Error),
}
