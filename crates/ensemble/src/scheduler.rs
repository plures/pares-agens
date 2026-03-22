//! CPU+GPU hybrid scheduler.
//!
//! The [`Scheduler`] assigns incoming requests to the appropriate compute
//! tier based on the request's latency requirement and the current pool state.
//!
//! # Assignment rules
//!
//! | Latency class | Target tier preference order |
//! |---------------|------------------------------|
//! | [`LatencyClass::RealTime`] | GPU-hot only |
//! | [`LatencyClass::Interactive`] | GPU-hot → CPU-warm |
//! | [`LatencyClass::Background`] | CPU-warm → cold-storage → GPU-hot (last resort) |
//!
//! The scheduler does **not** load or unload models — that is the
//! responsibility of the runtime layer.  It returns a [`ScheduleDecision`]
//! that tells the runtime which expert to activate and on which tier.

use serde::{Deserialize, Serialize};

use crate::{ComputeTier, EnsembleError, ExpertDomain};

// ── LatencyClass ──────────────────────────────────────────────────────────────

/// The latency requirement of an incoming request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LatencyClass {
    /// Sub-second response required — GPU-hot experts only.
    RealTime,
    /// Acceptable interactive latency — GPU-hot preferred, CPU-warm fallback.
    Interactive,
    /// Background or batch task — CPU-warm preferred, cold-storage fallback.
    Background,
}

// ── ScheduleDecision ──────────────────────────────────────────────────────────

/// The outcome of a scheduling decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleDecision {
    /// The expert chosen to handle the request.
    pub expert_id: String,
    /// The tier this expert is assigned to for this request.
    pub assigned_tier: ComputeTier,
    /// Whether the expert needs to be loaded (true = cold → warm promotion
    /// required before inference).
    pub requires_load: bool,
}

// ── ExpertCandidate ───────────────────────────────────────────────────────────

/// Minimal description of an expert used by the scheduler for tier-aware
/// selection.  This avoids a direct dependency on [`crate::expert::Expert`].
#[derive(Debug, Clone)]
pub struct ExpertCandidate {
    /// Expert identifier.
    pub id: String,
    /// Current compute tier.
    pub tier: ComputeTier,
    /// Domain this expert covers.
    pub domain: ExpertDomain,
}

// ── SchedulerConfig ───────────────────────────────────────────────────────────

/// Configuration for the [`Scheduler`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerConfig {
    /// When `true`, the scheduler will fall back to the next hotter/colder
    /// tier when the preferred tier has no available expert.
    pub allow_tier_fallback: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            allow_tier_fallback: true,
        }
    }
}

// ── Scheduler ─────────────────────────────────────────────────────────────────

/// CPU+GPU hybrid scheduler for the BitNet ensemble.
///
/// # Example
/// ```
/// use pares_agens_ensemble::{ExpertDomain, ComputeTier};
/// use pares_agens_ensemble::scheduler::{ExpertCandidate, LatencyClass, Scheduler, SchedulerConfig};
///
/// let scheduler = Scheduler::new(SchedulerConfig::default());
/// let candidates = vec![
///     ExpertCandidate { id: "c1".into(), tier: ComputeTier::CpuWarm, domain: ExpertDomain::Code },
/// ];
/// let decision = scheduler.schedule(ExpertDomain::Code, LatencyClass::Interactive, &candidates).unwrap();
/// println!("assigned to {} on {:?}", decision.expert_id, decision.assigned_tier);
/// ```
#[derive(Debug, Clone)]
pub struct Scheduler {
    config: SchedulerConfig,
}

impl Scheduler {
    /// Create a new scheduler with the given configuration.
    #[must_use]
    pub fn new(config: SchedulerConfig) -> Self {
        Self { config }
    }

    /// Schedule a request to the best matching expert from `candidates`.
    ///
    /// `candidates` is a slice of experts filtered to the relevant domain by
    /// the caller (typically the [`crate::pool::ExpertPool`]).
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::NoExpertAvailable`] when no candidate matches
    /// the latency requirements (even after fallback, if enabled).
    pub fn schedule(
        &self,
        domain: ExpertDomain,
        latency: LatencyClass,
        candidates: &[ExpertCandidate],
    ) -> Result<ScheduleDecision, EnsembleError> {
        let preferred_tiers = Self::preferred_tiers(latency);

        for &tier in preferred_tiers {
            if let Some(expert) = candidates.iter().find(|c| c.tier == tier) {
                return Ok(ScheduleDecision {
                    expert_id: expert.id.clone(),
                    assigned_tier: tier,
                    requires_load: tier == ComputeTier::ColdStorage,
                });
            }
            if !self.config.allow_tier_fallback {
                break;
            }
        }

        Err(EnsembleError::NoExpertAvailable(domain))
    }

    /// Return the tiers to try in order for a given latency class.
    fn preferred_tiers(latency: LatencyClass) -> &'static [ComputeTier] {
        match latency {
            LatencyClass::RealTime => &[ComputeTier::GpuHot],
            LatencyClass::Interactive => &[ComputeTier::GpuHot, ComputeTier::CpuWarm],
            LatencyClass::Background => {
                &[ComputeTier::CpuWarm, ComputeTier::ColdStorage, ComputeTier::GpuHot]
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler() -> Scheduler {
        Scheduler::new(SchedulerConfig::default())
    }

    fn candidate(id: &str, tier: ComputeTier) -> ExpertCandidate {
        ExpertCandidate {
            id: id.into(),
            tier,
            domain: ExpertDomain::Code,
        }
    }

    // ── Basic scheduling ──────────────────────────────────────────────────

    #[test]
    fn schedule_returns_error_when_no_candidates() {
        let s = scheduler();
        assert!(matches!(
            s.schedule(ExpertDomain::Code, LatencyClass::RealTime, &[]),
            Err(EnsembleError::NoExpertAvailable(_))
        ));
    }

    #[test]
    fn schedule_real_time_selects_gpu_hot() {
        let s = scheduler();
        let candidates = vec![
            candidate("warm", ComputeTier::CpuWarm),
            candidate("hot", ComputeTier::GpuHot),
        ];
        let d = s
            .schedule(ExpertDomain::Code, LatencyClass::RealTime, &candidates)
            .unwrap();
        assert_eq!(d.expert_id, "hot");
        assert_eq!(d.assigned_tier, ComputeTier::GpuHot);
        assert!(!d.requires_load);
    }

    #[test]
    fn schedule_interactive_falls_back_to_cpu_when_no_gpu() {
        let s = scheduler();
        let candidates = vec![candidate("warm", ComputeTier::CpuWarm)];
        let d = s
            .schedule(ExpertDomain::Code, LatencyClass::Interactive, &candidates)
            .unwrap();
        assert_eq!(d.expert_id, "warm");
        assert_eq!(d.assigned_tier, ComputeTier::CpuWarm);
    }

    #[test]
    fn schedule_real_time_fails_when_no_gpu_and_fallback_disabled() {
        let s = Scheduler::new(SchedulerConfig {
            allow_tier_fallback: false,
        });
        let candidates = vec![candidate("warm", ComputeTier::CpuWarm)];
        assert!(s
            .schedule(ExpertDomain::Code, LatencyClass::RealTime, &candidates)
            .is_err());
    }

    #[test]
    fn schedule_background_prefers_cpu_over_gpu() {
        let s = scheduler();
        let candidates = vec![
            candidate("hot", ComputeTier::GpuHot),
            candidate("warm", ComputeTier::CpuWarm),
        ];
        let d = s
            .schedule(ExpertDomain::Code, LatencyClass::Background, &candidates)
            .unwrap();
        assert_eq!(d.expert_id, "warm");
    }

    #[test]
    fn schedule_cold_storage_sets_requires_load() {
        let s = scheduler();
        let candidates = vec![candidate("cold", ComputeTier::ColdStorage)];
        let d = s
            .schedule(ExpertDomain::Code, LatencyClass::Background, &candidates)
            .unwrap();
        assert!(d.requires_load);
    }
}
