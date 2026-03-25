//! Device capacity profiles and storage tiers.
//!
//! Each node in the neural mesh advertises its available storage budget so
//! peers can make informed eviction and prefetch decisions.

use serde::{Deserialize, Serialize};

// ── StorageTier ───────────────────────────────────────────────────────────────

/// Which memories a device actively stores locally.
///
/// | Tier   | Typical device         | What's kept locally               |
/// |--------|------------------------|-----------------------------------|
/// | `Hot`  | Phone / constrained    | Recent 7 days + pinned            |
/// | `Warm` | Laptop / mid-range     | Recent 90 days + active projects  |
/// | `Full` | Desktop / server       | All memories, all indexes         |
/// | `Custom` | Any                  | User-defined recency window       |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageTier {
    /// Only very recent memories (default: 7 days) plus pinned entries.
    Hot,
    /// Recent memories plus active project context (default: 90 days).
    Warm,
    /// All memories, all indexes, full embeddings — no eviction under budget.
    Full,
    /// Caller-specified recency window in days.
    Custom { max_age_days: u32 },
}

impl StorageTier {
    /// The maximum age in days that this tier retains unpinned memories.
    ///
    /// Returns `None` for [`StorageTier::Full`] (no age limit).
    #[must_use]
    pub fn max_age_days(&self) -> Option<u32> {
        match self {
            Self::Hot => Some(7),
            Self::Warm => Some(90),
            Self::Full => None,
            Self::Custom { max_age_days } => Some(*max_age_days),
        }
    }
}

// ── StorageBudget ─────────────────────────────────────────────────────────────

/// The amount of disk storage this device dedicates to memory caching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageBudget {
    /// Total capacity allocated to the memory cache, in bytes.
    pub total_bytes: u64,
    /// Maximum bytes used by the hot tier (most recent / pinned).
    pub hot_bytes: u64,
    /// Maximum bytes used by the warm tier.
    pub warm_bytes: u64,
}

impl StorageBudget {
    /// Construct a budget with explicit byte limits.
    #[must_use]
    pub fn new(total_bytes: u64, hot_bytes: u64, warm_bytes: u64) -> Self {
        Self {
            total_bytes,
            hot_bytes,
            warm_bytes,
        }
    }

    /// Derive a sensible budget from a total disk capacity.
    ///
    /// Allocates 20 % of the disk to the memory cache, with the hot tier
    /// taking 25 % of that and the warm tier taking the remaining 75 %.
    #[must_use]
    pub fn from_disk_capacity(disk_bytes: u64) -> Self {
        let total = disk_bytes / 5; // 20 %
        let hot = total / 4; // 25 % of cache
        let warm = total - hot; // 75 % of cache
        Self::new(total, hot, warm)
    }
}

// ── DeviceCapacityProfile ─────────────────────────────────────────────────────

/// Advertised by each mesh peer so that sibling devices know what is available.
///
/// The profile is serialised and broadcast via Hyperswarm DHT so any peer can
/// decide whether to route a memory fetch to this device.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::capacity::{DeviceCapacityProfile, StorageTier, StorageBudget};
///
/// let profile = DeviceCapacityProfile {
///     device_id: "desktop-001".to_string(),
///     tier: StorageTier::Full,
///     budget: StorageBudget::from_disk_capacity(2_000_000_000_000), // 2 TB
///     supports_full_index: true,
///     supports_quantized_index: true,
/// };
///
/// assert_eq!(profile.tier.max_age_days(), None);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapacityProfile {
    /// Stable, unique identifier for this device (e.g. UUID or hostname hash).
    pub device_id: String,
    /// Tier this device operates at.
    pub tier: StorageTier,
    /// Storage budget for the memory cache.
    pub budget: StorageBudget,
    /// Whether this device maintains a full-resolution embedding vector index.
    pub supports_full_index: bool,
    /// Whether this device maintains a compact / quantized embedding index.
    pub supports_quantized_index: bool,
}

impl DeviceCapacityProfile {
    /// Convenience constructor for a desktop-class full-storage device.
    #[must_use]
    pub fn full(device_id: impl Into<String>, disk_bytes: u64) -> Self {
        Self {
            device_id: device_id.into(),
            tier: StorageTier::Full,
            budget: StorageBudget::from_disk_capacity(disk_bytes),
            supports_full_index: true,
            supports_quantized_index: true,
        }
    }

    /// Convenience constructor for a laptop-class warm-tier device.
    #[must_use]
    pub fn warm(device_id: impl Into<String>, disk_bytes: u64) -> Self {
        Self {
            device_id: device_id.into(),
            tier: StorageTier::Warm,
            budget: StorageBudget::from_disk_capacity(disk_bytes),
            supports_full_index: false,
            supports_quantized_index: true,
        }
    }

    /// Convenience constructor for a phone-class hot-tier device.
    #[must_use]
    pub fn hot(device_id: impl Into<String>, disk_bytes: u64) -> Self {
        Self {
            device_id: device_id.into(),
            tier: StorageTier::Hot,
            budget: StorageBudget::from_disk_capacity(disk_bytes),
            supports_full_index: false,
            supports_quantized_index: true,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_tier_max_age_days() {
        assert_eq!(StorageTier::Hot.max_age_days(), Some(7));
        assert_eq!(StorageTier::Warm.max_age_days(), Some(90));
        assert_eq!(StorageTier::Full.max_age_days(), None);
        assert_eq!(
            StorageTier::Custom { max_age_days: 30 }.max_age_days(),
            Some(30)
        );
    }

    #[test]
    fn storage_budget_from_disk_capacity() {
        let b = StorageBudget::from_disk_capacity(2_000_000_000_000); // 2 TB
        assert_eq!(b.total_bytes, 400_000_000_000);
        assert_eq!(b.hot_bytes + b.warm_bytes, b.total_bytes);
    }

    #[test]
    fn device_capacity_profile_full() {
        let p = DeviceCapacityProfile::full("d1", 2_000_000_000_000);
        assert_eq!(p.tier, StorageTier::Full);
        assert!(p.supports_full_index);
    }

    #[test]
    fn device_capacity_profile_hot() {
        let p = DeviceCapacityProfile::hot("phone", 128_000_000_000);
        assert_eq!(p.tier, StorageTier::Hot);
        assert!(!p.supports_full_index);
    }
}
