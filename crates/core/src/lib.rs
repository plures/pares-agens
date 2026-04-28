#![allow(missing_docs)] // TODO: re-enable once API stabilizes
//! `pares-agens-core` — reactive event loop and procedure executor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use pares_agens_core::{
//!     executor::Executor,
//!     handlers::{OnMessage, OnStateChange, OnTimer},
//!     memory::MemoryClient,
//!     model::{ModelClient, ToolDispatcher},
//!     procedure::ProcedureRegistry,
//! };
//!
//! # #[tokio::main]
//! # async fn main() {
//! // Wire up your memory/model/tool implementations here.
//! // See the `handlers` module docs for the full interface.
//!
//! let on_timer = OnTimer::new();
//! let on_state_change = OnStateChange::new();
//!
//! let mut registry = ProcedureRegistry::new();
//! registry.register(Box::new(on_timer));
//! registry.register(Box::new(on_state_change));
//!
//! let executor = Executor::new(registry);
//! // executor.run(&source, 0).await;  // pass a real EventSource
//! # }
//! ```

/// High-level agent abstraction and in-memory storage.
pub mod agent;
/// Authentication helpers for external providers.
pub mod auth;
/// Cerebellum orchestrator — autorecall, routing, and pipeline execution.
pub mod cerebellum;
/// Local multi-agent delegation and concurrent sub-task execution.
pub mod delegation;
/// Event types consumed and emitted by the reactive event loop.
pub mod event;
/// Reactive event loop executor with optimization safety enforcement.
pub mod executor;
/// Feature-gate helpers over the license tier.
pub mod features;
/// Built-in event handler procedures.
pub mod handlers;
/// License key validation and Pro feature gates.
pub mod license;
/// PluresLM — native memory recall, capture, and context injection.
pub mod memory;
/// LLM model client and tool dispatcher abstractions.
pub mod model;
/// Optimization safety gates for runtime enforcement.
pub mod optimization;
/// Praxis decision ledger and approval gate procedures.
pub mod praxis;
/// Procedure registry and priority-based event dispatch.
pub mod procedure;
/// Shipped default PluresLM procedure bundles (JSON library).
pub mod procedures;
/// Encrypted secret storage abstraction.
pub mod secrets;
/// First-run wizard configuration and state persistence.
pub mod setup;
/// Event source abstraction for polling inbound events.
pub mod source;
/// Key-value state store backed by PluresDB.
pub mod state;
/// Privacy-first anonymous telemetry aggregation primitives.
pub mod telemetry;

pub use agent::Memory as AgentMemory;
pub use agent::{Agent, InMemory};
pub use event::Event;
pub use state::{InMemoryStateStore, PluresDbStateStore, StateStore};
/// Channel capability contracts for output rendering.
pub mod channel_contract;
/// Event spine — bridges pares-agens to PluresDB's AgensRuntime.
pub mod event_spine;
pub mod lifecycle;
/// Personality contracts — identity, tone, and behavioral rules.
pub mod personality;
/// Dynamic system prompt builder from personality contracts.
pub mod prompt_builder;
/// Channel-specific output renderers.
pub mod renderers;
/// Tool execution governance — policies, timeouts, blocked-command filtering.
pub mod tool_governance;
/// Heartbeat system — periodic proactive check-ins.
pub mod heartbeat;
/// Plugin framework — application platform for schema-driven apps.
pub mod plugins;
