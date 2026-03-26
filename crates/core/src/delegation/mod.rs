//! Local multi-agent delegation.
//!
//! The delegation system lets the cerebellum spawn and coordinate multiple
//! specialised sub-agents within a single pares-agens instance.  Each
//! sub-agent has:
//!
//! * Its own system prompt and model preference.
//! * A curated set of allowed tool names.
//! * An isolated conversation history ([`AgentContext`]).
//!
//! The [`DelegationBroker`] accepts a list of [`SubTask`]s (one per required
//! specialisation) and executes them concurrently.  Results are merged by the
//! [`ResultAggregator`] into a single [`AggregatedResult`] that is returned
//! to the caller.
//!
//! # Built-in agents
//!
//! Three built-in definitions are provided via [`builtin::researcher`],
//! [`builtin::coder`], and [`builtin::analyst`].  They can be registered
//! into an [`AgentRegistry`] using [`AgentRegistry::register_builtins`].
//!
//! # User-defined agents
//!
//! Custom agents are defined via [`AgentDefinition`] and registered with
//! [`AgentRegistry::register`].  A [`SubTask`] selects an agent by name.
//!
//! # Concurrent execution
//!
//! [`DelegationBroker::delegate`] fans out all sub-tasks in parallel using
//! `tokio::task::JoinSet`.

pub mod aggregator;
pub mod broker;
pub mod builtin;
pub mod context;
pub mod registry;

pub use aggregator::{AggregatedResult, ResultAggregator};
pub use broker::{DelegationBroker, SubTask, SubTaskResult};
pub use context::AgentContext;
pub use registry::{AgentCapabilities, AgentDefinition, AgentRegistry};

/// Errors that can arise during delegation.
#[derive(Debug, thiserror::Error)]
pub enum DelegationError {
    /// No agent with the requested name is registered.
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    /// The underlying model client returned an error.
    #[error("model error in agent '{agent}': {message}")]
    ModelError {
        /// Name of the agent that encountered the error.
        agent: String,
        /// Human-readable error description from the model client.
        message: String,
    },
    /// A task join handle panicked.
    #[error("sub-task panicked: {0}")]
    Panic(String),
}
