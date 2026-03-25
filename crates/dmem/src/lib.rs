//! `pares-agens-dmem` — distributed memory management for the neural mesh.
//!
//! Implements capacity-aware memory caching across all devices in the neural
//! mesh.  Every device has the full logical view of all memories; physical
//! storage is tiered according to each device's capacity budget.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │              DISTRIBUTED MEMORY MESH                    │
//! │                                                         │
//! │  ┌─────────────┐  ┌──────────────┐  ┌───────────────┐  │
//! │  │ Desktop     │  │  Laptop      │  │   Phone       │  │
//! │  │ 2TB SSD     │  │  512GB SSD   │  │   128GB       │  │
//! │  │             │  │              │  │               │  │
//! │  │ FULL COPY   │  │ HOT + WARM   │  │ HOT ONLY     │  │
//! │  │ All memories│  │ Recent 90d   │  │ Recent 7d    │  │
//! │  │ All indexes │  │ + active     │  │ + pinned     │  │
//! │  │ Full embeds │  │   projects   │  │ Compact index│  │
//! │  │             │  │ Partial idx  │  │               │  │
//! │  └──────┬──────┘  └──────┬───────┘  └───────┬───────┘  │
//! │         │                │                   │          │
//! │         └────── Hyperswarm P2P sync ─────────┘          │
//! │                                                         │
//! │  Query from any device → local cache hit or P2P fetch   │
//! └─────────────────────────────────────────────────────────┘
//! ```
//!
//! # Caching tiers
//!
//! | Tier       | Where         | Description                              |
//! |------------|---------------|------------------------------------------|
//! | L1 (hot)   | Device-local  | Recent memories, pinned entries          |
//! | L2 (warm)  | Device-local  | Older memories within the capacity budget|
//! | L3 (cold)  | Device-local  | Compressed, seldom-accessed entries      |
//! | L4 (mesh)  | Remote peers  | Everything else, P2P fetch ~50–200 ms    |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_dmem::{
//!     manager::DistributedMemoryManager,
//!     capacity::DeviceCapacityProfile,
//!     fetch::SimulatedPeerFetcher,
//!     search::{MeshSearch, SimulatedPeerSearcher},
//!     index::QuantizedIndex,
//! };
//!
//! # #[tokio::main]
//! # async fn main() {
//! let profile = DeviceCapacityProfile::warm("laptop-01", 512_000_000_000);
//! let fetcher = SimulatedPeerFetcher::empty();
//! let index = QuantizedIndex::new(384);
//! let mesh: MeshSearch<SimulatedPeerSearcher> = MeshSearch::new(0.8);
//!
//! let mut mgr = DistributedMemoryManager::new(profile, fetcher, index, mesh);
//!
//! mgr.store(
//!     "mem-1".into(),
//!     b"Rust is great".to_vec(),
//!     "2026-01-01T00:00:00Z".to_string(),
//!     &[0.1_f32; 384],
//!     0.85,
//! ).unwrap();
//!
//! let payload = mgr.fetch("mem-1").await.unwrap();
//! assert!(payload.is_some());
//! # }
//! ```

pub mod cache;
pub mod capacity;
pub mod compress;
pub mod error;
pub mod eviction;
pub mod fetch;
pub mod index;
pub mod manager;
pub mod metrics;
pub mod pin;
pub mod policy;
pub mod prefetch;
pub mod search;

pub use cache::MemoryCache;
pub use capacity::{DeviceCapacityProfile, StorageBudget, StorageTier};
pub use error::DmemError;
pub use eviction::SmartEviction;
pub use manager::DistributedMemoryManager;
pub use metrics::CacheMetrics;
pub use pin::PinRegistry;
pub use policy::StoragePolicy;
pub use prefetch::PrefetchPredictor;
