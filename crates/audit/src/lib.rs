#![warn(missing_docs)]
//! `pares-agens-audit` — Comprehensive audit log for Pares Agens.
//!
//! Provides a full audit trail of what data went where: every model call,
//! every memory write, every external action.
//!
//! # Modules
//!
//! - [`event`] — [`EventKind`], [`AuditEvent`]: structured event definitions.
//! - [`store`] — [`AuditStore`] trait, [`InMemoryAuditStore`], and [`PluresDbAuditStore`].
//! - [`query`] — [`AuditQuery`]: filter by date, action type, and destination.
//! - [`export`] — [`export_json`] / [`export_csv`]: compliance export helpers.
//! - [`retention`] — [`RetentionConfig`] and [`apply_retention`]: log rotation.
//!
//! # Quick start — in-memory (tests / single-process)
//!
//! ```rust
//! # use std::sync::Arc;
//! # use pares_agens_audit::{
//! #     event::{AuditEvent, EventKind},
//! #     store::{AuditStore, InMemoryAuditStore},
//! #     query::AuditQuery,
//! # };
//! # #[tokio::main] async fn main() {
//! let store = Arc::new(InMemoryAuditStore::new());
//!
//! let event = AuditEvent::new(
//!     EventKind::ModelCall,
//!     "agent-1",
//!     "gpt-4o",
//!     "prompt tokens: 512",
//!     false,
//! );
//! store.append(event).await;
//!
//! let query = AuditQuery::new().with_kind(EventKind::ModelCall);
//! let results = store.query(&query).await;
//! assert_eq!(results.len(), 1);
//! # }
//! ```
//!
//! # Persistent store (production)
//!
//! ```rust,no_run
//! # use std::sync::Arc;
//! # use pares_agens_audit::{
//! #     event::{AuditEvent, EventKind},
//! #     store::{AuditStore, PluresDbAuditStore},
//! #     retention::{RetentionConfig, apply_retention},
//! # };
//! # #[tokio::main] async fn main() {
//! // Open (or create) a durable PluresDB-backed store.
//! let store = Arc::new(
//!     PluresDbAuditStore::open("/var/lib/pares-agens/audit").expect("open audit store"),
//! );
//!
//! store.append(AuditEvent::new(
//!     EventKind::MemoryWrite,
//!     "agent-1",
//!     "memory-store",
//!     "entry id: abc123",
//!     false,
//! )).await;
//!
//! // Apply 90-day retention policy.
//! let config = RetentionConfig::days(90);
//! apply_retention(store.as_ref(), &config).await;
//! # }
//! ```

pub mod event;
pub mod export;
pub mod query;
pub mod retention;
pub mod store;

pub use event::{AuditEvent, EventKind};
pub use export::{export_csv, export_json};
pub use query::AuditQuery;
pub use retention::{apply_retention, RetentionConfig};
pub use store::{AuditStore, InMemoryAuditStore, PluresDbAuditStore};

/// Errors that can occur during audit log operations.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// Serialization/deserialization failed.
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),

    /// An I/O error occurred during export.
    #[error("export error: {0}")]
    Export(String),

    /// The supplied timestamp string is not valid RFC 3339.
    #[error("invalid timestamp: {0}")]
    InvalidTimestamp(String),
}
