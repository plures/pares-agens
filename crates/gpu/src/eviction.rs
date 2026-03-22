//! Eviction policy abstraction and LRU implementation.
//!
//! The pool consults an [`EvictionPolicy`] whenever it needs to free a model
//! slot.  The built-in [`LruEviction`] evicts the least-recently-used model;
//! custom policies can be plugged in via the trait.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// ── EvictionPolicyKind ────────────────────────────────────────────────────────

/// Identifier for a built-in eviction strategy, used in [`crate::GpuConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvictionPolicyKind {
    /// Evict the model that was least recently used (default).
    #[default]
    Lru,
}

// ── EvictionPolicy trait ──────────────────────────────────────────────────────

/// Pluggable eviction strategy.
///
/// The pool calls [`EvictionPolicy::record_access`] on every inference and
/// [`EvictionPolicy::evict_candidate`] when a new model must be loaded but the
/// pool is full.
pub trait EvictionPolicy: Send + Sync {
    /// Record that `model_id` was just accessed.
    fn record_access(&mut self, model_id: &str);

    /// Return the `model_id` that should be evicted next, or `None` if the
    /// tracker has no candidates (empty).
    fn evict_candidate(&mut self) -> Option<String>;

    /// Remove `model_id` from the tracker (called after the pool evicts it).
    fn remove(&mut self, model_id: &str);

    /// Register a newly loaded model.
    fn insert(&mut self, model_id: &str);
}

// ── LruEviction ──────────────────────────────────────────────────────────────

/// Least-recently-used eviction policy.
///
/// Models are stored in a `VecDeque` ordered from least-recently-used (front)
/// to most-recently-used (back).  Eviction always picks the front element.
///
/// # Example
/// ```rust
/// use pares_agens_gpu::eviction::{EvictionPolicy, LruEviction};
///
/// let mut lru = LruEviction::default();
/// lru.insert("model-a");
/// lru.insert("model-b");
/// lru.record_access("model-a");           // a is now most-recently-used
/// assert_eq!(lru.evict_candidate().as_deref(), Some("model-b")); // b is LRU
/// ```
#[derive(Debug, Default)]
pub struct LruEviction {
    order: VecDeque<String>,
}

impl EvictionPolicy for LruEviction {
    fn record_access(&mut self, model_id: &str) {
        if let Some(pos) = self.order.iter().position(|id| id == model_id) {
            let id = self.order.remove(pos).unwrap();
            self.order.push_back(id);
        }
    }

    fn evict_candidate(&mut self) -> Option<String> {
        self.order.front().cloned()
    }

    fn remove(&mut self, model_id: &str) {
        self.order.retain(|id| id != model_id);
    }

    fn insert(&mut self, model_id: &str) {
        if !self.order.iter().any(|id| id == model_id) {
            self.order.push_back(model_id.to_owned());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_evicts_least_recently_used() {
        let mut lru = LruEviction::default();
        lru.insert("a");
        lru.insert("b");
        lru.insert("c");

        lru.record_access("a"); // order: b, c, a
        lru.record_access("c"); // order: b, a, c

        // Least recently used is "b".
        assert_eq!(lru.evict_candidate().as_deref(), Some("b"));
    }

    #[test]
    fn lru_evict_candidate_is_none_when_empty() {
        let mut lru = LruEviction::default();
        assert_eq!(lru.evict_candidate(), None);
    }

    #[test]
    fn lru_remove_eliminates_model() {
        let mut lru = LruEviction::default();
        lru.insert("a");
        lru.insert("b");
        lru.remove("a");

        assert_eq!(lru.evict_candidate().as_deref(), Some("b"));
    }

    #[test]
    fn lru_insert_is_idempotent() {
        let mut lru = LruEviction::default();
        lru.insert("a");
        lru.insert("a"); // duplicate insert
        lru.insert("b");

        lru.remove("a");
        // Only "b" remains.
        assert_eq!(lru.evict_candidate().as_deref(), Some("b"));
        lru.remove("b");
        assert_eq!(lru.evict_candidate(), None);
    }

    #[test]
    fn lru_record_access_promotes_to_mru() {
        let mut lru = LruEviction::default();
        lru.insert("x");
        lru.insert("y");
        lru.record_access("x"); // x becomes MRU

        // y is now LRU
        assert_eq!(lru.evict_candidate().as_deref(), Some("y"));
    }

    #[test]
    fn eviction_policy_kind_default_is_lru() {
        assert!(matches!(EvictionPolicyKind::default(), EvictionPolicyKind::Lru));
    }

    #[test]
    fn eviction_policy_kind_roundtrips_json() {
        let kind = EvictionPolicyKind::Lru;
        let json = serde_json::to_string(&kind).unwrap();
        let back: EvictionPolicyKind = serde_json::from_str(&json).unwrap();
        assert_eq!(kind, back);
    }
}
