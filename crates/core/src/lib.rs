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

pub mod agent;
pub mod event;
pub mod executor;
pub mod handlers;
pub mod memory;
pub mod model;
pub mod procedure;
pub mod setup;
pub mod source;
pub mod state;
pub mod praxis;

pub use agent::{Agent, InMemory, Memory};
pub use event::Event;
