//! Secure secret vault for Arca.
//!
//! [`Vault`] stores secrets (API keys, tokens, credentials) in memory under
//! named slots.  All secret values are kept as opaque byte vectors; callers
//! are responsible for serialising/deserialising the raw bytes.
//!
//! In this MVP the vault is purely in-memory.  A future version will add
//! at-rest encryption backed by the OS keyring or an AES-GCM encrypted file.

use std::collections::HashMap;

use crate::ArcaError;

// ── SecretEntry ───────────────────────────────────────────────────────────────

/// A single secret stored in the [`Vault`].
#[derive(Debug, Clone)]
pub struct SecretEntry {
    /// The raw secret bytes (e.g. UTF-8 encoded API key).
    pub bytes: Vec<u8>,

    /// Optional human-readable description of this secret.
    pub description: Option<String>,
}

// ── Vault ─────────────────────────────────────────────────────────────────────

/// In-memory vault for named secrets.
///
/// # Example
///
/// ```
/// use pares_agens_arca::vault::Vault;
///
/// let mut vault = Vault::new();
/// vault.store("openai_key", b"sk-abc123".to_vec(), Some("OpenAI API key".to_string())).unwrap();
/// let entry = vault.retrieve("openai_key").unwrap();
/// assert_eq!(entry.bytes, b"sk-abc123");
/// ```
#[derive(Debug, Default)]
pub struct Vault {
    secrets: HashMap<String, SecretEntry>,
    /// Maximum byte length allowed for a single secret value.
    max_secret_bytes: usize,
}

/// Default upper bound for a single stored secret (64 KiB).
const DEFAULT_MAX_SECRET_BYTES: usize = 65_536;

impl Vault {
    /// Create an empty `Vault` with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            secrets: HashMap::new(),
            max_secret_bytes: DEFAULT_MAX_SECRET_BYTES,
        }
    }

    /// Store `bytes` under `name`, with an optional description.
    ///
    /// If a secret already exists under `name`, it is overwritten.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::ValueTooLarge`] if `bytes` exceeds the
    /// configured limit.
    pub fn store(
        &mut self,
        name: &str,
        bytes: Vec<u8>,
        description: Option<String>,
    ) -> Result<(), ArcaError> {
        if bytes.len() > self.max_secret_bytes {
            return Err(ArcaError::ValueTooLarge(format!(
                "{name}: {} bytes exceeds limit of {}",
                bytes.len(),
                self.max_secret_bytes
            )));
        }
        self.secrets
            .insert(name.to_string(), SecretEntry { bytes, description });
        Ok(())
    }

    /// Retrieve the [`SecretEntry`] stored under `name`.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::NotFound`] when no secret exists under `name`.
    pub fn retrieve(&self, name: &str) -> Result<&SecretEntry, ArcaError> {
        self.secrets
            .get(name)
            .ok_or_else(|| ArcaError::NotFound(name.to_string()))
    }

    /// Delete the secret stored under `name`.
    ///
    /// Returns `true` if a secret was removed, `false` if `name` was not found.
    pub fn delete(&mut self, name: &str) -> bool {
        self.secrets.remove(name).is_some()
    }

    /// Return `true` when the vault holds no secrets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Return the number of secrets stored in the vault.
    #[must_use]
    pub fn len(&self) -> usize {
        self.secrets.len()
    }

    /// Return the names of all stored secrets (order not guaranteed).
    pub fn list_names(&self) -> Vec<&str> {
        self.secrets.keys().map(String::as_str).collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_retrieve_secret() {
        let mut vault = Vault::new();
        vault
            .store("key", b"secret".to_vec(), Some("test key".to_string()))
            .unwrap();
        let entry = vault.retrieve("key").unwrap();
        assert_eq!(entry.bytes, b"secret");
        assert_eq!(entry.description.as_deref(), Some("test key"));
    }

    #[test]
    fn retrieve_missing_key_returns_not_found() {
        let vault = Vault::new();
        assert!(matches!(vault.retrieve("missing"), Err(ArcaError::NotFound(_))));
    }

    #[test]
    fn store_oversized_secret_returns_value_too_large() {
        let mut vault = Vault::new();
        let big = vec![0u8; DEFAULT_MAX_SECRET_BYTES + 1];
        assert!(matches!(
            vault.store("big", big, None),
            Err(ArcaError::ValueTooLarge(_))
        ));
    }

    #[test]
    fn delete_removes_secret() {
        let mut vault = Vault::new();
        vault.store("k", b"v".to_vec(), None).unwrap();
        assert!(vault.delete("k"));
        assert!(matches!(vault.retrieve("k"), Err(ArcaError::NotFound(_))));
    }

    #[test]
    fn delete_missing_returns_false() {
        let mut vault = Vault::new();
        assert!(!vault.delete("no_such_key"));
    }

    #[test]
    fn list_names_reflects_stored_secrets() {
        let mut vault = Vault::new();
        vault.store("alpha", b"1".to_vec(), None).unwrap();
        vault.store("beta", b"2".to_vec(), None).unwrap();
        let mut names = vault.list_names();
        names.sort_unstable();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn overwrite_secret_replaces_value() {
        let mut vault = Vault::new();
        vault.store("k", b"old".to_vec(), None).unwrap();
        vault.store("k", b"new".to_vec(), None).unwrap();
        let entry = vault.retrieve("k").unwrap();
        assert_eq!(entry.bytes, b"new");
    }
}
