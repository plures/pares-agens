//! Built-in procedure pipelines for the cerebellum.
//!
//! These are the standard procedures that ship with pares-agens:
//!
//! - **autorecall** — retrieve + compress memories before agent execution
//! - **primitive-extract** — extract typed primitives (decisions, facts, risks)
//!   from a conversation exchange
//! - **cerebellum-sweep** — periodic background maintenance (prune stale,
//!   consolidate duplicates, update ledger)

use async_trait::async_trait;
use tracing::debug;

use crate::event::Event;
use crate::procedure::Procedure;

// ── autorecall ───────────────────────────────────────────────────────────────

/// Autorecall procedure — retrieves and compresses memories into learned
/// context before the conscious agent runs.
///
/// In the full system this delegates to PluresDB's VectorSearch → Transform
/// pipeline. This implementation provides the procedure interface; the actual
/// memory operations are performed by the cerebellum's `preprocess` method.
pub struct Autorecall;

#[async_trait]
impl Procedure for Autorecall {
    fn name(&self) -> &str {
        "autorecall"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        debug!(event_kind = event.kind(), "autorecall: triggered");
        // In production, this runs:
        //   VectorSearch(query) → Filter(min_score > 0.3) → Transform(toon) → Emit("context")
        // For now, the cerebellum.preprocess() handles this directly.
        // This stub allows registration + priority ordering.
        vec![]
    }
}

// ── primitive extraction ─────────────────────────────────────────────────────

/// Primitive extraction procedure — runs after capture to extract typed
/// primitives (decisions, facts, risks, preferences) from conversation.
///
/// This is the pares-agens equivalent of pluresLM issue #107.
pub struct PrimitiveExtract;

#[async_trait]
impl Procedure for PrimitiveExtract {
    fn name(&self) -> &str {
        "primitive-extract"
    }

    fn handles(&self) -> &str {
        "state_change"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        debug!(event_kind = event.kind(), "primitive-extract: triggered");
        // In production, this runs after a memory capture (state_change on memory store):
        //   1. Read the new memory content
        //   2. Classify into primitive types (decision, fact, risk, preference, entity)
        //   3. Store typed primitives as separate nodes with category tags
        //   4. Link primitives to source memory via graph edges
        //
        // Requires agent_invoke step for LLM classification — Phase 1 dependency.
        vec![]
    }
}

// ── cerebellum sweep ─────────────────────────────────────────────────────────

/// Periodic maintenance sweep — runs on timer events.
///
/// Tasks:
/// - Prune stale memories (not accessed in N days)
/// - Consolidate near-duplicate memories
/// - Update praxis ledger with new patterns
/// - Recompute graph statistics for routing optimization
pub struct CerebellumSweep;

#[async_trait]
impl Procedure for CerebellumSweep {
    fn name(&self) -> &str {
        "cerebellum-sweep"
    }

    fn handles(&self) -> &str {
        "timer"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        debug!(event_kind = event.kind(), "cerebellum-sweep: triggered");
        // In production, this runs:
        //   1. GraphStats → identify clusters and orphans
        //   2. Filter(accessed_before: now - 30d) → mark stale
        //   3. AutoLink(cosine, threshold: 0.85) → merge near-duplicates
        //   4. Emit("sweep_report") → log results
        //
        // Fully procedural — no LLM needed.
        vec![]
    }
}

/// Register all built-in cerebellum procedures into a registry.
pub fn register_builtins(registry: &mut crate::procedure::ProcedureRegistry) {
    // Autorecall runs first (lowest priority number)
    registry.register(Box::new(Autorecall));
    registry.set_priority("autorecall", -100);

    // Primitive extraction runs on state changes
    registry.register(Box::new(PrimitiveExtract));
    registry.set_priority("primitive-extract", 0);

    // Sweep runs on timers
    registry.register(Box::new(CerebellumSweep));
    registry.set_priority("cerebellum-sweep", 0);

    // Cerebellum itself handles messages (orchestrator)
    registry.register(Box::new(super::CerebellumProcedure));
    registry.set_priority("cerebellum", -200); // runs before autorecall
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procedure::ProcedureRegistry;

    #[test]
    fn register_builtins_adds_four_procedures() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry);
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn cerebellum_has_highest_priority_for_messages() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry);

        let message_procs: Vec<&str> = registry
            .matching("message")
            .map(|p| p.name())
            .collect();

        // cerebellum should come before autorecall
        assert_eq!(message_procs, vec!["cerebellum", "autorecall"]);
    }

    #[test]
    fn sweep_handles_timer_events() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry);

        let timer_procs: Vec<&str> = registry
            .matching("timer")
            .map(|p| p.name())
            .collect();

        assert_eq!(timer_procs, vec!["cerebellum-sweep"]);
    }
}
