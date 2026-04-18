//! Capacity planner — recommend optimal model mix for a given VRAM budget.
//!
//! [`CapacityPlanner`] takes a list of [`ModelSpec`] candidates and fills the
//! available VRAM budget (minus KV cache headroom) with as many models as
//! possible, preferring larger models first (greedy descending by VRAM).

/// Describes a model that may be loaded onto the GPU.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// A short, stable identifier (e.g. `"code-8b"`).
    pub model_id: String,

    /// VRAM footprint of the model weights, in MiB.
    pub vram_mb: u64,

    /// Approximate number of parameters (used for reporting only), in billions.
    pub params_b: f64,
}

impl ModelSpec {
    /// Convenience constructor.
    pub fn new(model_id: impl Into<String>, vram_mb: u64, params_b: f64) -> Self {
        Self {
            model_id: model_id.into(),
            vram_mb,
            params_b,
        }
    }
}

/// The allocation recommended by [`CapacityPlanner::plan`].
#[derive(Debug, Clone)]
pub struct PlannedAllocation {
    /// The models selected to fill the available VRAM.
    pub models: Vec<ModelSpec>,

    /// Total VRAM consumed by weights in this plan, in MiB.
    pub total_vram_mb: u64,

    /// Sum of parameter counts across all selected models, in billions.
    pub effective_params_b: f64,
}

/// Recommends an optimal model mix for a given VRAM budget.
///
/// # Algorithm
///
/// Uses a greedy descending strategy: sort candidates by VRAM (largest first),
/// then pick models until the weight budget is exhausted or `max_models` is
/// reached.  This maximises `effective_params_b` for a fixed VRAM envelope.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::planner::{CapacityPlanner, ModelSpec};
///
/// let planner = CapacityPlanner::new(20_000, 4_096, 5);
/// let candidates = vec![
///     ModelSpec::new("code-8b",   2_000, 8.0),
///     ModelSpec::new("chat-8b",   2_000, 8.0),
///     ModelSpec::new("fast-2b",     500, 2.0),
///     ModelSpec::new("large-30b", 8_000, 30.0),
/// ];
///
/// let plan = planner.plan(&candidates);
/// // large-30b + code-8b + chat-8b + fast-2b = 12_500 MB ≤ 15_904 MB (20_000 − 4_096)
/// assert!(plan.models.len() >= 3);
/// ```
#[derive(Debug, Clone)]
pub struct CapacityPlanner {
    /// Total VRAM budget, in MiB.
    vram_budget_mb: u64,
    /// VRAM reserved for the KV cache, in MiB.
    kv_cache_mb: u64,
    /// Maximum number of models allowed.
    max_models: usize,
}

impl CapacityPlanner {
    /// Create a planner with the given budget, KV cache reservation, and model cap.
    pub fn new(vram_budget_mb: u64, kv_cache_mb: u64, max_models: usize) -> Self {
        Self {
            vram_budget_mb,
            kv_cache_mb,
            max_models,
        }
    }

    /// VRAM available for model weights (budget minus KV cache headroom), in MiB.
    pub fn weight_budget_mb(&self) -> u64 {
        self.vram_budget_mb.saturating_sub(self.kv_cache_mb)
    }

    /// Recommend an optimal model mix from `candidates`.
    ///
    /// Returns a [`PlannedAllocation`] that fits within the weight budget and
    /// `max_models` limit.
    pub fn plan(&self, candidates: &[ModelSpec]) -> PlannedAllocation {
        let weight_budget = self.weight_budget_mb();

        // Sort descending by VRAM footprint (greedy largest-first).
        let mut sorted = candidates.to_vec();
        sorted.sort_by_key(|s| std::cmp::Reverse(s.vram_mb));

        let mut selected: Vec<ModelSpec> = Vec::new();
        let mut used_mb: u64 = 0;

        for spec in sorted {
            if selected.len() >= self.max_models {
                break;
            }
            if used_mb.saturating_add(spec.vram_mb) <= weight_budget {
                used_mb += spec.vram_mb;
                selected.push(spec);
            }
        }

        let effective_params_b = selected.iter().map(|s| s.params_b).sum();

        PlannedAllocation {
            models: selected,
            total_vram_mb: used_mb,
            effective_params_b,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<ModelSpec> {
        vec![
            ModelSpec::new("code-8b", 2_000, 8.0),
            ModelSpec::new("chat-8b", 2_000, 8.0),
            ModelSpec::new("fast-2b", 500, 2.0),
            ModelSpec::new("large-30b", 8_000, 30.0),
        ]
    }

    #[test]
    fn plan_fits_within_budget() {
        let planner = CapacityPlanner::new(20_000, 4_096, 5);
        let plan = planner.plan(&candidates());

        assert!(plan.total_vram_mb <= planner.weight_budget_mb());
        assert!(plan.effective_params_b > 0.0);
    }

    #[test]
    fn plan_respects_max_models() {
        let planner = CapacityPlanner::new(20_000, 4_096, 2);
        let plan = planner.plan(&candidates());

        assert!(plan.models.len() <= 2);
    }

    #[test]
    fn plan_selects_largest_first() {
        let planner = CapacityPlanner::new(20_000, 4_096, 5);
        let plan = planner.plan(&candidates());

        // large-30b (8_000 MB) should be first because it is the largest.
        assert_eq!(plan.models[0].model_id, "large-30b");
    }

    #[test]
    fn plan_skips_model_that_does_not_fit() {
        // Budget is only 3_000 MB for weights — large-30b (8_000) should be skipped.
        let planner = CapacityPlanner::new(7_096, 4_096, 5);
        let plan = planner.plan(&candidates());

        assert!(!plan.models.iter().any(|m| m.model_id == "large-30b"));
    }
}
