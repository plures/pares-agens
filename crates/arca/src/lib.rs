//! `pares-agens-arca` — Cache and vault subsystem for Pares Agens.
//!
//! Provides an in-process key-value cache with optional TTL expiry and a
//! secure vault for storing secrets (API keys, tokens, credentials).  Both
//! components are designed to be the local-first storage primitives for the
//! Pares App Runtime.
//!
//! # Modules
//!
//! - [`cache`] — [`CacheStore`](cache::CacheStore): TTL-aware in-memory cache.
//! - [`vault`] — [`Vault`](vault::Vault): secure secret storage.

pub mod cache;
pub mod vault;

use thiserror::Error;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors that can occur during Arca cache or vault operations.
#[derive(Debug, Error)]
pub enum ArcaError {
    /// A key was not found in the cache or vault.
    #[error("key not found: {0}")]
    NotFound(String),

    /// The requested cache entry has expired and been evicted.
    #[error("cache entry expired: {0}")]
    Expired(String),

    /// An attempt was made to store a value that exceeds size limits.
    #[error("value too large: {0}")]
    ValueTooLarge(String),

    /// Vault encryption or decryption failed.
    #[error("crypto error: {0}")]
    CryptoError(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
