//! Observability events and telemetry emitter.
//!
//! [`TelemetryEmitter`] is a zero-cost abstraction over a user-supplied sink
//! function.  Each optimizer iteration emits a structured [`ObservabilityEvent`]
//! that callers can forward to a logging system, metrics pipeline, or
//! in-memory buffer.
//!
//! A no-op emitter is provided via [`TelemetryEmitter::noop()`] for tests and
//! benchmarks that do not need side-effecting telemetry.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ── ObservabilityEvent ────────────────────────────────────────────────────────

/// Structured event emitted by the optimizer at key lifecycle points.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum ObservabilityEvent {
    /// Emitted once at the start of an optimization episode.
    EpisodeStarted {
        /// Run identifier from [`OptimizerInput`](crate::OptimizerInput).
        run_id: String,
        /// Policy identifier from [`OptimizerInput`](crate::OptimizerInput).
        policy_id: String,
        /// Objective score computed from the initial (unoptimized) input.
        initial_score: f64,
        /// Key-value context forwarded verbatim from the input.
        context: HashMap<String, String>,
    },

    /// Emitted after each optimizer iteration.
    IterationCompleted {
        /// Run identifier.
        run_id: String,
        /// 1-based iteration counter.
        iteration: u32,
        /// Objective score at the end of this iteration.
        objective_score: f64,
        /// Absolute improvement over the previous iteration's score.
        improvement: f64,
        /// Number of constraints violated in the candidate solution.
        violated_constraint_count: usize,
    },

    /// Emitted when one or more constraints are violated during an iteration.
    ConstraintViolated {
        /// Run identifier.
        run_id: String,
        /// 1-based iteration counter.
        iteration: u32,
        /// Names of the violated constraints.
        violated_constraints: Vec<String>,
    },

    /// Emitted once when the optimization episode finishes.
    EpisodeCompleted {
        /// Run identifier.
        run_id: String,
        /// Policy identifier.
        policy_id: String,
        /// Best objective score achieved.
        final_score: f64,
        /// Total iterations executed.
        iterations: u32,
        /// Whether the optimizer converged within tolerance.
        converged: bool,
        /// Names of any constraints violated in the final solution.
        violated_constraints: Vec<String>,
    },
}

impl ObservabilityEvent {
    /// Return the `run_id` associated with this event.
    #[must_use]
    pub fn run_id(&self) -> &str {
        match self {
            Self::EpisodeStarted { run_id, .. }
            | Self::IterationCompleted { run_id, .. }
            | Self::ConstraintViolated { run_id, .. }
            | Self::EpisodeCompleted { run_id, .. } => run_id,
        }
    }
}

// ── TelemetryEmitter ──────────────────────────────────────────────────────────

/// A pluggable telemetry sink.
///
/// Wrap any `Fn(ObservabilityEvent)` closure — or use [`TelemetryEmitter::noop`]
/// for tests that don't need side effects.
///
/// # Example
///
/// ```rust
/// use pares_agens_optimizer::telemetry::{TelemetryEmitter, ObservabilityEvent};
///
/// let emitter = TelemetryEmitter::new(|event: ObservabilityEvent| {
///     eprintln!("[telemetry] {:?}", event);
/// });
/// ```
pub struct TelemetryEmitter {
    sink: Box<dyn Fn(ObservabilityEvent) + Send + Sync>,
}

impl TelemetryEmitter {
    /// Create a new emitter backed by `sink`.
    pub fn new<F>(sink: F) -> Self
    where
        F: Fn(ObservabilityEvent) + Send + Sync + 'static,
    {
        Self {
            sink: Box::new(sink),
        }
    }

    /// Create a no-op emitter that discards all events.
    #[must_use]
    pub fn noop() -> Self {
        Self::new(|_| {})
    }

    /// Create an emitter that collects all events into a shared `Vec`.
    ///
    /// Returns the emitter and an [`std::sync::Arc`]`<`[`std::sync::Mutex`]`<Vec<ObservabilityEvent>>>`
    /// that the caller can inspect after the optimizer finishes.
    #[must_use]
    pub fn collecting() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<ObservabilityEvent>>>) {
        let store: std::sync::Arc<std::sync::Mutex<Vec<ObservabilityEvent>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let store_clone = store.clone();
        let emitter = Self::new(move |event| {
            store_clone.lock().unwrap().push(event);
        });
        (emitter, store)
    }

    /// Emit a single event to the backing sink.
    pub fn emit(&self, event: ObservabilityEvent) {
        (self.sink)(event);
    }
}
