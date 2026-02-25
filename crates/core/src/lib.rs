//! `pares-agens-core` — reactive event loop and procedure executor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use pares_agens_core::{
//!     executor::Executor,
//!     handlers::{OnMessage, OnTimer},
//!     procedure::ProcedureRegistry,
//! };
//!
//! # #[tokio::main]
//! # async fn main() {
//! let mut registry = ProcedureRegistry::new();
//! registry.register(Box::new(OnMessage));
//! registry.register(Box::new(OnTimer));
//!
//! let executor = Executor::new(registry);
//! // executor.run(&source, 0).await;  // pass a real EventSource
//! # }
//! ```

pub mod event;
pub mod executor;
pub mod handlers;
pub mod procedure;
pub mod source;
