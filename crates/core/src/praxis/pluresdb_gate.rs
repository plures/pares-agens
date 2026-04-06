//! PluresDB-backed [`PraxisGate`] implementation.
//!
//! Replaces the in-memory [`DefaultPraxisGate`] with one that evaluates
//! constraints stored in PluresDB via the native procedure engine,
//! merged with the built-in seed constraints.

use std::sync::Arc;

use tracing::warn;

use crate::cerebellum::bridge::PluresDbBridge;
use crate::executor::PraxisGate;
use pares_agens_praxis::db::{
    procedures::on_action,
    schema::{AgentContext, SessionType},
    store::PraxisStore,
};

/// A [`PraxisGate`] backed by PluresDB with in-memory fallback.
///
/// On each `check()` call:
/// 1. Loads constraints from PluresDB and merges with built-in seed constraints
/// 2. Falls back to the seed store alone if PluresDB is unavailable
///
/// This ensures the praxis gate never silently allows actions when the
/// DB connection drops — it degrades to the safe seed constraints.
pub struct PluresDbPraxisGate {
    /// PluresDB bridge for native procedure execution
    bridge: Arc<PluresDbBridge>,
}

impl PluresDbPraxisGate {
    /// Create a new gate with a PluresDB bridge.
    pub fn new(bridge: Arc<PluresDbBridge>) -> Self {
        Self { bridge }
    }

    /// Build a [`PraxisStore`] by merging seed constraints with any stored
    /// in PluresDB.
    fn build_store(&self) -> PraxisStore {
        let mut store = pares_agens_praxis::db::seed::default_store();

        match self.bridge.load_constraints() {
            Ok(constraints) => {
                for constraint in constraints {
                    store.upsert_constraint(constraint);
                }
            }
            Err(e) => {
                warn!("failed to load praxis constraints from PluresDB, using seed only: {e}");
            }
        }

        store
    }
}

impl PraxisGate for PluresDbPraxisGate {
    fn check(&self, action: &str) -> Result<(), String> {
        let store = self.build_store();
        let ctx = AgentContext::new(action, "", SessionType::Main);
        on_action(&store, &ctx)
            .map(|_| ())
            .map_err(|blocked| blocked.to_string())
    }
}
