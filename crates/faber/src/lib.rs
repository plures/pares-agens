//! `pares-agens-faber` — Agent-first CI runner for Pares Agens.
//!
//! Provides lightweight continuous-integration primitives that run entirely
//! within the Pares agent runtime.  A [`Pipeline`] is a named sequence of
//! [`Step`]s; a [`CiRunner`] executes pipelines and records per-step
//! [`StepResult`]s into a [`RunReport`].
//!
//! # Modules
//!
//! - [`pipeline`] — [`Pipeline`](pipeline::Pipeline) and [`Step`](pipeline::Step) definitions.
//! - [`runner`] — [`CiRunner`](runner::CiRunner) execution engine.

#![warn(missing_docs)]

pub mod pipeline;
pub mod runner;

use thiserror::Error;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors that can occur during Faber CI operations.
#[derive(Debug, Error)]
pub enum FaberError {
    /// A pipeline definition is invalid (e.g. contains no steps).
    #[error("invalid pipeline: {0}")]
    InvalidPipeline(String),

    /// A step within a pipeline failed during execution.
    #[error("step failed: {step} — {reason}")]
    StepFailed {
        /// Name of the step that failed.
        step: String,
        /// Human-readable failure reason.
        reason: String,
    },

    /// A pipeline run was cancelled before completing.
    #[error("run cancelled: {0}")]
    Cancelled(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
