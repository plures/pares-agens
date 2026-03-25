//! Cache performance metrics.
//!
//! [`CacheMetrics`] tracks per-device statistics for the distributed memory
//! cache: local hit rate, P2P fetch count and latency, and storage utilisation.

use std::time::Duration;

use serde::{Deserialize, Serialize};

// ── CacheMetrics ──────────────────────────────────────────────────────────────

/// Per-device distributed memory cache metrics.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::metrics::CacheMetrics;
/// use std::time::Duration;
///
/// let mut m = CacheMetrics::new();
/// m.record_local_hit();
/// m.record_remote_fetch(Duration::from_millis(80));
/// m.record_miss();
///
/// // 2 hits (local + remote) out of 3 total → 2/3
/// assert!((m.hit_rate() - (2.0 / 3.0)).abs() < 0.01);
/// assert_eq!(m.remote_fetches(), 1);
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CacheMetrics {
    local_hits: u64,
    remote_fetches: u64,
    misses: u64,
    total_remote_latency_ms: u64,
    storage_bytes_used: u64,
    storage_bytes_budget: u64,
}

impl CacheMetrics {
    /// Create a new, zeroed metrics instance.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Recording ─────────────────────────────────────────────────────────

    /// Record a local cache hit.
    pub fn record_local_hit(&mut self) {
        self.local_hits += 1;
    }

    /// Record a successful P2P fetch with the given round-trip latency.
    pub fn record_remote_fetch(&mut self, latency: Duration) {
        self.remote_fetches += 1;
        self.total_remote_latency_ms += latency.as_millis() as u64;
    }

    /// Record a cache miss (entry not found locally or remotely).
    pub fn record_miss(&mut self) {
        self.misses += 1;
    }

    /// Update the storage utilisation counters.
    pub fn update_storage(&mut self, bytes_used: u64, bytes_budget: u64) {
        self.storage_bytes_used = bytes_used;
        self.storage_bytes_budget = bytes_budget;
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    /// Total local cache hits.
    #[must_use]
    pub fn local_hits(&self) -> u64 {
        self.local_hits
    }

    /// Total P2P fetches (hits, not misses).
    #[must_use]
    pub fn remote_fetches(&self) -> u64 {
        self.remote_fetches
    }

    /// Total cache misses (neither local nor remote found the entry).
    #[must_use]
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Hit rate: `(local_hits + remote_fetches) / total_lookups`.
    ///
    /// Returns `0.0` if no lookups have been performed.
    #[must_use]
    pub fn hit_rate(&self) -> f32 {
        let total = self.local_hits + self.remote_fetches + self.misses;
        if total == 0 {
            return 0.0;
        }
        (self.local_hits + self.remote_fetches) as f32 / total as f32
    }

    /// Local-only hit rate: `local_hits / total_lookups`.
    #[must_use]
    pub fn local_hit_rate(&self) -> f32 {
        let total = self.local_hits + self.remote_fetches + self.misses;
        if total == 0 {
            return 0.0;
        }
        self.local_hits as f32 / total as f32
    }

    /// Mean P2P fetch latency in milliseconds.
    ///
    /// Returns `0` if no P2P fetches have been recorded.
    #[must_use]
    pub fn mean_remote_latency_ms(&self) -> u64 {
        if self.remote_fetches == 0 {
            return 0;
        }
        self.total_remote_latency_ms / self.remote_fetches
    }

    /// Storage utilisation as a fraction in `[0, 1]`.
    ///
    /// Returns `0.0` if the budget is zero.
    #[must_use]
    pub fn storage_utilisation(&self) -> f32 {
        if self.storage_bytes_budget == 0 {
            return 0.0;
        }
        (self.storage_bytes_used as f32 / self.storage_bytes_budget as f32).min(1.0)
    }

    /// Reset all counters to zero.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_rate_combined() {
        let mut m = CacheMetrics::new();
        m.record_local_hit();
        m.record_remote_fetch(Duration::from_millis(100));
        m.record_miss();
        // 2 hits (local + remote) out of 3 total
        assert!((m.hit_rate() - 2.0 / 3.0).abs() < 0.001);
    }

    #[test]
    fn hit_rate_zero_when_no_lookups() {
        let m = CacheMetrics::new();
        assert_eq!(m.hit_rate(), 0.0);
    }

    #[test]
    fn mean_remote_latency() {
        let mut m = CacheMetrics::new();
        m.record_remote_fetch(Duration::from_millis(100));
        m.record_remote_fetch(Duration::from_millis(200));
        assert_eq!(m.mean_remote_latency_ms(), 150);
    }

    #[test]
    fn mean_remote_latency_zero_when_no_fetches() {
        let m = CacheMetrics::new();
        assert_eq!(m.mean_remote_latency_ms(), 0);
    }

    #[test]
    fn storage_utilisation() {
        let mut m = CacheMetrics::new();
        m.update_storage(500, 1000);
        assert!((m.storage_utilisation() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn storage_utilisation_zero_budget() {
        let mut m = CacheMetrics::new();
        m.update_storage(100, 0);
        assert_eq!(m.storage_utilisation(), 0.0);
    }

    #[test]
    fn reset_clears_all_counters() {
        let mut m = CacheMetrics::new();
        m.record_local_hit();
        m.record_remote_fetch(Duration::from_millis(50));
        m.reset();
        assert_eq!(m.local_hits(), 0);
        assert_eq!(m.remote_fetches(), 0);
        assert_eq!(m.hit_rate(), 0.0);
    }
}
