//! LRU eviction bookkeeping for [`crate::pool::GpuModelPool`].
//!
//! [`LruEviction`] tracks the access order of loaded model IDs.  It does **not**
//! own the models or interact with VRAM — that is the pool's responsibility.

use std::collections::VecDeque;

/// Tracks model-access order for LRU eviction.
///
/// Internally maintains a `VecDeque` where the **front** is the least recently
/// used model and the **back** is the most recently used.
///
/// # Example
///
/// ```rust
/// use pares_agens_gpu::LruEviction;
///
/// let mut lru = LruEviction::new();
/// lru.insert("model-a");
/// lru.insert("model-b");
/// lru.touch("model-a");   // model-a is now MRU
///
/// // model-b is now the LRU candidate
/// assert_eq!(lru.evict_candidate(), Some("model-b"));
/// ```
#[derive(Debug, Default)]
pub struct LruEviction {
    /// Ordered list of model IDs; front = LRU, back = MRU.
    order: VecDeque<String>,
}

impl LruEviction {
    /// Create a new, empty eviction tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a newly loaded model.  The model is added as the MRU entry.
    ///
    /// If `model_id` is already tracked (e.g. loaded twice) this is a no-op —
    /// use [`touch`] to promote an existing entry.
    pub fn insert(&mut self, model_id: &str) {
        if !self.order.iter().any(|id| id == model_id) {
            self.order.push_back(model_id.to_owned());
        }
    }

    /// Mark `model_id` as most recently used.
    ///
    /// Moves the entry to the back of the queue.  If the model is not
    /// currently tracked this is a no-op.
    pub fn touch(&mut self, model_id: &str) {
        if let Some(pos) = self.order.iter().position(|id| id == model_id) {
            let id = self.order.remove(pos).expect("position is valid");
            self.order.push_back(id);
        }
    }

    /// Remove a model from the tracker (e.g. after manual eviction).
    pub fn remove(&mut self, model_id: &str) {
        self.order.retain(|id| id != model_id);
    }

    /// Return the model ID of the least recently used model, if any.
    ///
    /// The returned reference is only valid until the next mutation.
    pub fn evict_candidate(&self) -> Option<&str> {
        self.order.front().map(String::as_str)
    }

    /// The number of tracked models.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// `true` if no models are tracked.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lru_insert_and_evict() {
        let mut lru = LruEviction::new();
        lru.insert("a");
        lru.insert("b");
        lru.insert("c");

        assert_eq!(lru.evict_candidate(), Some("a"));
    }

    #[test]
    fn lru_touch_promotes_to_mru() {
        let mut lru = LruEviction::new();
        lru.insert("a");
        lru.insert("b");
        lru.touch("a");

        // "b" is now LRU
        assert_eq!(lru.evict_candidate(), Some("b"));
    }

    #[test]
    fn lru_remove() {
        let mut lru = LruEviction::new();
        lru.insert("a");
        lru.insert("b");
        lru.remove("a");

        assert_eq!(lru.evict_candidate(), Some("b"));
        assert_eq!(lru.len(), 1);
    }

    #[test]
    fn lru_insert_duplicate_is_noop() {
        let mut lru = LruEviction::new();
        lru.insert("a");
        lru.insert("a");

        assert_eq!(lru.len(), 1);
    }
}
