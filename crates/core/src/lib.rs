#![allow(missing_docs)] // TODO: re-enable once API stabilizes
//! `pares-agens-core` — cognition layer (agent, cerebellum, delegation, memory).
//!
//! As of Stage S-C this crate owns **only** the cognition surface. The platform
//! runtime (event loop, procedure executor, state, model client, plugins, spine,
//! praxis, tasks, secrets, license, renderers, …) lives in the
//! [`pares_radix_core`] crate, which this crate depends on. Cognition code
//! references platform items via `pares_radix_core::` directly — it no longer
//! bundles its own copies of those modules.
//!
//! The message-classifier contract (`ClassifierBackend`, `MessageClassification`,
//! `MessageIntent`, `CLASSIFIER_SYSTEM_PROMPT`) is owned by
//! [`pares_radix_core::classifier`] and re-exported here for ergonomic access;
//! the higher-level [`cerebellum::classifier::CerebellumClassifier`] orchestrator
//! is owned by this crate and implements/uses that contract.

// ---------------------------------------------------------------------------
// Cognition modules (owned by this crate).
// ---------------------------------------------------------------------------

/// High-level agent abstraction and in-memory storage.
pub mod agent;
/// Cerebellum orchestrator — autorecall, routing, and pipeline execution.
pub mod cerebellum;
/// Local multi-agent delegation and concurrent sub-task execution.
pub mod delegation;
/// Lightweight process diagnostics primitives (VmRSS / resident-set sampling).
pub mod diagnostics;
/// Feature-gate helpers over the license tier.
pub mod features;
/// HeadroomActionHandler — production implementation of headroom .px side-effect actors.
pub mod headroom;
/// Headroom context-compression bridge wired into the agent model loop.
pub mod headroom_bridge;
/// Heartbeat system — periodic proactive check-ins.
pub mod heartbeat;
/// PluresLM — native memory recall, capture, and context injection.
pub mod memory;
/// Model selection chain — BitNet → conscious → deep fallback.
pub mod model_chain;
/// Personality contracts — identity, tone, and behavioral rules.
pub mod personality;
/// Dynamic system prompt builder from personality contracts.
pub mod prompt_builder;

// ---------------------------------------------------------------------------
// Public cognition API re-exports.
// ---------------------------------------------------------------------------

pub use agent::Memory as AgentMemory;
pub use agent::{Agent, InMemory};
pub use headroom::HeadroomActionHandler;
pub use headroom_bridge::HeadroomHook;

/// Re-export the single canonical message-classifier contract from the platform
/// crate so callers can reach it as `pares_agens_core::{ClassifierBackend, …}`
/// without depending on `pares-radix-core` directly. The cognition crate's
/// `CerebellumClassifier` orchestrator implements this contract.
pub use pares_radix_core::classifier::{
    ClassifierBackend, MessageClassification, MessageIntent, CLASSIFIER_SYSTEM_PROMPT,
};
