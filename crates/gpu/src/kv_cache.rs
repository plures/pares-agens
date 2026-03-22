//! KV-cache manager — per-request allocations from a shared VRAM pool.
//!
//! A fixed VRAM budget is reserved for KV caches at pool creation time.
//! Each inference request calls [`KvCacheManager::allocate`] to claim a slice
//! and [`KvCacheManager::free`] (or drops the returned [`KvCacheAllocation`])
//! when done.
//!
//! The implementation is backend-agnostic: on real hardware the VRAM pointer
//! would be handed to the CUDA kernel; here we track only byte accounting.

use std::sync::{Arc, Mutex};

use tracing::debug;

use crate::GpuError;

// ── KvCacheAllocation ─────────────────────────────────────────────────────────

/// A live KV-cache allocation.
///
/// Drop this value (or call [`KvCacheManager::free`]) to return the VRAM
/// to the shared pool.
#[derive(Debug)]
pub struct KvCacheAllocation {
    /// Unique request identifier this allocation belongs to.
    pub request_id: String,
    /// Model this allocation was made for.
    pub model_id: String,
    /// Size of the allocation in megabytes.
    pub size_mb: u64,
    /// Shared reference back to the pool so we can free on drop.
    pool: Arc<Mutex<KvCachePool>>,
}

impl Drop for KvCacheAllocation {
    fn drop(&mut self) {
        if let Ok(mut p) = self.pool.lock() {
            p.free(self.size_mb);
        }
    }
}

// ── KvCachePool (internal) ────────────────────────────────────────────────────

#[derive(Debug)]
struct KvCachePool {
    total_mb: u64,
    used_mb: u64,
}

impl KvCachePool {
    fn new(total_mb: u64) -> Self {
        Self { total_mb, used_mb: 0 }
    }

    fn available_mb(&self) -> u64 {
        self.total_mb.saturating_sub(self.used_mb)
    }

    fn allocate(&mut self, size_mb: u64) -> Result<(), GpuError> {
        let available = self.available_mb();
        if size_mb > available {
            return Err(GpuError::KvCacheExhausted {
                requested_mb: size_mb,
                available_mb: available,
            });
        }
        self.used_mb += size_mb;
        Ok(())
    }

    fn free(&mut self, size_mb: u64) {
        self.used_mb = self.used_mb.saturating_sub(size_mb);
    }
}

// ── KvCacheManager ────────────────────────────────────────────────────────────

/// Manages KV-cache allocations from a fixed VRAM pool.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::kv_cache::KvCacheManager;
///
/// let mgr = KvCacheManager::new(5_120); // 5 GB pool
///
/// let alloc = mgr.allocate("req-1", "chat-8b", 512).unwrap();
/// assert_eq!(mgr.used_mb(), 512);
/// drop(alloc); // returns VRAM to the pool
/// assert_eq!(mgr.used_mb(), 0);
/// ```
#[derive(Debug, Clone)]
pub struct KvCacheManager {
    pool: Arc<Mutex<KvCachePool>>,
}

impl KvCacheManager {
    /// Create a new manager with the given VRAM budget (in MB).
    pub fn new(budget_mb: u64) -> Self {
        Self {
            pool: Arc::new(Mutex::new(KvCachePool::new(budget_mb))),
        }
    }

    /// Total VRAM budget in MB.
    pub fn total_mb(&self) -> u64 {
        self.pool
            .lock()
            .expect("KV-cache pool mutex poisoned — a thread panicked while holding the lock")
            .total_mb
    }

    /// VRAM currently in use in MB.
    pub fn used_mb(&self) -> u64 {
        self.pool
            .lock()
            .expect("KV-cache pool mutex poisoned — a thread panicked while holding the lock")
            .used_mb
    }

    /// VRAM available for new allocations in MB.
    pub fn available_mb(&self) -> u64 {
        self.pool
            .lock()
            .expect("KV-cache pool mutex poisoned — a thread panicked while holding the lock")
            .available_mb()
    }

    /// Allocate `size_mb` megabytes of KV-cache VRAM for `request_id` on `model_id`.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::KvCacheExhausted`] if there is not enough space.
    pub fn allocate(
        &self,
        request_id: impl Into<String>,
        model_id: impl Into<String>,
        size_mb: u64,
    ) -> Result<KvCacheAllocation, GpuError> {
        let request_id = request_id.into();
        let model_id = model_id.into();
        {
            let mut pool = self.pool.lock().expect(
                "KV-cache pool mutex poisoned — a thread panicked while holding the lock",
            );
            pool.allocate(size_mb)?;
        }
        debug!(request_id, model_id, size_mb, "kv-cache allocated");
        Ok(KvCacheAllocation {
            request_id,
            model_id,
            size_mb,
            pool: Arc::clone(&self.pool),
        })
    }

    /// Explicitly free an allocation (also freed automatically on [`Drop`]).
    pub fn free(&self, alloc: KvCacheAllocation) {
        drop(alloc);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_and_free_via_drop() {
        let mgr = KvCacheManager::new(1_024);
        assert_eq!(mgr.available_mb(), 1_024);

        let alloc = mgr.allocate("req-1", "model-a", 256).unwrap();
        assert_eq!(mgr.used_mb(), 256);
        assert_eq!(mgr.available_mb(), 768);

        drop(alloc);
        assert_eq!(mgr.used_mb(), 0);
        assert_eq!(mgr.available_mb(), 1_024);
    }

    #[test]
    fn multiple_allocations_tracked_separately() {
        let mgr = KvCacheManager::new(2_048);

        let a1 = mgr.allocate("req-1", "model-a", 512).unwrap();
        let a2 = mgr.allocate("req-2", "model-b", 256).unwrap();

        assert_eq!(mgr.used_mb(), 768);

        drop(a1);
        assert_eq!(mgr.used_mb(), 256);

        drop(a2);
        assert_eq!(mgr.used_mb(), 0);
    }

    #[test]
    fn allocation_fails_when_pool_full() {
        let mgr = KvCacheManager::new(512);
        let _a = mgr.allocate("req-1", "model-a", 400).unwrap();

        let err = mgr.allocate("req-2", "model-b", 200).unwrap_err();
        assert!(matches!(err, GpuError::KvCacheExhausted { .. }));
    }

    #[test]
    fn explicit_free_via_manager() {
        let mgr = KvCacheManager::new(1_024);
        let alloc = mgr.allocate("req-1", "model-a", 256).unwrap();
        assert_eq!(mgr.used_mb(), 256);
        mgr.free(alloc);
        assert_eq!(mgr.used_mb(), 0);
    }

    #[test]
    fn zero_budget_always_fails() {
        let mgr = KvCacheManager::new(0);
        let err = mgr.allocate("req-1", "model-a", 1).unwrap_err();
        assert!(matches!(err, GpuError::KvCacheExhausted { .. }));
    }

    #[test]
    fn total_mb_is_immutable() {
        let mgr = KvCacheManager::new(4_096);
        let _a = mgr.allocate("req-1", "m", 1_000).unwrap();
        assert_eq!(mgr.total_mb(), 4_096);
    }
}
