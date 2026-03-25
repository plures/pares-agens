//! Smart eviction — LRU weighted by access frequency, relevance score, and recency.
//!
//! [`SmartEviction`] maintains per-entry statistics and computes a composite
//! **eviction score** for each unpinned entry.  The entry with the *lowest*
//! eviction score is chosen as the eviction candidate.
//!
//! ## Eviction score formula
//!
//! ```text
//! score = (w_recency   * recency_score)
//!       + (w_frequency * frequency_score)
//!       + (w_relevance * relevance_score)
//! ```
//!
//! Where each component is normalised to `[0, 1]` and the weights sum to 1.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ── EvictionWeights ───────────────────────────────────────────────────────────

/// Weights for the composite eviction score.
///
/// All three fields must sum to approximately 1.0.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvictionWeights {
    /// Weight given to how recently the entry was accessed.
    pub recency: f32,
    /// Weight given to how frequently the entry has been accessed.
    pub frequency: f32,
    /// Weight given to the semantic relevance score of the entry.
    pub relevance: f32,
}

impl Default for EvictionWeights {
    fn default() -> Self {
        Self {
            recency: 0.5,
            frequency: 0.3,
            relevance: 0.2,
        }
    }
}

// ── EntryStats ────────────────────────────────────────────────────────────────

/// Per-entry access statistics used by [`SmartEviction`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryStats {
    /// Monotonically increasing logical clock of the last access.
    pub last_access_tick: u64,
    /// Total number of times this entry has been accessed.
    pub access_count: u64,
    /// Semantic relevance score in `[0, 1]` set by the embedding search.
    pub relevance_score: f32,
    /// If `true` the entry will never be evicted regardless of score.
    pub pinned: bool,
}

impl EntryStats {
    /// Create initial stats for a newly inserted entry.
    #[must_use]
    pub fn new(tick: u64) -> Self {
        Self {
            last_access_tick: tick,
            access_count: 1,
            relevance_score: 0.0,
            pinned: false,
        }
    }
}

// ── SmartEviction ─────────────────────────────────────────────────────────────

/// Maintains per-entry stats and selects the best eviction candidate.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::eviction::SmartEviction;
///
/// let mut ev = SmartEviction::new();
/// ev.track("entry-a", 0.9);  // high relevance — keep
/// ev.track("entry-b", 0.1);  // low relevance — likely evict
/// ev.touch("entry-a");
///
/// let candidate = ev.evict_candidate();
/// assert_eq!(candidate, Some("entry-b"));
/// ```
#[derive(Debug, Default)]
pub struct SmartEviction {
    stats: HashMap<String, EntryStats>,
    tick: u64,
    weights: EvictionWeights,
}

impl SmartEviction {
    /// Create a new eviction tracker with default weights.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a tracker with custom eviction weights.
    #[must_use]
    pub fn with_weights(weights: EvictionWeights) -> Self {
        Self {
            weights,
            ..Default::default()
        }
    }

    /// Register a new entry with an initial relevance score.
    ///
    /// If the entry already exists, this is a no-op — use [`touch`] to refresh
    /// an existing entry.
    pub fn track(&mut self, id: &str, relevance_score: f32) {
        self.tick += 1;
        self.stats.entry(id.to_owned()).or_insert_with(|| {
            let mut s = EntryStats::new(self.tick);
            s.relevance_score = relevance_score;
            s
        });
    }

    /// Record an access for `id`, marking it as recently used.
    pub fn touch(&mut self, id: &str) {
        self.tick += 1;
        if let Some(s) = self.stats.get_mut(id) {
            s.last_access_tick = self.tick;
            s.access_count += 1;
        }
    }

    /// Update the relevance score for an entry (e.g. after a new search query).
    pub fn set_relevance(&mut self, id: &str, score: f32) {
        if let Some(s) = self.stats.get_mut(id) {
            s.relevance_score = score;
        }
    }

    /// Pin an entry so it is never selected as an eviction candidate.
    pub fn pin(&mut self, id: &str) {
        if let Some(s) = self.stats.get_mut(id) {
            s.pinned = true;
        }
    }

    /// Unpin a previously pinned entry.
    pub fn unpin(&mut self, id: &str) {
        if let Some(s) = self.stats.get_mut(id) {
            s.pinned = false;
        }
    }

    /// Remove an entry from tracking (after successful eviction).
    pub fn remove(&mut self, id: &str) {
        self.stats.remove(id);
    }

    /// Return the ID of the best eviction candidate (lowest composite score),
    /// excluding pinned entries.
    ///
    /// Returns `None` if all tracked entries are pinned or no entries exist.
    #[must_use]
    pub fn evict_candidate(&self) -> Option<&str> {
        let max_tick = self.tick.max(1) as f32;
        let max_count = self
            .stats
            .values()
            .map(|s| s.access_count)
            .max()
            .unwrap_or(1)
            .max(1) as f32;

        self.stats
            .iter()
            .filter(|(_, s)| !s.pinned)
            .min_by(|(_, a), (_, b)| {
                let score_a = self.composite_score(a, max_tick, max_count);
                let score_b = self.composite_score(b, max_tick, max_count);
                score_a
                    .partial_cmp(&score_b)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(id, _)| id.as_str())
    }

    /// Return the composite eviction score for an entry.
    ///
    /// Higher = more valuable = less likely to be evicted.
    fn composite_score(&self, stats: &EntryStats, max_tick: f32, max_count: f32) -> f32 {
        let recency = stats.last_access_tick as f32 / max_tick;
        let frequency = stats.access_count as f32 / max_count;
        let relevance = stats.relevance_score.clamp(0.0, 1.0);

        self.weights.recency * recency
            + self.weights.frequency * frequency
            + self.weights.relevance * relevance
    }

    /// The number of tracked entries (including pinned ones).
    #[must_use]
    pub fn len(&self) -> usize {
        self.stats.len()
    }

    /// `true` if no entries are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stats.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evict_candidate_returns_lowest_score() {
        let mut ev = SmartEviction::new();
        ev.track("a", 0.9); // high relevance
        ev.track("b", 0.1); // low relevance
        // Touch a several times so it's also more frequent and recent
        ev.touch("a");
        ev.touch("a");

        // b has lower relevance, lower frequency, older access → evict b
        assert_eq!(ev.evict_candidate(), Some("b"));
    }

    #[test]
    fn pinned_entries_are_never_candidates() {
        let mut ev = SmartEviction::new();
        ev.track("only-entry", 0.0);
        ev.pin("only-entry");
        assert_eq!(ev.evict_candidate(), None);
    }

    #[test]
    fn unpin_restores_candidacy() {
        let mut ev = SmartEviction::new();
        ev.track("e", 0.1);
        ev.pin("e");
        assert_eq!(ev.evict_candidate(), None);
        ev.unpin("e");
        assert_eq!(ev.evict_candidate(), Some("e"));
    }

    #[test]
    fn remove_decreases_len() {
        let mut ev = SmartEviction::new();
        ev.track("x", 0.5);
        ev.track("y", 0.5);
        assert_eq!(ev.len(), 2);
        ev.remove("x");
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn set_relevance_affects_score() {
        // Use relevance-heavy weights so relevance is the decisive factor.
        let mut ev = SmartEviction::with_weights(EvictionWeights {
            recency: 0.1,
            frequency: 0.1,
            relevance: 0.8,
        });
        ev.track("a", 0.5);
        ev.track("b", 0.5);
        // Raise a's relevance — b should become the eviction candidate.
        ev.set_relevance("a", 1.0);
        ev.set_relevance("b", 0.0);
        assert_eq!(ev.evict_candidate(), Some("b"));
    }
}
