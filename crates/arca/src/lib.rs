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
//! - [`vault`] — [`Vault`](vault::Vault): legacy simple in-memory vault.
//!   [`CredentialVault`](vault::CredentialVault): encrypted vault with
//!   key-wrapping and lock/unlock lifecycle.
//! - [`cli`] — [`VaultCommand`](cli::VaultCommand): CLI sub-commands for vault
//!   operations (`lock`, `unlock`, `rotate`).

pub mod cache;
pub mod cli;
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

    /// Operation requires the vault to be unlocked first.
    #[error("vault is locked — call unlock() with the master password first")]
    VaultLocked,

    /// Vault has not been initialised yet.
    #[error("vault not initialised — call initialise() with a master password first")]
    NotInitialised,

    /// Vault is already initialised; cannot initialise again.
    #[error("vault already initialised")]
    AlreadyInitialised,

    /// Vault is already unlocked.
    #[error("vault is already unlocked")]
    AlreadyUnlocked,

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
