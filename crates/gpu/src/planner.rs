//! Capacity planner — recommends an optimal model mix for a given VRAM budget.
//!
//! Given total VRAM and a catalogue of candidate models (name + weight size),
//! [`CapacityPlanner`] computes how many of each model fit alongside the KV-cache
//! reservation and proposes the mix that maximises effective-parameter count.
//!
//! BitNet weight estimates used throughout the code assume 1.58-bit quantisation:
//! roughly **0.25 bytes/parameter**, so an 8 B model → ~2 GB and a 30 B model → ~7.5 GB.

use serde::{Deserialize, Serialize};

// ── ModelSpec ─────────────────────────────────────────────────────────────────

/// Description of a single candidate model for capacity planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    /// Human-readable model identifier (e.g. `"code-8b"`).
    pub name: String,
    /// Number of parameters in millions (e.g. `8_000` for an 8 B model).
    pub params_m: u64,
    /// Weight VRAM footprint in megabytes.
    ///
    /// If `None`, the planner estimates it from `params_m` using the
    /// BitNet heuristic (≈ 0.25 bytes/parameter → 256 MB per 1 B params).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_mb: Option<u64>,
}

impl ModelSpec {
    /// Construct a spec; `weight_mb` is estimated from `params_m` when `None`.
    pub fn new(name: impl Into<String>, params_m: u64) -> Self {
        Self {
            name: name.into(),
            params_m,
            weight_mb: None,
        }
    }

    /// Construct a spec with an explicit VRAM footprint.
    pub fn with_weight_mb(name: impl Into<String>, params_m: u64, weight_mb: u64) -> Self {
        Self {
            name: name.into(),
            params_m,
            weight_mb: Some(weight_mb),
        }
    }

    /// Resolved weight VRAM in MB.
    ///
    /// Uses the explicit `weight_mb` when provided, otherwise estimates via the
    /// BitNet 1.58-bit heuristic: `params_m * 256 / 1_000`.
    pub fn resolved_weight_mb(&self) -> u64 {
        self.weight_mb.unwrap_or_else(|| {
            // 1.58-bit ≈ 0.25 bytes/param → 256 MB per billion params.
            // params_m is in millions, so divide by 1_000 to get billions.
            self.params_m * 256 / 1_000
        })
    }
}

// ── MixEntry ─────────────────────────────────────────────────────────────────

/// A single model in the recommended mix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixEntry {
    /// Model identifier.
    pub name: String,
    /// Number of instances of this model in the recommended mix.
    pub count: usize,
    /// VRAM consumed per instance (MB).
    pub weight_mb: u64,
    /// Total VRAM for all instances (MB).
    pub total_mb: u64,
}

// ── CapacityPlan ──────────────────────────────────────────────────────────────

/// Output of [`CapacityPlanner::recommend_mix`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityPlan {
    /// VRAM budget used for weight allocations (MB).
    pub weight_budget_mb: u64,
    /// VRAM reserved for KV caches (MB).
    pub kv_cache_budget_mb: u64,
    /// Total VRAM available (MB).
    pub total_vram_mb: u64,
    /// Recommended model mix ordered by descending total_mb.
    pub mix: Vec<MixEntry>,
    /// Total effective parameters loaded (millions).
    pub effective_params_m: u64,
    /// VRAM consumed by model weights in the plan (MB).
    pub used_weight_mb: u64,
}

impl CapacityPlan {
    /// Number of distinct models in the plan.
    pub fn model_count(&self) -> usize {
        self.mix.iter().map(|e| e.count).sum()
    }
}

// ── CapacityPlanner ───────────────────────────────────────────────────────────

/// Recommends an optimal model mix for a given VRAM budget.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::planner::{CapacityPlanner, ModelSpec};
///
/// let planner = CapacityPlanner::new(20_480).with_kv_cache_reservation(5_120);
/// let candidates = vec![
///     ModelSpec::new("code-8b", 8_000),
///     ModelSpec::new("chat-8b", 8_000),
///     ModelSpec::new("fast-2b", 2_000),
/// ];
/// let plan = planner.plan(&candidates);
/// println!("fits {} models, {} M eff. params", plan.model_count(), plan.effective_params_m);
/// ```
#[derive(Debug, Clone)]
pub struct CapacityPlanner {
    total_vram_mb: u64,
    kv_cache_reservation_mb: u64,
}

impl CapacityPlanner {
    /// Create a planner for a GPU with `total_vram_mb` megabytes.
    ///
    /// The default KV-cache reservation is 5 GB; override with
    /// [`CapacityPlanner::with_kv_cache_reservation`].
    pub fn new(total_vram_mb: u64) -> Self {
        Self {
            total_vram_mb,
            kv_cache_reservation_mb: 5_120,
        }
    }

    /// Override the KV-cache VRAM reservation.
    #[must_use]
    pub fn with_kv_cache_reservation(mut self, kv_cache_mb: u64) -> Self {
        self.kv_cache_reservation_mb = kv_cache_mb;
        self
    }

    /// VRAM available for model weights after reserving KV-cache space.
    pub fn weight_budget_mb(&self) -> u64 {
        self.total_vram_mb.saturating_sub(self.kv_cache_reservation_mb)
    }

    /// Recommend a model mix that maximises effective-parameter count within
    /// the available weight budget.
    ///
    /// The algorithm greedily fills the budget starting with the largest models
    /// (highest `params_m`).  When a model no longer fits, it skips to the
    /// next smaller one.  This produces a mix that maximises capability per GB.
    pub fn plan(&self, candidates: &[ModelSpec]) -> CapacityPlan {
        let weight_budget = self.weight_budget_mb();

        // Sort descending by params_m (largest first).
        let mut sorted: Vec<&ModelSpec> = candidates.iter().collect();
        sorted.sort_by(|a, b| b.params_m.cmp(&a.params_m));

        let mut mix: Vec<MixEntry> = Vec::new();
        let mut remaining_mb = weight_budget;
        let mut effective_params = 0u64;
        let mut used_weight_mb = 0u64;

        'outer: for spec in &sorted {
            let w = spec.resolved_weight_mb();
            if w == 0 {
                continue;
            }
            loop {
                if remaining_mb < w {
                    continue 'outer;
                }
                remaining_mb -= w;
                used_weight_mb += w;
                effective_params += spec.params_m;

                // Update or create the mix entry for this model.
                if let Some(entry) = mix.iter_mut().find(|e| e.name == spec.name) {
                    entry.count += 1;
                    entry.total_mb += w;
                } else {
                    mix.push(MixEntry {
                        name: spec.name.clone(),
                        count: 1,
                        weight_mb: w,
                        total_mb: w,
                    });
                }
            }
        }

        // Order by descending total_mb for readability.
        mix.sort_by(|a, b| b.total_mb.cmp(&a.total_mb));

        CapacityPlan {
            weight_budget_mb: weight_budget,
            kv_cache_budget_mb: self.kv_cache_reservation_mb,
            total_vram_mb: self.total_vram_mb,
            mix,
            effective_params_m: effective_params,
            used_weight_mb,
        }
    }

    /// Convenience wrapper: build [`ModelSpec`]s from `(name, params_m)` pairs
    /// and call [`CapacityPlanner::plan`].
    pub fn recommend_mix(&self, models: &[(String, u64)]) -> CapacityPlan {
        let specs: Vec<ModelSpec> = models
            .iter()
            .map(|(name, params)| ModelSpec::new(name.clone(), *params))
            .collect();
        self.plan(&specs)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitnet_weight_estimate_for_8b_model() {
        let spec = ModelSpec::new("8b", 8_000);
        // 8_000 * 256 / 1_000 = 2_048 MB ≈ 2 GB
        assert_eq!(spec.resolved_weight_mb(), 2_048);
    }

    #[test]
    fn explicit_weight_mb_overrides_estimate() {
        let spec = ModelSpec::with_weight_mb("custom", 8_000, 1_800);
        assert_eq!(spec.resolved_weight_mb(), 1_800);
    }

    #[test]
    fn planner_weight_budget_subtracts_kv_cache() {
        let planner = CapacityPlanner::new(20_480).with_kv_cache_reservation(5_120);
        assert_eq!(planner.weight_budget_mb(), 15_360);
    }

    #[test]
    fn plan_five_8b_models_fit_in_20gb() {
        // 8B model: 8_000 * 256 / 1_000 = 2_048 MB per instance.
        // Weight budget: 20_480 - 5_120 = 15_360 MB → fits 7 instances (7 × 2_048 = 14_336 MB).
        let planner = CapacityPlanner::new(20_480).with_kv_cache_reservation(5_120);
        let candidates = vec![ModelSpec::new("8b", 8_000)];
        let plan = planner.plan(&candidates);

        assert!(
            plan.model_count() >= 5,
            "expected at least 5 models, got {}",
            plan.model_count()
        );
        assert!(plan.used_weight_mb <= plan.weight_budget_mb);
    }

    #[test]
    fn plan_with_mixed_models() {
        let planner = CapacityPlanner::new(20_480).with_kv_cache_reservation(5_120);
        let candidates = vec![
            ModelSpec::new("30b", 30_000),
            ModelSpec::new("8b", 8_000),
            ModelSpec::new("2b", 2_000),
        ];
        let plan = planner.plan(&candidates);

        // At least one model should be loaded.
        assert!(plan.model_count() > 0);
        assert!(plan.effective_params_m > 0);
        assert!(plan.used_weight_mb <= plan.weight_budget_mb);
    }

    #[test]
    fn plan_with_no_candidates_is_empty() {
        let planner = CapacityPlanner::new(20_480);
        let plan = planner.plan(&[]);
        assert_eq!(plan.model_count(), 0);
        assert_eq!(plan.effective_params_m, 0);
        assert_eq!(plan.used_weight_mb, 0);
    }

    #[test]
    fn plan_with_zero_vram_loads_nothing() {
        let planner = CapacityPlanner::new(0);
        let plan = planner.plan(&[ModelSpec::new("8b", 8_000)]);
        assert_eq!(plan.model_count(), 0);
    }

    #[test]
    fn capacity_plan_model_count_sums_all_counts() {
        let planner = CapacityPlanner::new(20_480).with_kv_cache_reservation(5_120);
        let candidates = vec![ModelSpec::new("2b", 2_000)];
        let plan = planner.plan(&candidates);
        let sum: usize = plan.mix.iter().map(|e| e.count).sum();
        assert_eq!(plan.model_count(), sum);
    }

    #[test]
    fn recommend_mix_matches_plan() {
        let planner = CapacityPlanner::new(10_240).with_kv_cache_reservation(2_048);
        let models = vec![("8b".to_string(), 8_000u64)];
        let plan = planner.recommend_mix(&models);
        assert!(plan.model_count() >= 1);
    }
}
