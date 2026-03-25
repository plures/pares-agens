//! Memory pinning registry.
//!
//! Users can pin critical memories so they are **always** present on every
//! device regardless of the device's capacity tier or eviction pressure.
//!
//! [`PinRegistry`] tracks which memory IDs are pinned and propagates pin
//! changes to the local [`MemoryCache`] and eviction tracker.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

// ── PinRegistry ───────────────────────────────────────────────────────────────

/// Tracks user-pinned memory IDs.
///
/// # Example
///
/// ```rust
/// use pares_agens_dmem::pin::PinRegistry;
///
/// let mut reg = PinRegistry::new();
/// reg.pin("important-memory-id");
/// assert!(reg.is_pinned("important-memory-id"));
///
/// reg.unpin("important-memory-id");
/// assert!(!reg.is_pinned("important-memory-id"));
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PinRegistry {
    pinned: HashSet<String>,
}

impl PinRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin a memory entry by ID.
    ///
    /// Has no effect if the entry is already pinned.
    pub fn pin(&mut self, id: &str) {
        self.pinned.insert(id.to_owned());
    }

    /// Unpin a memory entry.
    ///
    /// Has no effect if the entry is not pinned.
    pub fn unpin(&mut self, id: &str) {
        self.pinned.remove(id);
    }

    /// Return `true` if the given ID is pinned.
    #[must_use]
    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }

    /// Return an iterator over all currently pinned IDs.
    pub fn pinned_ids(&self) -> impl Iterator<Item = &str> {
        self.pinned.iter().map(String::as_str)
    }

    /// The number of pinned entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pinned.len()
    }

    /// `true` if no entries are pinned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pinned.is_empty()
    }

    /// Serialise the registry to JSON for sync across devices.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if serialisation fails (should never
    /// happen for a `HashSet<String>`).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialise a registry from JSON.
    ///
    /// # Errors
    ///
    /// Returns a `serde_json::Error` if the input is malformed.
    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }

    /// Merge another registry into this one (union of pinned IDs).
    ///
    /// Used for CRDT-style merge during P2P sync.
    pub fn merge(&mut self, other: &PinRegistry) {
        for id in &other.pinned {
            self.pinned.insert(id.clone());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_and_is_pinned() {
        let mut r = PinRegistry::new();
        assert!(!r.is_pinned("x"));
        r.pin("x");
        assert!(r.is_pinned("x"));
    }

    #[test]
    fn unpin_removes() {
        let mut r = PinRegistry::new();
        r.pin("y");
        r.unpin("y");
        assert!(!r.is_pinned("y"));
    }

    #[test]
    fn len() {
        let mut r = PinRegistry::new();
        r.pin("a");
        r.pin("b");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn json_roundtrip() {
        let mut r = PinRegistry::new();
        r.pin("mem-1");
        r.pin("mem-2");
        let json = r.to_json().unwrap();
        let r2 = PinRegistry::from_json(&json).unwrap();
        assert!(r2.is_pinned("mem-1"));
        assert!(r2.is_pinned("mem-2"));
    }

    #[test]
    fn merge_is_union() {
        let mut a = PinRegistry::new();
        a.pin("x");
        let mut b = PinRegistry::new();
        b.pin("y");
        a.merge(&b);
        assert!(a.is_pinned("x"));
        assert!(a.is_pinned("y"));
    }
}
