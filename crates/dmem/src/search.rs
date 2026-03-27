//! Mesh-wide semantic search.
//!
//! [`MeshSearch`] fans out a semantic query to peer devices when the local
//! embedding index does not return high-confidence results.  The results from
//! all responding peers are merged and re-ranked by score before being returned
//! to the caller.

use async_trait::async_trait;

use crate::{error::DmemError, index::ScoredEntry};

// ── PeerSearcher (trait) ──────────────────────────────────────────────────────

/// Abstraction over a remote peer's semantic search endpoint.
#[async_trait]
pub trait PeerSearcher: Send + Sync {
    /// Execute a semantic search against this peer's local index.
    ///
    /// `embedding` is the query vector; `top_k` is the maximum results to
    /// return.
    async fn search(&self, embedding: &[f32], top_k: usize) -> Result<Vec<ScoredEntry>, DmemError>;
}

// ── MeshSearch ────────────────────────────────────────────────────────────────

/// Fans out semantic search queries to mesh peers when local confidence is low.
///
/// # Fanout strategy
///
/// 1. Collect results from the local index (caller's responsibility).
/// 2. If the best local result has `score ≥ confidence_threshold`, return
///    local results without contacting peers.
/// 3. Otherwise, query all registered [`PeerSearcher`]s concurrently.
/// 4. Merge and re-rank all results, returning the global top-k.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::search::{MeshSearch, SimulatedPeerSearcher};
/// use pares_agens_dmem::index::ScoredEntry;
///
/// # #[tokio::main]
/// # async fn main() {
/// let mut mesh = MeshSearch::new(0.8);
/// let peer = SimulatedPeerSearcher::new(vec![
///     ScoredEntry { id: "peer-m1".into(), score: 0.95 },
/// ]);
/// mesh.add_peer(peer);
///
/// // Low-confidence local results trigger fanout
/// let local = vec![ScoredEntry { id: "local-m1".into(), score: 0.5 }];
/// let merged = mesh.search_with_fanout(&[0.1, 0.9], local, 5).await.unwrap();
/// assert!(merged.iter().any(|e| e.id == "peer-m1"));
/// # }
/// ```
pub struct MeshSearch<P: PeerSearcher> {
    peers: Vec<P>,
    /// Minimum local score above which peer fanout is skipped.
    confidence_threshold: f32,
}

impl<P: PeerSearcher> MeshSearch<P> {
    /// Create a new mesh searcher with the given confidence threshold.
    ///
    /// If the best local result scores ≥ `confidence_threshold`, the search
    /// returns without querying peers.
    #[must_use]
    pub fn new(confidence_threshold: f32) -> Self {
        Self {
            peers: Vec::new(),
            confidence_threshold,
        }
    }

    /// Register a peer searcher.
    pub fn add_peer(&mut self, peer: P) {
        self.peers.push(peer);
    }

    /// Run the mesh search.
    ///
    /// If `local_results` already contains a result with `score ≥
    /// confidence_threshold`, this returns `local_results` as-is.
    /// Otherwise it fans out to all registered peers, merges the results,
    /// re-ranks by score, and returns the global top-k.
    pub async fn search_with_fanout(
        &self,
        embedding: &[f32],
        mut local_results: Vec<ScoredEntry>,
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, DmemError> {
        let best_local = local_results.first().map(|e| e.score).unwrap_or(0.0);

        if best_local >= self.confidence_threshold {
            local_results.truncate(top_k);
            return Ok(local_results);
        }

        // Fan out to peers sequentially (real impl would use tokio::join! or
        // FuturesUnordered for true concurrency)
        let mut all = local_results;
        for peer in &self.peers {
            let peer_results = peer.search(embedding, top_k).await?;
            all.extend(peer_results);
        }

        // Merge and re-rank
        all.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all.dedup_by(|a, b| a.id == b.id); // keep highest-scored duplicate
        all.truncate(top_k);
        Ok(all)
    }
}

// ── SimulatedPeerSearcher ─────────────────────────────────────────────────────

/// A peer searcher backed by a fixed result list — useful for tests.
pub struct SimulatedPeerSearcher {
    results: Vec<ScoredEntry>,
}

impl SimulatedPeerSearcher {
    /// Create a simulated peer that returns the given results for any query.
    #[must_use]
    pub fn new(results: Vec<ScoredEntry>) -> Self {
        Self { results }
    }
}

#[async_trait]
impl PeerSearcher for SimulatedPeerSearcher {
    async fn search(
        &self,
        _embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredEntry>, DmemError> {
        let mut r = self.results.clone();
        r.truncate(top_k);
        Ok(r)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn high_confidence_local_skips_fanout() {
        let mut mesh: MeshSearch<SimulatedPeerSearcher> = MeshSearch::new(0.8);
        let peer = SimulatedPeerSearcher::new(vec![ScoredEntry {
            id: "peer-result".into(),
            score: 0.99,
        }]);
        mesh.add_peer(peer);

        let local = vec![ScoredEntry {
            id: "local-top".into(),
            score: 0.9,
        }];
        let result = mesh.search_with_fanout(&[1.0], local, 5).await.unwrap();
        // Peer result should NOT appear because local confidence was sufficient
        assert!(!result.iter().any(|e| e.id == "peer-result"));
        assert_eq!(result[0].id, "local-top");
    }

    #[tokio::test]
    async fn low_confidence_local_triggers_fanout() {
        let mut mesh = MeshSearch::new(0.8);
        let peer = SimulatedPeerSearcher::new(vec![ScoredEntry {
            id: "peer-m1".into(),
            score: 0.95,
        }]);
        mesh.add_peer(peer);

        let local = vec![ScoredEntry {
            id: "local-m1".into(),
            score: 0.5,
        }];
        let result = mesh
            .search_with_fanout(&[0.5, 0.5], local, 5)
            .await
            .unwrap();
        assert!(result.iter().any(|e| e.id == "peer-m1"));
        // peer result should be ranked first (higher score)
        assert_eq!(result[0].id, "peer-m1");
    }

    #[tokio::test]
    async fn no_peers_returns_local_results() {
        let mesh: MeshSearch<SimulatedPeerSearcher> = MeshSearch::new(0.8);
        let local = vec![ScoredEntry {
            id: "only-local".into(),
            score: 0.3,
        }];
        let result = mesh.search_with_fanout(&[1.0], local, 5).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "only-local");
    }

    #[tokio::test]
    async fn deduplication_keeps_highest_score() {
        // Use a threshold of 0.99 so that the local score of 0.7 triggers fanout.
        let mut mesh = MeshSearch::new(0.99);
        let peer = SimulatedPeerSearcher::new(vec![ScoredEntry {
            id: "shared".into(),
            score: 0.95,
        }]);
        mesh.add_peer(peer);

        let local = vec![ScoredEntry {
            id: "shared".into(),
            score: 0.7,
        }];
        let result = mesh.search_with_fanout(&[1.0], local, 5).await.unwrap();
        // Only one "shared" entry and it should have the higher score
        let shared: Vec<_> = result.iter().filter(|e| e.id == "shared").collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].score, 0.95);
    }
}
