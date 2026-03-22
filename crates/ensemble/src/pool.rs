//! Expert pool — hot / warm / cold tier management with graduation logic.
//!
//! [`ExpertPool`] owns all registered experts and enforces the three-tier
//! capacity model described in the crate-level documentation.  It supports:
//!
//! * **Registration** — add an expert to cold storage.
//! * **Tier queries** — list experts by tier or domain.
//! * **Expert graduation** — automatically promote a CPU-warm expert to
//!   GPU-hot when the hot pool has space and the expert meets the accuracy
//!   threshold defined in [`crate::EnsembleConfig`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    expert::Expert, ComputeTier, EnsembleConfig, EnsembleError, ExpertDomain, ExpertMetrics,
};

// ── PoolConfig ────────────────────────────────────────────────────────────────

/// Capacity limits extracted from [`EnsembleConfig`] for use inside the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolConfig {
    /// Maximum number of GPU-hot experts.
    pub gpu_hot_capacity: usize,
    /// Maximum number of CPU-warm experts.
    pub cpu_warm_capacity: usize,
    /// EMA alpha forwarded from [`EnsembleConfig`].
    pub ema_alpha: f32,
    /// Minimum query count for graduation eligibility.
    pub graduation_min_queries: u64,
    /// Minimum accuracy for graduation eligibility.
    pub graduation_min_accuracy: f32,
}

impl From<&EnsembleConfig> for PoolConfig {
    fn from(cfg: &EnsembleConfig) -> Self {
        Self {
            gpu_hot_capacity: cfg.gpu_hot_capacity,
            cpu_warm_capacity: cfg.cpu_warm_capacity,
            ema_alpha: cfg.ema_alpha,
            graduation_min_queries: cfg.graduation_min_queries,
            graduation_min_accuracy: cfg.graduation_min_accuracy,
        }
    }
}

// ── ExpertPool ────────────────────────────────────────────────────────────────

/// Manages the full population of BitNet experts across all three compute
/// tiers.
///
/// Internally all experts are stored in a single `HashMap<id, Expert>` for O(1)
/// lookup; tier membership is tracked on the [`Expert::tier`] field.
///
/// # Example
/// ```
/// use pares_agens_ensemble::{EnsembleConfig, ExpertDomain, ComputeTier};
/// use pares_agens_ensemble::expert::Expert;
/// use pares_agens_ensemble::pool::ExpertPool;
///
/// let cfg = EnsembleConfig::consumer_hardware();
/// let mut pool = ExpertPool::new(&cfg);
///
/// let expert = Expert::new("e1", ExpertDomain::Code, "models/code.gguf", 8.0);
/// pool.register(expert).unwrap();
/// assert_eq!(pool.experts_in_tier(ComputeTier::ColdStorage).len(), 1);
/// ```
#[derive(Debug)]
pub struct ExpertPool {
    experts: HashMap<String, Expert>,
    config: PoolConfig,
}

impl ExpertPool {
    /// Create an empty pool with the given configuration.
    #[must_use]
    pub fn new(cfg: &EnsembleConfig) -> Self {
        Self {
            experts: HashMap::new(),
            config: PoolConfig::from(cfg),
        }
    }

    /// Register a new expert.  The expert's `id` must be unique within the
    /// pool; it is always placed in cold storage on registration.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::ExpertNotFound`] if the expert fails its own
    /// validation, or if an expert with the same `id` already exists.
    pub fn register(&mut self, mut expert: Expert) -> Result<(), EnsembleError> {
        expert.validate()?;
        if self.experts.contains_key(&expert.id) {
            return Err(EnsembleError::InvalidPoolConfig(format!(
                "expert with id '{}' is already registered",
                expert.id
            )));
        }
        expert.tier = ComputeTier::ColdStorage;
        self.experts.insert(expert.id.clone(), expert);
        Ok(())
    }

    /// Return a reference to the expert with `id`, or an error if not found.
    pub fn get(&self, id: &str) -> Result<&Expert, EnsembleError> {
        self.experts
            .get(id)
            .ok_or_else(|| EnsembleError::ExpertNotFound(id.to_string()))
    }

    /// Return a mutable reference to the expert with `id`.
    pub fn get_mut(&mut self, id: &str) -> Result<&mut Expert, EnsembleError> {
        self.experts
            .get_mut(id)
            .ok_or_else(|| EnsembleError::ExpertNotFound(id.to_string()))
    }

    /// List all experts currently in `tier`.
    #[must_use]
    pub fn experts_in_tier(&self, tier: ComputeTier) -> Vec<&Expert> {
        self.experts
            .values()
            .filter(|e| e.tier == tier)
            .collect()
    }

    /// List all experts registered for `domain`, regardless of tier.
    #[must_use]
    pub fn experts_for_domain(&self, domain: ExpertDomain) -> Vec<&Expert> {
        self.experts
            .values()
            .filter(|e| e.domain == domain)
            .collect()
    }

    /// Return the best available expert for `domain`, preferring hotter tiers.
    ///
    /// Selection priority: GpuHot → CpuWarm → ColdStorage.  Within a tier the
    /// expert with the highest accuracy is chosen.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::NoExpertAvailable`] when no expert exists for
    /// the domain.
    pub fn best_for_domain(&self, domain: ExpertDomain) -> Result<&Expert, EnsembleError> {
        let candidates: Vec<&Expert> = self
            .experts
            .values()
            .filter(|e| e.domain == domain || e.domain == ExpertDomain::General)
            .collect();

        if candidates.is_empty() {
            return Err(EnsembleError::NoExpertAvailable(domain));
        }

        // Prefer exact domain match, then by tier (lower = hotter), then by accuracy.
        candidates
            .into_iter()
            .min_by(|a, b| {
                let domain_rank = |e: &&Expert| {
                    if e.domain == domain { 0u8 } else { 1u8 }
                };
                domain_rank(a)
                    .cmp(&domain_rank(b))
                    .then(a.tier.cmp(&b.tier))
                    .then(
                        b.metrics
                            .accuracy
                            .partial_cmp(&a.metrics.accuracy)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
            })
            .ok_or(EnsembleError::NoExpertAvailable(domain))
    }

    /// Record the outcome of a query for the expert with `id`.
    ///
    /// Delegates to [`Expert::record_outcome`] using the pool's configured EMA
    /// alpha.
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::ExpertNotFound`] if no expert with `id` exists.
    pub fn record_outcome(
        &mut self,
        id: &str,
        outcome: f32,
        latency_ms: f64,
    ) -> Result<(), EnsembleError> {
        let alpha = self.config.ema_alpha;
        let expert = self.get_mut(id)?;
        expert.record_outcome(outcome, latency_ms, alpha);
        Ok(())
    }

    /// Promote eligible CPU-warm experts to the GPU-hot pool.
    ///
    /// An expert is eligible when:
    /// 1. It currently occupies [`ComputeTier::CpuWarm`].
    /// 2. The GPU-hot pool has fewer than `gpu_hot_capacity` experts.
    /// 3. `metrics.queries_handled >= graduation_min_queries`.
    /// 4. `metrics.accuracy >= graduation_min_accuracy`.
    ///
    /// Returns the IDs of all promoted experts.
    pub fn graduate_experts(&mut self) -> Vec<String> {
        let min_queries = self.config.graduation_min_queries;
        let min_accuracy = self.config.graduation_min_accuracy;
        let gpu_capacity = self.config.gpu_hot_capacity;

        // Collect candidates first to avoid borrow issues.
        let mut candidates: Vec<String> = self
            .experts
            .values()
            .filter(|e| {
                e.tier == ComputeTier::CpuWarm
                    && e.metrics.queries_handled >= min_queries
                    && e.metrics.accuracy >= min_accuracy
            })
            .map(|e| e.id.clone())
            .collect();

        // Sort by accuracy descending so the best experts are promoted first.
        candidates.sort_by(|a, b| {
            let acc_a = self.experts[a].metrics.accuracy;
            let acc_b = self.experts[b].metrics.accuracy;
            acc_b.partial_cmp(&acc_a).unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut promoted = Vec::new();
        for id in candidates {
            let hot_count = self
                .experts
                .values()
                .filter(|e| e.tier == ComputeTier::GpuHot)
                .count();
            if hot_count >= gpu_capacity {
                break;
            }
            if let Some(expert) = self.experts.get_mut(&id) {
                expert.tier = ComputeTier::GpuHot;
                promoted.push(id);
            }
        }
        promoted
    }

    /// Return the total number of registered experts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.experts.len()
    }

    /// Return `true` when no experts are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.experts.is_empty()
    }

    /// Return a snapshot of all expert metrics keyed by expert ID.
    #[must_use]
    pub fn metrics_snapshot(&self) -> HashMap<String, ExpertMetrics> {
        self.experts
            .iter()
            .map(|(id, e)| (id.clone(), e.metrics.clone()))
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EnsembleConfig {
        EnsembleConfig::consumer_hardware()
    }

    fn make_pool() -> ExpertPool {
        ExpertPool::new(&cfg())
    }

    fn expert(id: &str, domain: ExpertDomain) -> Expert {
        Expert::new(id, domain, "models/test.gguf", 8.0)
    }

    // ── Registration ──────────────────────────────────────────────────────

    #[test]
    fn register_adds_to_cold_storage() {
        let mut pool = make_pool();
        pool.register(expert("e1", ExpertDomain::Code)).unwrap();
        assert_eq!(pool.experts_in_tier(ComputeTier::ColdStorage).len(), 1);
    }

    #[test]
    fn register_rejects_duplicate_id() {
        let mut pool = make_pool();
        pool.register(expert("e1", ExpertDomain::Code)).unwrap();
        assert!(matches!(
            pool.register(expert("e1", ExpertDomain::Math)),
            Err(EnsembleError::InvalidPoolConfig(_))
        ));
    }

    // ── Lookup ────────────────────────────────────────────────────────────

    #[test]
    fn get_returns_expert_by_id() {
        let mut pool = make_pool();
        pool.register(expert("e1", ExpertDomain::Code)).unwrap();
        assert!(pool.get("e1").is_ok());
    }

    #[test]
    fn get_returns_error_for_unknown_id() {
        let pool = make_pool();
        assert!(matches!(pool.get("missing"), Err(EnsembleError::ExpertNotFound(_))));
    }

    // ── Domain queries ────────────────────────────────────────────────────

    #[test]
    fn best_for_domain_returns_error_when_empty() {
        let pool = make_pool();
        assert!(matches!(
            pool.best_for_domain(ExpertDomain::Code),
            Err(EnsembleError::NoExpertAvailable(_))
        ));
    }

    #[test]
    fn best_for_domain_returns_general_expert_as_fallback() {
        let mut pool = make_pool();
        pool.register(expert("g1", ExpertDomain::General)).unwrap();
        // No code expert — should fall back to general.
        let e = pool.best_for_domain(ExpertDomain::Code).unwrap();
        assert_eq!(e.domain, ExpertDomain::General);
    }

    #[test]
    fn best_for_domain_prefers_exact_domain_match() {
        let mut pool = make_pool();
        pool.register(expert("g1", ExpertDomain::General)).unwrap();
        pool.register(expert("c1", ExpertDomain::Code)).unwrap();
        let e = pool.best_for_domain(ExpertDomain::Code).unwrap();
        assert_eq!(e.id, "c1");
    }

    #[test]
    fn best_for_domain_prefers_hotter_tier() {
        let mut pool = make_pool();
        let mut warm = expert("c1", ExpertDomain::Code);
        warm.tier = ComputeTier::CpuWarm;
        let mut cold = expert("c2", ExpertDomain::Code);
        cold.tier = ComputeTier::ColdStorage;
        pool.experts.insert("c1".into(), warm);
        pool.experts.insert("c2".into(), cold);
        let best = pool.best_for_domain(ExpertDomain::Code).unwrap();
        assert_eq!(best.id, "c1");
    }

    // ── Record outcome ────────────────────────────────────────────────────

    #[test]
    fn record_outcome_updates_metrics() {
        let mut pool = make_pool();
        pool.register(expert("e1", ExpertDomain::Code)).unwrap();
        pool.record_outcome("e1", 1.0, 100.0).unwrap();
        let m = &pool.get("e1").unwrap().metrics;
        assert_eq!(m.queries_handled, 1);
    }

    // ── Graduation ────────────────────────────────────────────────────────

    #[test]
    fn graduate_experts_promotes_eligible_warm_expert() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.graduation_min_queries = 1;
        cfg.graduation_min_accuracy = 0.6;
        let mut pool = ExpertPool::new(&cfg);

        let mut e = expert("e1", ExpertDomain::Code);
        e.tier = ComputeTier::CpuWarm;
        e.metrics.accuracy = 0.9;
        e.metrics.queries_handled = 5;
        pool.experts.insert("e1".into(), e);

        let promoted = pool.graduate_experts();
        assert_eq!(promoted, vec!["e1"]);
        assert_eq!(pool.get("e1").unwrap().tier, ComputeTier::GpuHot);
    }

    #[test]
    fn graduate_experts_skips_expert_with_insufficient_queries() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.graduation_min_queries = 100;
        let mut pool = ExpertPool::new(&cfg);

        let mut e = expert("e1", ExpertDomain::Code);
        e.tier = ComputeTier::CpuWarm;
        e.metrics.accuracy = 0.95;
        e.metrics.queries_handled = 5; // below threshold
        pool.experts.insert("e1".into(), e);

        assert!(pool.graduate_experts().is_empty());
    }

    #[test]
    fn graduate_experts_respects_gpu_capacity() {
        let mut cfg = EnsembleConfig::consumer_hardware();
        cfg.gpu_hot_capacity = 1;
        cfg.graduation_min_queries = 1;
        cfg.graduation_min_accuracy = 0.0;
        let mut pool = ExpertPool::new(&cfg);

        for i in 0..3usize {
            let mut e = expert(&format!("e{i}"), ExpertDomain::Code);
            e.tier = ComputeTier::CpuWarm;
            e.metrics.accuracy = 0.9;
            e.metrics.queries_handled = 10;
            pool.experts.insert(format!("e{i}"), e);
        }

        let promoted = pool.graduate_experts();
        assert_eq!(promoted.len(), 1);
        let hot_count = pool.experts_in_tier(ComputeTier::GpuHot).len();
        assert_eq!(hot_count, 1);
    }

    // ── Metrics snapshot ──────────────────────────────────────────────────

    #[test]
    fn metrics_snapshot_returns_all_entries() {
        let mut pool = make_pool();
        pool.register(expert("e1", ExpertDomain::Code)).unwrap();
        pool.register(expert("e2", ExpertDomain::Math)).unwrap();
        let snap = pool.metrics_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains_key("e1"));
        assert!(snap.contains_key("e2"));
    }
}
