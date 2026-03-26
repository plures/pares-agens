//! Praxis decision ledger and approval gate procedures.
//!
//! # Overview
//!
//! The `praxis` module provides deterministic decision tracking and approval
//! gates for every agent action.  It maps to the `praxis_ledger` table in
//! PluresDB.
//!
//! ## Table schema
//! ```text
//! id | timestamp | event_type | action | rationale | validation_status | gate_status
//! ```
//!
//! ## Procedures
//! - [`Ledger::log`] — append an audit entry for every model interaction
//! - [`Ledger::validate`] — check an action against stored policies
//! - [`Ledger::gate`] — create an approval gate and notify the user
//! - [`Ledger::check_gates`] — return all pending gates for a given context
//!
//! ## Gate flow
//! 1. Procedure wants to perform a high-stakes action (send email, post publicly).
//! 2. `validate()` → returns [`ValidationStatus::GateRequired`].
//! 3. `gate()` → creates a [`GateStatus::Pending`] entry in the ledger and
//!    notifies the user via the active channel.
//! 4. User calls [`Ledger::resolve_gate`] with `Approved` or `Rejected`.
//! 5. Procedure continues or aborts based on the resolved status.

/// Guidance service — stores and retrieves Praxis coprocessor guidance entries.
pub mod guidance;
/// Decision ledger — append-only audit trail with optional approval gates.
pub mod ledger;

pub use guidance::{AnalysisEvent, GuidanceCategory, GuidanceEntry, GuidanceService, SourceSpan};
pub use ledger::{GateStatus, Ledger, LedgerEntry, ValidationStatus};
