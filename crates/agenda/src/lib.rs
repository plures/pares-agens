//! `pares-agens-agenda` — Issue and pull-request workflow management for Pares Agens.
//!
//! Provides an in-process issue tracker and lightweight pull-request model so
//! that Pares agents can manage their own work items without requiring an
//! external issue tracker during the local-first MVP phase.
//!
//! # Modules
//!
//! - [`issue`] — [`Issue`](issue::Issue) work-item model and lifecycle.
//! - [`manager`] — [`AgendaManager`](manager::AgendaManager): CRUD for issues and PRs.
//! - [`scheduler`] — tokio-based scheduler with cron/interval tasks and optional
//!   PluresDB persistence.

#![warn(missing_docs)]

pub mod issue;
pub mod manager;

use thiserror::Error;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors that can occur during Agenda operations.
#[derive(Debug, Error)]
pub enum AgendaError {
    /// No issue or PR with the given ID exists.
    #[error("not found: {0}")]
    NotFound(String),

    /// An issue or PR field contains an invalid value.
    #[error("invalid field: {0}")]
    InvalidField(String),

    /// The requested state transition is not allowed.
    #[error("invalid transition: {0}")]
    InvalidTransition(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
pub mod scheduler;
