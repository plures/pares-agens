//! TTL-aware in-memory cache for Arca.

use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ArcaError;

// ── CacheEntry ────────────────────────────────────────────────────────────────

/// A single entry stored in the [`CacheStore`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    /// The cached value.
    pub value: Value,

    /// UTC timestamp at which this entry was inserted.
    pub inserted_at: DateTime<Utc>,

    /// Optional UTC timestamp after which this entry is considered expired.
    pub expires_at: Option<DateTime<Utc>>,
}

impl CacheEntry {
    /// Return `true` when the entry has passed its expiry time.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| Utc::now() > exp)
            .unwrap_or(false)
    }
}

// ── CacheStore ────────────────────────────────────────────────────────────────

/// In-memory key-value cache with optional per-entry TTL.
///
/// Entries are plain [`serde_json::Value`] so that any serialisable type can
/// be cached without requiring a concrete type parameter.
///
/// # Example
///
/// ```
/// use pares_agens_arca::cache::CacheStore;
/// use serde_json::json;
///
/// let mut cache = CacheStore::new();
/// cache.insert("greeting", json!("hello"), None);
/// let entry = cache.get("greeting").unwrap();
/// assert_eq!(entry.value, json!("hello"));
/// ```
#[derive(Debug, Default)]
pub struct CacheStore {
    entries: HashMap<String, CacheEntry>,
}

impl CacheStore {
    /// Create an empty `CacheStore`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert `value` under `key`.
    ///
    /// If `ttl_secs` is `Some(n)`, the entry will expire after `n` seconds.
    pub fn insert(&mut self, key: &str, value: Value, ttl_secs: Option<u64>) {
        let expires_at = ttl_secs.map(|secs| Utc::now() + Duration::seconds(secs as i64));
        let entry = CacheEntry {
            value,
            inserted_at: Utc::now(),
            expires_at,
        };
        self.entries.insert(key.to_string(), entry);
    }

    /// Retrieve the entry stored under `key`.
    ///
    /// Returns [`ArcaError::NotFound`] when the key does not exist and
    /// [`ArcaError::Expired`] when the entry has passed its TTL.
    ///
    /// # Errors
    ///
    /// See [`ArcaError`].
    pub fn get(&self, key: &str) -> Result<&CacheEntry, ArcaError> {
        let entry = self
            .entries
            .get(key)
            .ok_or_else(|| ArcaError::NotFound(key.to_string()))?;
        if entry.is_expired() {
            return Err(ArcaError::Expired(key.to_string()));
        }
        Ok(entry)
    }

    /// Remove the entry stored under `key`, returning it if it existed.
    pub fn remove(&mut self, key: &str) -> Option<CacheEntry> {
        self.entries.remove(key)
    }

    /// Evict all entries that have passed their TTL.
    ///
    /// Returns the number of entries evicted.
    pub fn evict_expired(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|_, v| !v.is_expired());
        before - self.entries.len()
    }

    /// Return the total number of entries (including expired ones not yet evicted).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when the cache contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn insert_and_get_returns_value() {
        let mut cache = CacheStore::new();
        cache.insert("key1", json!(42), None);
        let entry = cache.get("key1").unwrap();
        assert_eq!(entry.value, json!(42));
    }

    #[test]
    fn get_missing_key_returns_not_found() {
        let cache = CacheStore::new();
        assert!(matches!(cache.get("missing"), Err(ArcaError::NotFound(_))));
    }

    #[test]
    fn expired_entry_returns_expired_error() {
        let mut cache = CacheStore::new();
        // Insert with TTL of 0 seconds — immediately expired.
        cache.insert("stale", json!("old"), Some(0));
        // Give the clock one tick.
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert!(matches!(cache.get("stale"), Err(ArcaError::Expired(_))));
    }

    #[test]
    fn evict_expired_removes_stale_entries() {
        let mut cache = CacheStore::new();
        cache.insert("a", json!(1), Some(0));
        cache.insert("b", json!(2), None);
        std::thread::sleep(std::time::Duration::from_millis(1));
        let evicted = cache.evict_expired();
        assert_eq!(evicted, 1);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn remove_deletes_entry() {
        let mut cache = CacheStore::new();
        cache.insert("x", json!("hello"), None);
        assert!(cache.remove("x").is_some());
        assert!(matches!(cache.get("x"), Err(ArcaError::NotFound(_))));
    }

    #[test]
    fn is_empty_reflects_state() {
        let mut cache = CacheStore::new();
        assert!(cache.is_empty());
        cache.insert("y", json!(true), None);
        assert!(!cache.is_empty());
    }
}
