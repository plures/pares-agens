//! Plugin framework — turns pares-agens into an application platform.
//!
//! Plugins declare schemas, logic, tools, and optional UI via a
//! [`PluginManifest`].  The [`PluginRuntime`] loads manifests, registers
//! their schemas in PluresDB, exposes generic CRUD tools, and injects
//! schema context into the system prompt so the AI knows what data exists.

pub mod manifest;
pub mod runtime;
pub mod crud;
pub mod error;
pub mod executor;

pub use manifest::*;
pub use runtime::PluginRuntime;
pub use error::PluginError;
pub use executor::PluginCrudExecutor;
