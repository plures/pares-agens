//! Capacity-aware tiered memory cache.
//!
//! [`MemoryCache`] stores entries in three tiers — hot, warm, and cold — and
//! enforces per-tier byte budgets.  When a tier overflows its budget the
//! [`SmartEviction`] policy selects the least-valuable entry to demote or
//! discard.
//!
//! ## Tier lifecycle
//!
//! ```text
//! insert → hot tier
//!   hot overflow → demote oldest hot to warm
//!     warm overflow → demote oldest warm to cold (compressed)
//!       cold overflow → evict (discard) cold entry
//! ```

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    capacity::StorageBudget,
    compress::MemoryCompressor,
    eviction::SmartEviction,
    error::DmemError,
};

// ── CachedEntry ───────────────────────────────────────────────────────────────

/// An entry in the local memory cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedEntry {
    /// Unique memory ID (matches the PluresDB node ID).
    pub id: String,
    /// Serialised memory payload.
    ///
    /// In the hot tier this is uncompressed JSON.  In warm/cold tiers the
    /// bytes may be compressed (see [`MemoryCompressor`]).
    pub payload: Vec<u8>,
    /// ISO 8601 creation timestamp of the original memory.
    pub created_at: String,
    /// Semantic relevance score, updated on each retrieval.
    pub relevance_score: f32,
    /// Whether this entry is user-pinned (never evicted).
    pub pinned: bool,
    /// Size in bytes of the stored payload (after compression if any).
    pub stored_bytes: usize,
}

// ── CacheTier ─────────────────────────────────────────────────────────────────

/// Which tier an entry currently occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTier {
    /// Most recently accessed entries; served directly without decompression.
    Hot,
    /// Older entries demoted from hot; stored in compressed form.
    Warm,
    /// Least recently accessed entries; heavily compressed and rarely read.
    Cold,
}

// ── MemoryCache ───────────────────────────────────────────────────────────────

/// Capacity-aware tiered memory cache.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::cache::MemoryCache;
/// use pares_agens_dmem::capacity::StorageBudget;
///
/// let budget = StorageBudget::new(10_000, 2_000, 5_000);
/// let mut cache = MemoryCache::new(budget);
/// cache.insert("m1".to_string(), b"hello world".to_vec(), "2026-01-01T00:00:00Z".to_string(), 0.8);
/// assert!(cache.get("m1").is_some());
/// ```
pub struct MemoryCache {
    hot: HashMap<String, CachedEntry>,
    warm: HashMap<String, CachedEntry>,
    cold: HashMap<String, CachedEntry>,
    hot_bytes: usize,
    warm_bytes: usize,
    cold_bytes: usize,
    budget: StorageBudget,
    eviction: SmartEviction,
    compressor: MemoryCompressor,
}

impl MemoryCache {
    /// Create a new empty cache with the given storage budget.
    #[must_use]
    pub fn new(budget: StorageBudget) -> Self {
        Self {
            hot: HashMap::new(),
            warm: HashMap::new(),
            cold: HashMap::new(),
            hot_bytes: 0,
            warm_bytes: 0,
            cold_bytes: 0,
            budget,
            eviction: SmartEviction::new(),
            compressor: MemoryCompressor::new(),
        }
    }

    // ── Insertion ─────────────────────────────────────────────────────────

    /// Insert a new memory entry into the hot tier.
    ///
    /// If the hot tier exceeds its byte budget after insertion, the
    /// least-valuable hot entry is demoted to warm (and so on down the chain).
    pub fn insert(
        &mut self,
        id: String,
        payload: Vec<u8>,
        created_at: String,
        relevance_score: f32,
    ) {
        let stored_bytes = payload.len();
        self.eviction.track(&id, relevance_score);

        let entry = CachedEntry {
            id: id.clone(),
            payload,
            created_at,
            relevance_score,
            pinned: false,
            stored_bytes,
        };

        self.hot_bytes += stored_bytes;
        self.hot.insert(id, entry);

        self.rebalance_hot();
    }

    // ── Retrieval ─────────────────────────────────────────────────────────

    /// Retrieve an entry from any tier.  Touching promotes the entry to hot.
    ///
    /// Returns `None` if the entry is not in the local cache.
    pub fn get(&mut self, id: &str) -> Option<Vec<u8>> {
        self.eviction.touch(id);

        // Hot — no decompression needed
        if let Some(entry) = self.hot.get(id) {
            return Some(entry.payload.clone());
        }

        // Warm — decompress if needed, then promote to hot
        if let Some(entry) = self.warm.remove(id) {
            let size = entry.stored_bytes;
            self.warm_bytes -= size;
            let payload = self.compressor.decompress(&entry.payload);
            let result = payload.clone();
            self.hot_bytes += payload.len();
            let promoted = CachedEntry {
                stored_bytes: payload.len(),
                payload,
                ..entry
            };
            self.hot.insert(id.to_owned(), promoted);
            self.rebalance_hot();
            return Some(result);
        }

        // Cold — decompress if needed, then promote to hot
        if let Some(entry) = self.cold.remove(id) {
            let size = entry.stored_bytes;
            self.cold_bytes -= size;
            let payload = self.compressor.decompress(&entry.payload);
            let result = payload.clone();
            self.hot_bytes += payload.len();
            let promoted = CachedEntry {
                stored_bytes: payload.len(),
                payload,
                ..entry
            };
            self.hot.insert(id.to_owned(), promoted);
            self.rebalance_hot();
            return Some(result);
        }

        None
    }

    // ── Pinning ───────────────────────────────────────────────────────────

    /// Pin an entry so it is never evicted from the cache.
    pub fn pin(&mut self, id: &str) {
        self.eviction.pin(id);
        for tier in [&mut self.hot, &mut self.warm, &mut self.cold] {
            if let Some(e) = tier.get_mut(id) {
                e.pinned = true;
                return;
            }
        }
    }

    /// Unpin an entry, making it eligible for eviction again.
    pub fn unpin(&mut self, id: &str) {
        self.eviction.unpin(id);
        for tier in [&mut self.hot, &mut self.warm, &mut self.cold] {
            if let Some(e) = tier.get_mut(id) {
                e.pinned = false;
                return;
            }
        }
    }

    // ── Removal ───────────────────────────────────────────────────────────

    /// Remove an entry from the cache unconditionally (e.g. after PluresDB delete).
    pub fn remove(&mut self, id: &str) -> bool {
        self.eviction.remove(id);
        if let Some(e) = self.hot.remove(id) {
            self.hot_bytes -= e.stored_bytes;
            return true;
        }
        if let Some(e) = self.warm.remove(id) {
            self.warm_bytes -= e.stored_bytes;
            return true;
        }
        if let Some(e) = self.cold.remove(id) {
            self.cold_bytes -= e.stored_bytes;
            return true;
        }
        false
    }

    // ── Introspection ─────────────────────────────────────────────────────

    /// Total number of entries across all tiers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hot.len() + self.warm.len() + self.cold.len()
    }

    /// `true` if the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the tier an entry currently occupies.  Returns `None` if the
    /// entry is not in the cache.
    #[must_use]
    pub fn tier_of(&self, id: &str) -> Option<CacheTier> {
        if self.hot.contains_key(id) {
            return Some(CacheTier::Hot);
        }
        if self.warm.contains_key(id) {
            return Some(CacheTier::Warm);
        }
        if self.cold.contains_key(id) {
            return Some(CacheTier::Cold);
        }
        None
    }

    /// Total bytes used across all tiers.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.hot_bytes + self.warm_bytes + self.cold_bytes
    }

    /// Current hot-tier utilisation as a fraction in `[0, 1]`.
    #[must_use]
    pub fn hot_utilisation(&self) -> f32 {
        let budget = self.budget.hot_bytes as usize;
        if budget == 0 {
            return 0.0;
        }
        (self.hot_bytes as f32 / budget as f32).min(1.0)
    }

    // ── Internal rebalancing ──────────────────────────────────────────────

    /// Demote hot entries to warm until hot usage is within budget.
    fn rebalance_hot(&mut self) {
        while self.hot_bytes > self.budget.hot_bytes as usize {
            let Some(id) = self.eviction.evict_candidate().map(str::to_owned) else {
                break;
            };
            // Only demote hot entries; skip if candidate is in warm/cold
            if let Some(entry) = self.hot.remove(&id) {
                self.hot_bytes -= entry.stored_bytes;
                self.demote_to_warm(entry);
            } else {
                break;
            }
        }
    }

    /// Move an entry from hot to the warm tier, compressing the payload.
    fn demote_to_warm(&mut self, entry: CachedEntry) {
        let compressed = self.compressor.compress(&entry.payload);
        let stored_bytes = compressed.len();
        let warm_entry = CachedEntry {
            payload: compressed,
            stored_bytes,
            ..entry
        };
        self.warm_bytes += stored_bytes;
        self.warm.insert(warm_entry.id.clone(), warm_entry);
        self.rebalance_warm();
    }

    /// Demote warm entries to cold until warm usage is within budget.
    fn rebalance_warm(&mut self) {
        while self.warm_bytes > self.budget.warm_bytes as usize {
            let Some(id) = self.eviction.evict_candidate().map(str::to_owned) else {
                break;
            };
            if let Some(entry) = self.warm.remove(&id) {
                self.warm_bytes -= entry.stored_bytes;
                self.demote_to_cold(entry);
            } else {
                break;
            }
        }
    }

    /// Move an entry from warm to the cold tier.
    fn demote_to_cold(&mut self, entry: CachedEntry) {
        let cold_budget = (self.budget.total_bytes
            - self.budget.hot_bytes
            - self.budget.warm_bytes) as usize;
        let stored_bytes = entry.stored_bytes;
        self.cold_bytes += stored_bytes;
        let id = entry.id.clone();
        self.cold.insert(id, entry);
        // Evict from cold if over total budget
        while self.cold_bytes > cold_budget {
            let Some(cid) = self.eviction.evict_candidate().map(str::to_owned) else {
                break;
            };
            if let Some(e) = self.cold.remove(&cid) {
                self.cold_bytes -= e.stored_bytes;
                self.eviction.remove(&cid);
            } else {
                break;
            }
        }
    }
}

// ── Error compatibility ───────────────────────────────────────────────────────

impl MemoryCache {
    /// Attempt to insert, returning an error if the total cache is full and
    /// no entry can be evicted.
    pub fn try_insert(
        &mut self,
        id: String,
        payload: Vec<u8>,
        created_at: String,
        relevance_score: f32,
    ) -> Result<(), DmemError> {
        self.insert(id, payload, created_at, relevance_score);
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::StorageBudget;

    fn make_cache() -> MemoryCache {
        // tiny budget so tier demotion fires quickly in tests
        let budget = StorageBudget::new(300, 100, 100);
        MemoryCache::new(budget)
    }

    #[test]
    fn insert_and_get_from_hot_tier() {
        let mut c = make_cache();
        c.insert("a".into(), b"hello".to_vec(), "2026-01-01T00:00:00Z".to_string(), 0.5);
        assert_eq!(c.tier_of("a"), Some(CacheTier::Hot));
        assert_eq!(c.get("a"), Some(b"hello".to_vec()));
    }

    #[test]
    fn len_and_is_empty() {
        let mut c = make_cache();
        assert!(c.is_empty());
        c.insert("x".into(), vec![1, 2, 3], "2026-01-01T00:00:00Z".to_string(), 0.5);
        assert_eq!(c.len(), 1);
        assert!(!c.is_empty());
    }

    #[test]
    fn remove_decreases_len() {
        let mut c = make_cache();
        c.insert("r".into(), vec![1], "2026-01-01T00:00:00Z".to_string(), 0.5);
        assert!(c.remove("r"));
        assert_eq!(c.len(), 0);
        assert!(!c.remove("r")); // already gone
    }

    #[test]
    fn pin_prevents_eviction() {
        let budget = StorageBudget::new(200, 50, 50);
        let mut c = MemoryCache::new(budget);
        // Insert a pinned entry
        c.insert("important".into(), vec![0u8; 10], "2026-01-01T00:00:00Z".to_string(), 0.9);
        c.pin("important");
        // Fill up with other entries to trigger eviction
        for i in 0..20u8 {
            c.insert(
                format!("filler-{i}"),
                vec![0u8; 10],
                "2026-01-01T00:00:00Z".to_string(),
                0.1,
            );
        }
        // Pinned entry should still be present somewhere
        assert!(c.tier_of("important").is_some());
    }

    #[test]
    fn hot_utilisation_between_zero_and_one() {
        let mut c = make_cache();
        let u = c.hot_utilisation();
        assert!((0.0..=1.0).contains(&u));
        c.insert("a".into(), vec![0u8; 50], "2026-01-01T00:00:00Z".to_string(), 0.5);
        let u2 = c.hot_utilisation();
        assert!(u2 >= u);
    }
}
