//! Context-driven prefetch predictor.
//!
//! [`PrefetchPredictor`] analyses the current *context tags* (e.g. the name of
//! the open project, active application, recent topics) and predicts which
//! memory entries are likely to be needed soon.  The predictions can then be
//! used by the [`crate::manager::DistributedMemoryManager`] to warm the local
//! cache ahead of an explicit query.

use std::collections::HashMap;

// ── PrefetchHint ──────────────────────────────────────────────────────────────

/// A predicted memory entry that should be prefetched.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefetchHint {
    /// ID of the memory entry to prefetch.
    pub memory_id: String,
    /// Confidence that this entry will be needed, in `[0, 1]`.
    pub confidence: f32,
}

// ── PrefetchPredictor ─────────────────────────────────────────────────────────

/// Predicts which memories to prefetch based on the current context.
///
/// The predictor builds an inverted index from *context tag* → list of memory
/// IDs that co-occurred with that tag in past access patterns.  When a new
/// context is observed the predictor looks up all matching IDs and scores them
/// by their co-occurrence frequency.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::prefetch::PrefetchPredictor;
///
/// let mut p = PrefetchPredictor::new();
/// // Record that memory "m1" was accessed when the "project:axle" context was active
/// p.record_access("m1", &["project:axle", "app:vscode"]);
/// p.record_access("m2", &["project:axle"]);
///
/// // When context switches to project:axle, predict m1 and m2
/// let hints = p.predict(&["project:axle"], 5);
/// assert_eq!(hints.len(), 2);
/// assert!(hints.iter().any(|h| h.memory_id == "m1"));
/// ```
#[derive(Debug, Default)]
pub struct PrefetchPredictor {
    /// tag → (memory_id → co-occurrence count)
    index: HashMap<String, HashMap<String, u32>>,
}

impl PrefetchPredictor {
    /// Create an empty predictor.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `memory_id` was accessed while `context_tags` were active.
    ///
    /// This updates the internal co-occurrence index so future calls to
    /// [`predict`] can return this entry when the same tags are seen.
    pub fn record_access(&mut self, memory_id: &str, context_tags: &[&str]) {
        for &tag in context_tags {
            self.index
                .entry(tag.to_owned())
                .or_default()
                .entry(memory_id.to_owned())
                .and_modify(|c| *c += 1)
                .or_insert(1);
        }
    }

    /// Return up to `top_k` memory IDs predicted to be needed given
    /// `active_tags`, sorted by descending confidence.
    ///
    /// Confidence is computed as the sum of co-occurrence counts across all
    /// active tags, normalised by the maximum observed sum.
    #[must_use]
    pub fn predict(&self, active_tags: &[&str], top_k: usize) -> Vec<PrefetchHint> {
        let mut scores: HashMap<&str, u32> = HashMap::new();

        for &tag in active_tags {
            if let Some(memories) = self.index.get(tag) {
                for (id, &count) in memories {
                    *scores.entry(id.as_str()).or_insert(0) += count;
                }
            }
        }

        if scores.is_empty() {
            return Vec::new();
        }

        let max_score = *scores.values().max().unwrap_or(&1) as f32;

        let mut hints: Vec<PrefetchHint> = scores
            .into_iter()
            .map(|(id, score)| PrefetchHint {
                memory_id: id.to_owned(),
                confidence: score as f32 / max_score,
            })
            .collect();

        hints.sort_by(|a, b| {
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        hints.truncate(top_k);
        hints
    }

    /// Clear all recorded co-occurrence data.
    pub fn reset(&mut self) {
        self.index.clear();
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predict_returns_relevant_ids() {
        let mut p = PrefetchPredictor::new();
        p.record_access("m1", &["project:axle", "app:vscode"]);
        p.record_access("m2", &["project:axle"]);
        let hints = p.predict(&["project:axle"], 5);
        let ids: Vec<&str> = hints.iter().map(|h| h.memory_id.as_str()).collect();
        assert!(ids.contains(&"m1"));
        assert!(ids.contains(&"m2"));
    }

    #[test]
    fn predict_empty_when_no_matching_tags() {
        let mut p = PrefetchPredictor::new();
        p.record_access("m1", &["project:axle"]);
        let hints = p.predict(&["project:unknown"], 5);
        assert!(hints.is_empty());
    }

    #[test]
    fn top_k_respected() {
        let mut p = PrefetchPredictor::new();
        for i in 0..10 {
            p.record_access(&format!("m{i}"), &["tag:common"]);
        }
        let hints = p.predict(&["tag:common"], 3);
        assert_eq!(hints.len(), 3);
    }

    #[test]
    fn confidence_normalised_to_one() {
        let mut p = PrefetchPredictor::new();
        // Access m1 three times, m2 once
        for _ in 0..3 {
            p.record_access("m1", &["t"]);
        }
        p.record_access("m2", &["t"]);
        let hints = p.predict(&["t"], 5);
        let top = hints.iter().find(|h| h.memory_id == "m1").unwrap();
        assert!((top.confidence - 1.0).abs() < f32::EPSILON);
        let second = hints.iter().find(|h| h.memory_id == "m2").unwrap();
        assert!(second.confidence < 1.0);
    }

    #[test]
    fn reset_clears_index() {
        let mut p = PrefetchPredictor::new();
        p.record_access("m1", &["t"]);
        p.reset();
        assert!(p.predict(&["t"], 5).is_empty());
    }
}
