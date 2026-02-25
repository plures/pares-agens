//! PluresDB state store stub.

use serde_json::Value;

/// Minimal key-value state interface backed by PluresDB.
///
/// A full implementation is provided by the `pares-pluresdb` crate (pending).
#[async_trait::async_trait]
pub trait StateStore: Send + Sync {
    /// Retrieve a value by key.
    async fn get(&self, key: &str) -> Option<Value>;
    /// Store a value under key.
    async fn set(&self, key: &str, value: Value);
}
