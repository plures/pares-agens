//! `pares-agens-praxis` — Declarative logic management for Pares Agens.
//!
//! This crate provides a typed rules engine modelled on the `@plures/praxis`
//! framework.  Every piece of routing, lifecycle, coordination, and safety
//! logic in the agent mesh is expressed as an auditable [`Rule`] that returns
//! a typed [`RuleResult`].
//!
//! # Core concepts
//!
//! | Concept | Description |
//! |---------|-------------|
//! | [`RuleResult`] | Typed outcome of evaluating a rule: Pass / Fail / Warning / Gate |
//! | [`Rule`] | Trait implemented by every named rule |
//! | [`RuleContext`] | Input bag (action + payload + metadata) passed to `Rule::evaluate` |
//! | [`RuleCategory`] | Factory category: Input / State / Data |
//! | [`RulesFactory`] | Builds and owns rules grouped by [`RuleCategory`] |
//! | [`PraxisModule`] | Domain grouping of rules with a completeness audit |
//! | [`CompletenessReport`] | Coverage summary produced by a [`PraxisModule`] |
//!
//! # PraxisModules
//!
//! Four modules cover the decision surface of the agent mesh:
//!
//! | Module | Import |
//! |--------|--------|
//! | Agent lifecycle | [`modules::agent_lifecycle::AgentLifecycleModule`] |
//! | Task routing | [`modules::task_routing::TaskRoutingModule`] |
//! | Agent coordination | [`modules::coordination::CoordinationModule`] |
//! | Safety constraints | [`modules::safety::SafetyModule`] |
//!
//! # Quick start
//!
//! ```rust
//! use pares_agens_praxis::{
//!     RuleContext, RuleResult,
//!     module::PraxisModule,
//!     modules::safety::SafetyModule,
//! };
//! use serde_json::json;
//!
//! let module = SafetyModule::default();
//! let ctx = RuleContext::new("send_email", json!({"recipients": 50}));
//! let results = module.evaluate_all(&ctx);
//! for (name, result) in &results {
//!     println!("{name}: {result:?}");
//! }
//! let report = module.audit();
//! println!("completeness: {}%", report.completeness_pct);
//! ```

#![allow(missing_docs)] // TODO: re-enable once praxis API stabilizes

pub mod audit;
pub mod db;
pub mod factory;
pub mod module;
pub mod modules;
pub mod rule;

pub use audit::CompletenessAudit;
pub use factory::RulesFactory;
pub use module::{CompletenessReport, PraxisModule};
pub use rule::{Rule, RuleCategory, RuleContext, RuleResult};
pub mod px;
