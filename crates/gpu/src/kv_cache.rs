//! KV cache manager — per-request allocation from a shared VRAM pool.
//!
//! [`KvCacheManager`] tracks how many MiB of the shared KV cache pool are
//! currently allocated to in-flight requests.  It does **not** manage raw
//! device memory itself — that is left to the actual CUDA kernel or simulator.

use std::collections::HashMap;

use crate::error::GpuError;

/// Manages per-request KV cache allocations from a fixed VRAM budget.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::KvCacheManager;
///
/// let mut kv = KvCacheManager::new(4_096);
/// kv.allocate("req-1", 512).expect("should fit");
/// kv.allocate("req-2", 512).expect("should fit");
///
/// assert_eq!(kv.available_mb(), 3_072);
///
/// kv.free("req-1");
/// assert_eq!(kv.available_mb(), 3_584);
/// ```
#[derive(Debug)]
pub struct KvCacheManager {
    /// Total KV cache VRAM budget, in MiB.
    total_mb: u64,
    /// Currently allocated per-request slices (request_id → MiB).
    allocations: HashMap<String, u64>,
}

impl KvCacheManager {
    /// Create a new manager with `total_mb` MiB of available KV cache.
    pub fn new(total_mb: u64) -> Self {
        Self {
            total_mb,
            allocations: HashMap::new(),
        }
    }

    /// Allocate `size_mb` MiB for `request_id`.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::KvCacheExhausted`] if the pool does not have
    /// enough remaining capacity.
    pub fn allocate(&mut self, request_id: &str, size_mb: u64) -> Result<(), GpuError> {
        let avail = self.available_mb();
        if size_mb > avail {
            return Err(GpuError::KvCacheExhausted {
                needed_mb: size_mb,
                available_mb: avail,
            });
        }
        self.allocations
            .insert(request_id.to_owned(), size_mb);
        Ok(())
    }

    /// Release the KV cache slot held by `request_id`.
    ///
    /// If `request_id` was not previously allocated this is a no-op.
    pub fn free(&mut self, request_id: &str) {
        self.allocations.remove(request_id);
    }

    /// Total KV cache VRAM budget, in MiB.
    pub fn total_mb(&self) -> u64 {
        self.total_mb
    }

    /// Currently allocated KV cache, in MiB.
    pub fn used_mb(&self) -> u64 {
        self.allocations.values().sum()
    }

    /// Remaining KV cache available for new allocations, in MiB.
    pub fn available_mb(&self) -> u64 {
        self.total_mb.saturating_sub(self.used_mb())
    }

    /// Number of currently active allocations.
    pub fn active_count(&self) -> usize {
        self.allocations.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_free() {
        let mut kv = KvCacheManager::new(4_096);
        kv.allocate("req-1", 512).unwrap();
        kv.allocate("req-2", 1_024).unwrap();

        assert_eq!(kv.used_mb(), 1_536);
        assert_eq!(kv.available_mb(), 2_560);

        kv.free("req-1");
        assert_eq!(kv.available_mb(), 3_072);
        assert_eq!(kv.active_count(), 1);
    }

    #[test]
    fn exhausted_returns_error() {
        let mut kv = KvCacheManager::new(1_000);
        let err = kv.allocate("big-req", 2_000).unwrap_err();
        assert!(matches!(err, GpuError::KvCacheExhausted { .. }));
    }

    #[test]
    fn free_unknown_is_noop() {
        let mut kv = KvCacheManager::new(1_000);
        kv.free("nonexistent"); // should not panic
        assert_eq!(kv.available_mb(), 1_000);
    }
}
