//! Top-level distributed memory manager.
//!
//! [`DistributedMemoryManager`] is the single entry point for all distributed
//! memory operations.  It wires together the tiered cache, eviction policy,
//! embedding index, pin registry, prefetch predictor, fetch router, and metrics
//! into a cohesive whole.

use crate::{
    cache::MemoryCache,
    capacity::DeviceCapacityProfile,
    error::DmemError,
    eviction::SmartEviction,
    fetch::{FetchResult, FetchRouter, PeerFetcher},
    index::{EmbeddingIndex, ScoredEntry},
    metrics::CacheMetrics,
    pin::PinRegistry,
    policy::StoragePolicy,
    prefetch::PrefetchPredictor,
    search::{MeshSearch, PeerSearcher},
};

// ── DistributedMemoryManager ──────────────────────────────────────────────────

/// Orchestrates all distributed memory management operations.
///
/// # Generic parameters
///
/// - `F` — A [`PeerFetcher`] implementation (e.g. Hyperswarm transport or
///   [`crate::fetch::SimulatedPeerFetcher`] for tests).
/// - `S` — A [`PeerSearcher`] implementation (e.g. remote semantic search or
///   [`crate::search::SimulatedPeerSearcher`] for tests).
/// - `I` — An [`EmbeddingIndex`] implementation (e.g. [`crate::index::FullIndex`]
///   or [`crate::index::QuantizedIndex`]).
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::manager::DistributedMemoryManager;
/// use pares_agens_dmem::capacity::DeviceCapacityProfile;
/// use pares_agens_dmem::fetch::SimulatedPeerFetcher;
/// use pares_agens_dmem::search::{MeshSearch, SimulatedPeerSearcher};
/// use pares_agens_dmem::index::QuantizedIndex;
///
/// # #[tokio::main]
/// # async fn main() {
/// let profile = DeviceCapacityProfile::warm("laptop-01", 512_000_000_000);
/// let fetcher = SimulatedPeerFetcher::empty();
/// let index = QuantizedIndex::new(384);
/// let mesh_search: MeshSearch<SimulatedPeerSearcher> = MeshSearch::new(0.8);
///
/// let mut mgr = DistributedMemoryManager::new(profile, fetcher, index, mesh_search);
///
/// // Store a memory
/// mgr.store(
///     "mem-1".into(),
///     b"Rust is great for systems programming".to_vec(),
///     "2026-01-01T00:00:00Z".to_string(),
///     &[0.1_f32; 384],
///     0.85,
/// ).unwrap();
///
/// // Fetch it back
/// let result = mgr.fetch("mem-1").await.unwrap();
/// assert!(result.is_some());
/// # }
/// ```
pub struct DistributedMemoryManager<F, S, I>
where
    F: PeerFetcher,
    S: PeerSearcher,
    I: EmbeddingIndex,
{
    #[allow(dead_code)]
    profile: DeviceCapacityProfile,
    #[allow(dead_code)]
    policy: StoragePolicy,
    cache: MemoryCache,
    eviction: SmartEviction,
    index: I,
    pins: PinRegistry,
    prefetch: PrefetchPredictor,
    router: FetchRouter<F>,
    mesh: MeshSearch<S>,
    metrics: CacheMetrics,
}

impl<F, S, I> DistributedMemoryManager<F, S, I>
where
    F: PeerFetcher,
    S: PeerSearcher,
    I: EmbeddingIndex,
{
    /// Create a new manager for the given device profile.
    #[must_use]
    pub fn new(
        profile: DeviceCapacityProfile,
        fetcher: F,
        index: I,
        mesh: MeshSearch<S>,
    ) -> Self {
        let policy = StoragePolicy::for_tier(&profile.tier);
        let cache = MemoryCache::new(profile.budget.clone());
        Self {
            profile,
            policy,
            cache,
            eviction: SmartEviction::new(),
            index,
            pins: PinRegistry::new(),
            prefetch: PrefetchPredictor::new(),
            router: FetchRouter::new(fetcher),
            mesh,
            metrics: CacheMetrics::new(),
        }
    }

    // ── Storage ───────────────────────────────────────────────────────────

    /// Store a memory entry in the local cache and update the embedding index.
    ///
    /// # Errors
    ///
    /// Returns [`DmemError::Index`] if the embedding index update fails.
    pub fn store(
        &mut self,
        id: String,
        payload: Vec<u8>,
        created_at: String,
        embedding: &[f32],
        relevance_score: f32,
    ) -> Result<(), DmemError> {
        self.index.upsert(&id, embedding)?;
        self.eviction.track(&id, relevance_score);
        self.cache
            .try_insert(id, payload, created_at, relevance_score)?;
        self.update_storage_metrics();
        Ok(())
    }

    // ── Retrieval ─────────────────────────────────────────────────────────

    /// Fetch a memory entry by ID.
    ///
    /// Consults the local cache first, falls back to P2P fetch on a miss.
    /// Returns `None` only if the entry is not available anywhere in the mesh.
    ///
    /// # Errors
    ///
    /// Returns [`DmemError`] if the P2P fetch transport encounters an error.
    pub async fn fetch(&mut self, id: &str) -> Result<Option<Vec<u8>>, DmemError> {
        match self.router.get(id, &mut self.cache, &mut self.metrics).await? {
            FetchResult::LocalHit { payload } | FetchResult::RemoteFetch { payload, .. } => {
                self.eviction.touch(id);
                Ok(Some(payload))
            }
            FetchResult::Miss => Ok(None),
        }
    }

    // ── Semantic search ───────────────────────────────────────────────────

    /// Perform a semantic search against the local embedding index, fanning
    /// out to mesh peers if the local confidence is insufficient.
    ///
    /// # Errors
    ///
    /// Returns [`DmemError`] if a peer search transport fails.
    pub async fn search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, DmemError> {
        let local_results = self.index.query(query_embedding, top_k);
        self.mesh
            .search_with_fanout(query_embedding, local_results, top_k)
            .await
    }

    // ── Pinning ───────────────────────────────────────────────────────────

    /// Pin a memory entry so it is never evicted.
    pub fn pin(&mut self, id: &str) {
        self.pins.pin(id);
        self.cache.pin(id);
        self.eviction.pin(id);
    }

    /// Unpin a memory entry.
    pub fn unpin(&mut self, id: &str) {
        self.pins.unpin(id);
        self.cache.unpin(id);
        self.eviction.unpin(id);
    }

    /// Return `true` if `id` is currently pinned.
    #[must_use]
    pub fn is_pinned(&self, id: &str) -> bool {
        self.pins.is_pinned(id)
    }

    // ── Prefetch ──────────────────────────────────────────────────────────

    /// Record a memory access correlated with the given context tags.
    ///
    /// Call this every time a memory is accessed so the prefetch predictor can
    /// learn co-occurrence patterns.
    pub fn record_access(&mut self, id: &str, context_tags: &[&str]) {
        self.prefetch.record_access(id, context_tags);
        self.eviction.touch(id);
    }

    /// Return the top predicted memory IDs for the given context tags.
    ///
    /// The caller can then call [`fetch`][Self::fetch] for each hint to warm
    /// the local cache before the user's query arrives.
    #[must_use]
    pub fn predict_prefetch(
        &self,
        context_tags: &[&str],
        top_k: usize,
    ) -> Vec<crate::prefetch::PrefetchHint> {
        self.prefetch.predict(context_tags, top_k)
    }

    // ── Eviction ──────────────────────────────────────────────────────────

    /// Remove the current eviction candidate from the cache.
    ///
    /// Returns the ID of the evicted entry, or `None` if all entries are
    /// pinned or the cache is empty.
    pub fn evict_one(&mut self) -> Option<String> {
        let id = self.eviction.evict_candidate()?.to_owned();
        self.cache.remove(&id);
        self.index.remove(&id);
        self.eviction.remove(&id);
        self.update_storage_metrics();
        Some(id)
    }

    // ── Metrics ───────────────────────────────────────────────────────────

    /// Return a snapshot of the current cache metrics.
    #[must_use]
    pub fn metrics(&self) -> &CacheMetrics {
        &self.metrics
    }

    /// The current local cache hit rate in `[0, 1]`.
    #[must_use]
    pub fn hit_rate(&self) -> f32 {
        self.metrics.hit_rate()
    }

    // ── Internal helpers ──────────────────────────────────────────────────

    fn update_storage_metrics(&mut self) {
        self.metrics.update_storage(
            self.cache.total_bytes() as u64,
            self.profile.budget.total_bytes,
        );
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::DeviceCapacityProfile;
    use crate::fetch::SimulatedPeerFetcher;
    use crate::index::FullIndex;
    use crate::search::{MeshSearch, SimulatedPeerSearcher};

    fn make_manager() -> DistributedMemoryManager<
        SimulatedPeerFetcher,
        SimulatedPeerSearcher,
        FullIndex,
    > {
        let profile = DeviceCapacityProfile::warm("test-device", 10_000_000_000);
        let fetcher = SimulatedPeerFetcher::empty();
        let index = FullIndex::new();
        let mesh: MeshSearch<SimulatedPeerSearcher> = MeshSearch::new(0.8);
        DistributedMemoryManager::new(profile, fetcher, index, mesh)
    }

    #[tokio::test]
    async fn store_and_fetch_roundtrip() {
        let mut mgr = make_manager();
        mgr.store(
            "m1".into(),
            b"test payload".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            &[1.0, 0.0, 0.0],
            0.8,
        )
        .unwrap();
        let result = mgr.fetch("m1").await.unwrap();
        assert_eq!(result, Some(b"test payload".to_vec()));
    }

    #[tokio::test]
    async fn fetch_miss_returns_none() {
        let mut mgr = make_manager();
        let result = mgr.fetch("nonexistent").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn pin_prevents_eviction() {
        let mut mgr = make_manager();
        mgr.store(
            "keep".into(),
            b"important".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            &[1.0],
            0.1,
        )
        .unwrap();
        mgr.pin("keep");
        let evicted = mgr.evict_one();
        // Only entry is pinned → nothing to evict
        assert!(evicted.is_none() || evicted.as_deref() != Some("keep"));
    }

    #[tokio::test]
    async fn evict_removes_entry() {
        let mut mgr = make_manager();
        mgr.store(
            "e1".into(),
            b"data".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            &[0.5],
            0.2,
        )
        .unwrap();
        let evicted = mgr.evict_one();
        assert_eq!(evicted, Some("e1".into()));
        let result = mgr.fetch("e1").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn metrics_record_hits() {
        let mut mgr = make_manager();
        mgr.store(
            "x".into(),
            b"x".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            &[0.1],
            0.5,
        )
        .unwrap();
        mgr.fetch("x").await.unwrap();
        assert_eq!(mgr.metrics().local_hits(), 1);
    }

    #[test]
    fn predict_prefetch_returns_hints() {
        let mut mgr = make_manager();
        mgr.record_access("m1", &["project:foo"]);
        let hints = mgr.predict_prefetch(&["project:foo"], 5);
        assert!(!hints.is_empty());
        assert!(hints.iter().any(|h| h.memory_id == "m1"));
    }

    #[tokio::test]
    async fn search_returns_results() {
        let mut mgr = make_manager();
        mgr.store(
            "s1".into(),
            b"search me".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            &[1.0, 0.0],
            0.9,
        )
        .unwrap();
        let results = mgr.search(&[1.0, 0.0], 5).await.unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, "s1");
    }
}
