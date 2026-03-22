//! Cerebellum gating function — routes queries to the best expert.
//!
//! The [`Gate`] is the mixture-of-experts router.  It maintains per-domain
//! routing weights that are updated after every query via a simple accuracy
//! feedback loop, implementing self-tuning as described in the issue.
//!
//! # Routing algorithm
//!
//! 1. The gate holds a `HashMap<ExpertDomain, Vec<(expert_id, weight)>>`.
//! 2. [`Gate::route`] selects the domain expert with the highest weight for
//!    the given [`ExpertDomain`].
//! 3. After a query, [`Gate::update_weights`] nudges the weight of the chosen
//!    expert up or down proportional to the outcome.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{EnsembleError, ExpertDomain};

// ── RoutingEntry ──────────────────────────────────────────────────────────────

/// A single entry in the routing table: expert ID plus its current weight.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingEntry {
    /// Expert identifier (matches [`crate::expert::Expert::id`]).
    pub expert_id: String,
    /// Routing weight — higher means more likely to be selected.
    pub weight: f32,
}

// ── Gate ──────────────────────────────────────────────────────────────────────

/// Cerebellum gating function for the BitNet MoE ensemble.
///
/// Maintains a per-domain routing table and updates weights based on
/// performance feedback, enabling self-tuning over time.
///
/// # Example
/// ```
/// use pares_agens_ensemble::ExpertDomain;
/// use pares_agens_ensemble::gate::Gate;
///
/// let mut gate = Gate::new(0.05);
/// gate.register_expert(ExpertDomain::Code, "code-expert-1");
/// gate.register_expert(ExpertDomain::Code, "code-expert-2");
///
/// let chosen = gate.route(ExpertDomain::Code).unwrap();
/// println!("routing to {chosen}");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gate {
    /// Per-domain routing table.
    routing_table: HashMap<ExpertDomain, Vec<RoutingEntry>>,
    /// Step size for weight updates — controls how quickly routing adapts.
    /// Must be in `(0.0, 1.0]`.
    learning_rate: f32,
}

impl Gate {
    /// Create a new gate with an empty routing table.
    ///
    /// `learning_rate` is the step size for weight updates; 0.05 is a
    /// reasonable starting point for most workloads.
    #[must_use]
    pub fn new(learning_rate: f32) -> Self {
        Self {
            routing_table: HashMap::new(),
            learning_rate,
        }
    }

    /// Register a new expert under the given domain.
    ///
    /// Newly registered experts start with weight `1.0` so they are eligible
    /// for routing immediately.
    pub fn register_expert(&mut self, domain: ExpertDomain, expert_id: &str) {
        let entries = self.routing_table.entry(domain).or_default();
        // Avoid duplicates.
        if entries.iter().any(|e| e.expert_id == expert_id) {
            return;
        }
        entries.push(RoutingEntry {
            expert_id: expert_id.to_string(),
            weight: 1.0,
        });
    }

    /// Remove an expert from the routing table.
    ///
    /// Returns `true` if the expert was found and removed.
    pub fn deregister_expert(&mut self, domain: ExpertDomain, expert_id: &str) -> bool {
        if let Some(entries) = self.routing_table.get_mut(&domain) {
            let before = entries.len();
            entries.retain(|e| e.expert_id != expert_id);
            return entries.len() < before;
        }
        false
    }

    /// Select the best expert ID for the given domain.
    ///
    /// Returns the `expert_id` with the highest weight.  Ties are broken by
    /// position (first registered wins), which is deterministic.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::NoExpertAvailable`] when no expert is
    /// registered for `domain`.
    pub fn route(&self, domain: ExpertDomain) -> Result<String, EnsembleError> {
        let entries = self
            .routing_table
            .get(&domain)
            .ok_or(EnsembleError::NoExpertAvailable(domain))?;

        entries
            .iter()
            .max_by(|a, b| {
                a.weight
                    .partial_cmp(&b.weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|e| e.expert_id.clone())
            .ok_or(EnsembleError::NoExpertAvailable(domain))
    }

    /// Update the routing weight for `expert_id` in `domain` based on
    /// `outcome` (1.0 = correct, 0.0 = incorrect).
    ///
    /// The weight is nudged by `learning_rate * (outcome - 0.5)`, clamped to
    /// `[0.01, 10.0]` to prevent runaway or dead weights.
    ///
    /// This is the self-tuning feedback loop: accurate experts gain weight and
    /// are routed more frequently; inaccurate experts lose weight.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::ExpertNotFound`] if the expert is not in the
    /// routing table for `domain`.
    pub fn update_weights(
        &mut self,
        domain: ExpertDomain,
        expert_id: &str,
        outcome: f32,
    ) -> Result<(), EnsembleError> {
        let entries = self
            .routing_table
            .get_mut(&domain)
            .ok_or_else(|| EnsembleError::ExpertNotFound(expert_id.to_string()))?;

        let entry = entries
            .iter_mut()
            .find(|e| e.expert_id == expert_id)
            .ok_or_else(|| EnsembleError::ExpertNotFound(expert_id.to_string()))?;

        let delta = self.learning_rate * (outcome - 0.5);
        entry.weight = (entry.weight + delta).clamp(0.01, 10.0);
        Ok(())
    }

    /// Return all routing entries for `domain`, or an empty slice if none.
    #[must_use]
    pub fn entries_for_domain(&self, domain: ExpertDomain) -> &[RoutingEntry] {
        self.routing_table
            .get(&domain)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Return the current weight of `expert_id` in `domain`, or `None`.
    #[must_use]
    pub fn weight_of(&self, domain: ExpertDomain, expert_id: &str) -> Option<f32> {
        self.routing_table.get(&domain).and_then(|entries| {
            entries
                .iter()
                .find(|e| e.expert_id == expert_id)
                .map(|e| e.weight)
        })
    }

    /// Return `true` when no experts are registered for any domain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.routing_table.values().all(Vec::is_empty)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_gate() -> Gate {
        Gate::new(0.05)
    }

    #[test]
    fn new_gate_is_empty() {
        assert!(make_gate().is_empty());
    }

    #[test]
    fn register_expert_adds_to_table() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        assert_eq!(g.entries_for_domain(ExpertDomain::Code).len(), 1);
    }

    #[test]
    fn register_expert_is_idempotent() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        g.register_expert(ExpertDomain::Code, "e1");
        assert_eq!(g.entries_for_domain(ExpertDomain::Code).len(), 1);
    }

    #[test]
    fn deregister_expert_removes_entry() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        assert!(g.deregister_expert(ExpertDomain::Code, "e1"));
        assert!(g.entries_for_domain(ExpertDomain::Code).is_empty());
    }

    #[test]
    fn deregister_missing_expert_returns_false() {
        let mut g = make_gate();
        assert!(!g.deregister_expert(ExpertDomain::Code, "ghost"));
    }

    #[test]
    fn route_returns_error_when_no_experts() {
        let g = make_gate();
        assert!(matches!(
            g.route(ExpertDomain::Code),
            Err(EnsembleError::NoExpertAvailable(_))
        ));
    }

    #[test]
    fn route_returns_single_expert_when_only_one() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        assert_eq!(g.route(ExpertDomain::Code).unwrap(), "e1");
    }

    #[test]
    fn route_returns_highest_weight_expert() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Math, "m1");
        g.register_expert(ExpertDomain::Math, "m2");
        // Boost m2 weight.
        g.update_weights(ExpertDomain::Math, "m2", 1.0).unwrap();
        g.update_weights(ExpertDomain::Math, "m2", 1.0).unwrap();
        assert_eq!(g.route(ExpertDomain::Math).unwrap(), "m2");
    }

    #[test]
    fn update_weights_increases_weight_on_good_outcome() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        let before = g.weight_of(ExpertDomain::Code, "e1").unwrap();
        g.update_weights(ExpertDomain::Code, "e1", 1.0).unwrap();
        let after = g.weight_of(ExpertDomain::Code, "e1").unwrap();
        assert!(after > before);
    }

    #[test]
    fn update_weights_decreases_weight_on_bad_outcome() {
        let mut g = make_gate();
        g.register_expert(ExpertDomain::Code, "e1");
        let before = g.weight_of(ExpertDomain::Code, "e1").unwrap();
        g.update_weights(ExpertDomain::Code, "e1", 0.0).unwrap();
        let after = g.weight_of(ExpertDomain::Code, "e1").unwrap();
        assert!(after < before);
    }

    #[test]
    fn update_weights_clamps_to_minimum() {
        let mut g = Gate::new(1.0); // large learning rate
        g.register_expert(ExpertDomain::Code, "e1");
        // Many bad outcomes should clamp at 0.01.
        for _ in 0..100 {
            g.update_weights(ExpertDomain::Code, "e1", 0.0).unwrap();
        }
        assert!(g.weight_of(ExpertDomain::Code, "e1").unwrap() >= 0.01);
    }

    #[test]
    fn update_weights_clamps_to_maximum() {
        let mut g = Gate::new(1.0);
        g.register_expert(ExpertDomain::Code, "e1");
        for _ in 0..100 {
            g.update_weights(ExpertDomain::Code, "e1", 1.0).unwrap();
        }
        assert!(g.weight_of(ExpertDomain::Code, "e1").unwrap() <= 10.0);
    }

    #[test]
    fn update_weights_returns_error_for_unknown_expert() {
        let mut g = make_gate();
        assert!(matches!(
            g.update_weights(ExpertDomain::Code, "ghost", 1.0),
            Err(EnsembleError::ExpertNotFound(_))
        ));
    }
}
