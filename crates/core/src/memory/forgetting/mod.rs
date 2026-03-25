//! Controlled forgetting — retention policies, purge, and simulation drills.
//!
//! This module implements the full forgetting workflow:
//!
//! | Sub-module | Description |
//! |------------|-------------|
//! | [`policy`] | Define per-category [`RetentionRule`]s in a [`RetentionPolicy`] |
//! | [`engine`] | [`ForgettingEngine`]: dry-run, execute, restore, scheduled purge |
//! | [`gate`] | [`ApprovalGate`] trait + built-in guards (auto, deny, threshold) |
//! | [`audit`] | Append-only [`AuditLog`] — the "praxis ledger" for deletions |
//! | [`simulation`] | [`SimulationDrill`]: intentional memory-loss for resilience testing |
//!
//! # Quick-start
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use pares_agens_core::memory::{
//! #     store::InMemoryStore,
//! #     entry::MemoryCategory,
//! #     forgetting::{
//! #         ForgettingEngine, RetentionPolicy, RetentionRule, AutoApproveGate,
//! #     },
//! # };
//! # #[tokio::main] async fn main() {
//! // 1. Create a store and seed it with some memories
//! let store = Arc::new(InMemoryStore::new());
//!
//! // 2. Build an engine with a 24-hour recovery window
//! let engine = ForgettingEngine::new(store, 24);
//!
//! // 3. Define a retention policy
//! let mut policy = RetentionPolicy::new();
//! policy.set_rule(MemoryCategory::Conversation, RetentionRule::expire_after(30));
//!
//! // 4. Dry-run to inspect impact
//! let report = engine.dry_run(&policy).await.unwrap();
//! println!("{}", report.summary());
//!
//! // 5. Execute with auto-approval (for scheduled jobs)
//! let result = engine.execute(report, &AutoApproveGate).await.unwrap();
//! println!("soft-deleted {} entries", result.soft_deleted_ids.len());
//!
//! // 6. Restore an entry within the recovery window
//! if let Ok(entry) = engine.restore("some-id").await {
//!     println!("recovered: {}", entry.content);
//! }
//! # }
//! ```

pub mod audit;
pub mod engine;
pub mod gate;
pub mod policy;
pub mod simulation;

// Convenient top-level re-exports
pub use audit::{AuditAction, AuditEntry, AuditLog};
pub use engine::{ForgettingEngine, ImpactEntry, PurgeReport, PurgeResult};
pub use gate::{ApprovalGate, AutoApproveGate, DenyAllGate, PreApprovedGate, ThresholdGate};
pub use policy::{RetentionPolicy, RetentionRule};
pub use simulation::{run_drill, DrillResult, SimulationDrill};
