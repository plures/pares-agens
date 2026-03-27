//! Transparent memory fetch — local cache hit or P2P peer fetch.
//!
//! [`FetchRouter`] is the single entry point for retrieving a memory entry.
//! It first consults the local [`MemoryCache`] and, on a miss, delegates to a
//! [`PeerFetcher`] to retrieve the entry from a mesh peer.
//!
//! Latency is tracked so that [`crate::metrics::CacheMetrics`] can report P2P
//! fetch latency percentiles.

use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::{cache::MemoryCache, error::DmemError, metrics::CacheMetrics};

// ── PeerFetcher (trait) ───────────────────────────────────────────────────────

/// Abstraction over the P2P transport layer.
///
/// In production this will use the Hyperswarm DHT transport from
/// `pares-agens-sync`.  In tests a [`SimulatedPeerFetcher`] is provided.
#[async_trait]
pub trait PeerFetcher: Send + Sync {
    /// Attempt to fetch the raw payload for `memory_id` from any available
    /// mesh peer.
    ///
    /// Returns `None` if no peer has the entry.
    async fn fetch(&self, memory_id: &str) -> Result<Option<Vec<u8>>, DmemError>;
}

// ── FetchResult ───────────────────────────────────────────────────────────────

/// The outcome of a [`FetchRouter::get`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchResult {
    /// The entry was found in the local cache tier.
    LocalHit {
        /// The raw payload bytes.
        payload: Vec<u8>,
    },
    /// The entry was fetched from a remote mesh peer.
    RemoteFetch {
        /// The raw payload bytes.
        payload: Vec<u8>,
        /// Round-trip latency of the P2P fetch.
        latency: Duration,
    },
    /// The entry was not found anywhere in the mesh.
    Miss,
}

// ── FetchRouter ───────────────────────────────────────────────────────────────

/// Routes memory retrieval to local cache or a remote peer.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::fetch::{FetchRouter, SimulatedPeerFetcher};
/// use pares_agens_dmem::cache::MemoryCache;
/// use pares_agens_dmem::capacity::StorageBudget;
/// use pares_agens_dmem::metrics::CacheMetrics;
///
/// # #[tokio::main]
/// # async fn main() {
/// let budget = StorageBudget::new(100_000, 20_000, 50_000);
/// let mut cache = MemoryCache::new(budget);
/// cache.insert("m1".into(), b"hello".to_vec(), "2026-01-01T00:00:00Z".to_string(), 0.8);
///
/// let fetcher = SimulatedPeerFetcher::empty();
/// let mut metrics = CacheMetrics::new();
/// let mut router = FetchRouter::new(fetcher);
///
/// let result = router.get("m1", &mut cache, &mut metrics).await;
/// // m1 is in the local cache → local hit
/// use pares_agens_dmem::fetch::FetchResult;
/// assert!(matches!(result, Ok(FetchResult::LocalHit { .. })));
/// # }
/// ```
pub struct FetchRouter<F: PeerFetcher> {
    fetcher: F,
}

impl<F: PeerFetcher> FetchRouter<F> {
    /// Construct a router backed by the given peer fetcher.
    pub fn new(fetcher: F) -> Self {
        Self { fetcher }
    }

    /// Retrieve a memory entry, consulting the local cache first and falling
    /// back to a P2P fetch on a miss.
    ///
    /// On a remote hit the entry is inserted back into the local cache at the
    /// hot tier so subsequent reads are served locally.
    pub async fn get(
        &mut self,
        id: &str,
        cache: &mut MemoryCache,
        metrics: &mut CacheMetrics,
    ) -> Result<FetchResult, DmemError> {
        // 1. Try local cache
        if let Some(payload) = cache.get(id) {
            metrics.record_local_hit();
            return Ok(FetchResult::LocalHit { payload });
        }

        // 2. Fall back to P2P fetch
        let start = Instant::now();
        match self.fetcher.fetch(id).await? {
            Some(payload) => {
                let latency = start.elapsed();
                metrics.record_remote_fetch(latency);
                // Warm the local cache so the next access is local
                cache.insert(
                    id.to_owned(),
                    payload.clone(),
                    chrono::Utc::now().to_rfc3339(),
                    0.5,
                );
                Ok(FetchResult::RemoteFetch { payload, latency })
            }
            None => {
                metrics.record_miss();
                Ok(FetchResult::Miss)
            }
        }
    }
}

// ── SimulatedPeerFetcher ──────────────────────────────────────────────────────

/// A peer fetcher backed by an in-memory map — useful for tests.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::fetch::SimulatedPeerFetcher;
///
/// let mut f = SimulatedPeerFetcher::empty();
/// f.add_peer_entry("remote-id", b"peer payload".to_vec());
/// ```
pub struct SimulatedPeerFetcher {
    entries: std::collections::HashMap<String, Vec<u8>>,
}

impl SimulatedPeerFetcher {
    /// Create a fetcher with no peer entries (everything is a miss).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
        }
    }

    /// Register an entry that this simulated peer holds.
    pub fn add_peer_entry(&mut self, id: &str, payload: Vec<u8>) {
        self.entries.insert(id.to_owned(), payload);
    }
}

#[async_trait]
impl PeerFetcher for SimulatedPeerFetcher {
    async fn fetch(&self, memory_id: &str) -> Result<Option<Vec<u8>>, DmemError> {
        Ok(self.entries.get(memory_id).cloned())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::StorageBudget;

    fn make_cache() -> MemoryCache {
        MemoryCache::new(StorageBudget::new(100_000, 20_000, 50_000))
    }

    #[tokio::test]
    async fn local_hit_returns_correct_payload() {
        let mut cache = make_cache();
        cache.insert(
            "a".into(),
            b"local data".to_vec(),
            "2026-01-01T00:00:00Z".to_string(),
            0.9,
        );

        let fetcher = SimulatedPeerFetcher::empty();
        let mut router = FetchRouter::new(fetcher);
        let mut metrics = CacheMetrics::new();

        let result = router.get("a", &mut cache, &mut metrics).await.unwrap();
        assert!(matches!(result, FetchResult::LocalHit { payload } if payload == b"local data"));
        assert_eq!(metrics.local_hits(), 1);
    }

    #[tokio::test]
    async fn remote_fetch_on_cache_miss() {
        let mut cache = make_cache();
        let mut fetcher = SimulatedPeerFetcher::empty();
        fetcher.add_peer_entry("remote", b"peer payload".to_vec());

        let mut router = FetchRouter::new(fetcher);
        let mut metrics = CacheMetrics::new();

        let result = router
            .get("remote", &mut cache, &mut metrics)
            .await
            .unwrap();
        assert!(
            matches!(result, FetchResult::RemoteFetch { payload, .. } if payload == b"peer payload")
        );
        assert_eq!(metrics.remote_fetches(), 1);
    }

    #[tokio::test]
    async fn miss_when_not_found_anywhere() {
        let mut cache = make_cache();
        let fetcher = SimulatedPeerFetcher::empty();
        let mut router = FetchRouter::new(fetcher);
        let mut metrics = CacheMetrics::new();

        let result = router.get("ghost", &mut cache, &mut metrics).await.unwrap();
        assert_eq!(result, FetchResult::Miss);
    }

    #[tokio::test]
    async fn remote_hit_warms_local_cache() {
        let mut cache = make_cache();
        let mut fetcher = SimulatedPeerFetcher::empty();
        fetcher.add_peer_entry("r1", b"data".to_vec());

        let mut router = FetchRouter::new(fetcher);
        let mut metrics = CacheMetrics::new();

        // First access: remote fetch
        router.get("r1", &mut cache, &mut metrics).await.unwrap();
        // Second access: should be local hit now
        let result = router.get("r1", &mut cache, &mut metrics).await.unwrap();
        assert!(matches!(result, FetchResult::LocalHit { .. }));
    }
}
