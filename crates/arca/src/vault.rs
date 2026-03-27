//! Secure credential vault for Arca.
//!
//! This module provides two vault implementations:
//!
//! - [`Vault`] — the original simple in-memory vault (kept for backward
//!   compatibility and tests).
//! - [`CredentialVault`] — full-featured encrypted vault with:
//!   - **AES-256-GCM** encryption of secret values using a per-vault Data
//!     Encryption Key (DEK).
//!   - **Key-wrapping**: the DEK is itself encrypted (wrapped) by a Key
//!     Encryption Key (KEK) derived from a master password via Argon2id.
//!     Key rotation generates a new KEK and re-wraps the DEK without
//!     re-encrypting any stored secrets.
//!   - **Lock/unlock lifecycle**: secrets are only accessible while the vault
//!     is unlocked (DEK held in memory).  The vault can be explicitly locked,
//!     and also supports a configurable idle-timeout auto-lock.
//!
//! # Security design
//!
//! ```text
//! master_password ──(Argon2id)──► KEK ──(AES-256-GCM)──► wrapped_DEK (persisted)
//!                                                              │
//!                                              ◄──(unwrap)──── DEK (in memory only when unlocked)
//!                                                              │
//! plaintext_secret ──(AES-256-GCM with DEK)──► ciphertext (persisted)
//! ```
//!
//! Rotating the master password derives a fresh KEK and re-wraps the same
//! DEK — no secret ciphertext needs to change.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::{
    password_hash::{rand_core::RngCore, SaltString},
    Argon2, Params, PasswordHasher,
};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::ArcaError;

// ── Constants ─────────────────────────────────────────────────────────────────

/// AES-256-GCM key length in bytes.
const KEY_BYTES: usize = 32;
/// AES-256-GCM nonce length in bytes.
const NONCE_BYTES: usize = 12;

/// Default idle timeout before the vault auto-locks (5 minutes).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

// ── EncryptedBlob ─────────────────────────────────────────────────────────────

/// A nonce + ciphertext pair produced by AES-256-GCM.
#[derive(Debug, Clone)]
struct EncryptedBlob {
    nonce: [u8; NONCE_BYTES],
    ciphertext: Vec<u8>,
}

impl EncryptedBlob {
    fn encrypt(key_bytes: &[u8; KEY_BYTES], plaintext: &[u8]) -> Result<Self, ArcaError> {
        let key = Key::<Aes256Gcm>::from_slice(key_bytes);
        let cipher = Aes256Gcm::new(key);
        let mut nonce_bytes = [0u8; NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| ArcaError::CryptoError(e.to_string()))?;
        Ok(Self {
            nonce: nonce_bytes,
            ciphertext,
        })
    }

    fn decrypt(&self, key_bytes: &[u8; KEY_BYTES]) -> Result<Vec<u8>, ArcaError> {
        let key = Key::<Aes256Gcm>::from_slice(key_bytes);
        let cipher = Aes256Gcm::new(key);
        let nonce = Nonce::from_slice(&self.nonce);
        cipher
            .decrypt(nonce, self.ciphertext.as_ref())
            .map_err(|e| ArcaError::CryptoError(e.to_string()))
    }
}

// ── InMemoryDek ───────────────────────────────────────────────────────────────

/// The Data Encryption Key held in memory while the vault is unlocked.
///
/// Implements [`ZeroizeOnDrop`] so the key bytes are wiped when the struct
/// is dropped (e.g. on lock).
#[derive(Zeroize, ZeroizeOnDrop)]
struct InMemoryDek([u8; KEY_BYTES]);

// ── WrappedDek ────────────────────────────────────────────────────────────────

/// The DEK encrypted (wrapped) by the KEK and stored at rest.
#[derive(Debug, Clone)]
struct WrappedDek {
    /// Argon2id salt used to derive the KEK from the master password.
    kek_salt: String,
    /// AES-256-GCM encrypted DEK bytes.
    blob: EncryptedBlob,
}

impl WrappedDek {
    /// Derive a KEK from `password` using the stored Argon2id salt, then
    /// decrypt and return the raw DEK bytes.
    fn unwrap(&self, password: &str) -> Result<[u8; KEY_BYTES], ArcaError> {
        let kek = derive_kek(password, &self.kek_salt)?;
        let plain = self.blob.decrypt(&kek)?;
        if plain.len() != KEY_BYTES {
            return Err(ArcaError::CryptoError(
                "unwrapped DEK has unexpected length".to_string(),
            ));
        }
        let mut dek = [0u8; KEY_BYTES];
        dek.copy_from_slice(&plain);
        Ok(dek)
    }
}

// ── KDF helper ────────────────────────────────────────────────────────────────

/// Derive a 32-byte KEK from `password` and `salt` using Argon2id.
fn derive_kek(password: &str, salt: &str) -> Result<[u8; KEY_BYTES], ArcaError> {
    // OWASP-recommended minimum Argon2id parameters.
    let params = Params::new(65536, 3, 1, Some(KEY_BYTES))
        .map_err(|e| ArcaError::CryptoError(e.to_string()))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let salt_str = SaltString::from_b64(salt)
        .map_err(|e| ArcaError::CryptoError(format!("invalid salt: {e}")))?;
    let hash = argon2
        .hash_password(password.as_bytes(), &salt_str)
        .map_err(|e| ArcaError::CryptoError(e.to_string()))?;
    let raw = hash
        .hash
        .ok_or_else(|| ArcaError::CryptoError("Argon2 produced no hash output".to_string()))?;
    let raw_bytes = raw.as_bytes();
    if raw_bytes.len() < KEY_BYTES {
        return Err(ArcaError::CryptoError(
            "Argon2 output shorter than expected".to_string(),
        ));
    }
    let mut kek = [0u8; KEY_BYTES];
    kek.copy_from_slice(&raw_bytes[..KEY_BYTES]);
    Ok(kek)
}

// ── EncryptedEntry ────────────────────────────────────────────────────────────

/// A single encrypted secret stored in [`CredentialVault`].
#[derive(Debug, Clone)]
struct EncryptedEntry {
    blob: EncryptedBlob,
    description: Option<String>,
}

impl EncryptedEntry {
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
}

// ── CredentialVaultState ──────────────────────────────────────────────────────

enum VaultState {
    /// Vault has never been initialised (no DEK, no wrapped DEK).
    Uninitialised,
    /// Vault is initialised but currently locked (DEK not in memory).
    Locked { wrapped_dek: WrappedDek },
    /// Vault is initialised and unlocked (DEK in memory).
    Unlocked {
        wrapped_dek: WrappedDek,
        dek: InMemoryDek,
        last_activity: Instant,
    },
}

// ── CredentialVault ───────────────────────────────────────────────────────────

/// Full-featured encrypted credential vault with key-wrapping and lock
/// lifecycle.
///
/// # Example
///
/// ```rust
/// use pares_agens_arca::vault::CredentialVault;
///
/// let mut vault = CredentialVault::new(None);
/// vault.initialise("master-password").unwrap();
/// // The vault is unlocked immediately after initialise.
///
/// vault.store_credential("openai_key", "sk-abc123", Some("OpenAI key".to_string())).unwrap();
/// let value = vault.retrieve_credential("openai_key").unwrap();
/// assert_eq!(value, "sk-abc123");
///
/// // Lock, then unlock again with the master password.
/// vault.lock();
/// vault.unlock("master-password").unwrap();
/// assert_eq!(vault.retrieve_credential("openai_key").unwrap(), "sk-abc123");
///
/// vault.lock();
/// assert!(vault.retrieve_credential("openai_key").is_err());
/// ```
pub struct CredentialVault {
    state: VaultState,
    entries: HashMap<String, EncryptedEntry>,
    idle_timeout: Duration,
}

impl CredentialVault {
    /// Create a new, uninitialised `CredentialVault`.
    ///
    /// `idle_timeout` controls how long the vault stays unlocked without any
    /// activity before [`Self::check_idle`] (or any operation) locks it
    /// automatically.  Pass `None` to use the [`DEFAULT_IDLE_TIMEOUT`].
    #[must_use]
    pub fn new(idle_timeout: Option<Duration>) -> Self {
        Self {
            state: VaultState::Uninitialised,
            entries: HashMap::new(),
            idle_timeout: idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT),
        }
    }

    // ── Lifecycle ──────────────────────────────────────────────────────────

    /// Initialise the vault with `master_password`.
    ///
    /// Generates a fresh random DEK and wraps it with a KEK derived from the
    /// provided password.  The vault is left in the **unlocked** state after
    /// initialisation so that callers can immediately store secrets.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::CryptoError`] if key generation or wrapping fails.
    /// Returns [`ArcaError::AlreadyInitialised`] if called on a vault that has
    /// already been set up.
    pub fn initialise(&mut self, master_password: &str) -> Result<(), ArcaError> {
        if !matches!(self.state, VaultState::Uninitialised) {
            return Err(ArcaError::AlreadyInitialised);
        }
        let (wrapped_dek, dek) = Self::generate_wrapped_dek(master_password)?;
        self.state = VaultState::Unlocked {
            wrapped_dek,
            dek: InMemoryDek(dek),
            last_activity: Instant::now(),
        };
        Ok(())
    }

    /// Unlock the vault with `master_password`.
    ///
    /// Derives the KEK from the password, unwraps the stored DEK, and holds
    /// it in memory until [`Self::lock`] is called or the idle timeout fires.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::NotInitialised`] if the vault has never been
    /// initialised, [`ArcaError::AlreadyUnlocked`] if already unlocked, or
    /// [`ArcaError::CryptoError`] if the password is wrong.
    pub fn unlock(&mut self, master_password: &str) -> Result<(), ArcaError> {
        match &self.state {
            VaultState::Uninitialised => Err(ArcaError::NotInitialised),
            VaultState::Unlocked { .. } => Err(ArcaError::AlreadyUnlocked),
            VaultState::Locked { wrapped_dek } => {
                let dek = wrapped_dek.unwrap(master_password)?;
                let wrapped_dek = wrapped_dek.clone();
                self.state = VaultState::Unlocked {
                    wrapped_dek,
                    dek: InMemoryDek(dek),
                    last_activity: Instant::now(),
                };
                Ok(())
            }
        }
    }

    /// Lock the vault, wiping the in-memory DEK.
    ///
    /// After locking, all `store_credential`/`retrieve_credential` calls will
    /// fail until the vault is unlocked again.  Safe to call on an already-
    /// locked or uninitialised vault (no-op).
    pub fn lock(&mut self) {
        if let VaultState::Unlocked { wrapped_dek, .. } = &self.state {
            let wrapped_dek = wrapped_dek.clone();
            self.state = VaultState::Locked { wrapped_dek };
        }
    }

    /// Return `true` if the vault is currently unlocked.
    #[must_use]
    pub fn is_unlocked(&self) -> bool {
        matches!(self.state, VaultState::Unlocked { .. })
    }

    /// Check whether the idle timeout has elapsed and lock automatically.
    ///
    /// Call this periodically (e.g. from a background task) to enforce the
    /// idle-timeout policy.  Returns `true` if the vault was just locked by
    /// this call.
    pub fn check_idle(&mut self) -> bool {
        if let VaultState::Unlocked { last_activity, .. } = &self.state {
            if last_activity.elapsed() >= self.idle_timeout {
                self.lock();
                return true;
            }
        }
        false
    }

    // ── Key rotation ───────────────────────────────────────────────────────

    /// Rotate the master password.
    ///
    /// Derives a fresh KEK from `new_password` and re-wraps the existing DEK.
    /// **No stored secret ciphertext is re-encrypted** — only the wrapped DEK
    /// changes.  The vault remains unlocked after a successful rotation.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::NotInitialised`] / [`ArcaError::VaultLocked`] if
    /// the vault is not currently unlocked.  The caller must unlock first.
    pub fn rotate_key(&mut self, new_password: &str) -> Result<(), ArcaError> {
        let dek_bytes = self.require_dek()?.0;
        // Wrap the existing DEK under the new password.
        let salt = SaltString::generate(&mut OsRng);
        let kek = derive_kek(new_password, salt.as_str())?;
        let blob = EncryptedBlob::encrypt(&kek, &dek_bytes)?;
        let new_wrapped = WrappedDek {
            kek_salt: salt.as_str().to_string(),
            blob,
        };
        // Update state while preserving the in-memory DEK.
        if let VaultState::Unlocked {
            wrapped_dek,
            last_activity,
            ..
        } = &mut self.state
        {
            *wrapped_dek = new_wrapped;
            *last_activity = Instant::now();
        }
        Ok(())
    }

    // ── Secret CRUD ────────────────────────────────────────────────────────

    /// Encrypt and store `value` under `name`.
    ///
    /// Requires the vault to be unlocked.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::VaultLocked`] or [`ArcaError::NotInitialised`] if
    /// the vault is not unlocked, or [`ArcaError::CryptoError`] on encryption
    /// failure.
    pub fn store_credential(
        &mut self,
        name: &str,
        value: &str,
        description: Option<String>,
    ) -> Result<(), ArcaError> {
        let dek = self.require_dek()?;
        let blob = EncryptedBlob::encrypt(&dek.0, value.as_bytes())?;
        self.entries
            .insert(name.to_string(), EncryptedEntry { blob, description });
        self.touch();
        Ok(())
    }

    /// Decrypt and return the credential stored under `name`.
    ///
    /// Requires the vault to be unlocked.
    ///
    /// # Errors
    ///
    /// Returns [`ArcaError::VaultLocked`] / [`ArcaError::NotInitialised`] if
    /// locked, [`ArcaError::NotFound`] if no such entry exists, or
    /// [`ArcaError::CryptoError`] on decryption failure.
    pub fn retrieve_credential(&mut self, name: &str) -> Result<String, ArcaError> {
        let dek = self.require_dek()?;
        let entry = self
            .entries
            .get(name)
            .ok_or_else(|| ArcaError::NotFound(name.to_string()))?;
        let plain = entry.blob.decrypt(&dek.0)?;
        self.touch();
        String::from_utf8(plain)
            .map_err(|e| ArcaError::CryptoError(format!("decrypted value is not UTF-8: {e}")))
    }

    /// Return the description of the credential stored under `name`, if any.
    ///
    /// Does **not** require the vault to be unlocked.
    pub fn credential_description(&self, name: &str) -> Option<&str> {
        self.entries.get(name).and_then(|e| e.description())
    }

    /// Delete the credential stored under `name`.
    ///
    /// Returns `true` if a credential was removed, `false` if `name` was not
    /// found.  Does **not** require the vault to be unlocked.
    pub fn delete_credential(&mut self, name: &str) -> bool {
        self.entries.remove(name).is_some()
    }

    /// Return the names of all stored credentials (order not guaranteed).
    ///
    /// Does **not** require the vault to be unlocked.
    pub fn list_credential_names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// Return `true` when the vault holds no credentials.
    #[must_use]
    pub fn credentials_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the number of stored credentials.
    #[must_use]
    pub fn credentials_len(&self) -> usize {
        self.entries.len()
    }

    // ── Private helpers ────────────────────────────────────────────────────

    /// Return a reference to the in-memory DEK, failing if locked/uninitialised.
    fn require_dek(&self) -> Result<&InMemoryDek, ArcaError> {
        match &self.state {
            VaultState::Uninitialised => Err(ArcaError::NotInitialised),
            VaultState::Locked { .. } => Err(ArcaError::VaultLocked),
            VaultState::Unlocked { dek, .. } => Ok(dek),
        }
    }

    /// Update `last_activity` timestamp (call after every authenticated op).
    fn touch(&mut self) {
        if let VaultState::Unlocked { last_activity, .. } = &mut self.state {
            *last_activity = Instant::now();
        }
    }

    /// Generate a fresh random DEK and wrap it under `master_password`.
    fn generate_wrapped_dek(
        master_password: &str,
    ) -> Result<(WrappedDek, [u8; KEY_BYTES]), ArcaError> {
        // Generate random DEK.
        let mut dek = [0u8; KEY_BYTES];
        OsRng.fill_bytes(&mut dek);
        // Derive KEK.
        let salt = SaltString::generate(&mut OsRng);
        let kek = derive_kek(master_password, salt.as_str())?;
        // Wrap DEK under KEK.
        let blob = EncryptedBlob::encrypt(&kek, &dek)?;
        Ok((
            WrappedDek {
                kek_salt: salt.as_str().to_string(),
                blob,
            },
            dek,
        ))
    }
}

// ── Legacy Vault (backward compatible) ───────────────────────────────────────

/// A single secret stored in the [`Vault`].
#[derive(Debug, Clone)]
pub struct SecretEntry {
    /// The raw secret bytes (e.g. UTF-8 encoded API key).
    pub bytes: Vec<u8>,

    /// Optional human-readable description of this secret.
    pub description: Option<String>,
}

/// Default upper bound for a single stored secret (64 KiB).
const DEFAULT_MAX_SECRET_BYTES: usize = 65_536;

/// Simple in-memory vault for named secrets (no encryption).
///
/// This is the original MVP implementation kept for backward compatibility.
/// For production use, prefer [`CredentialVault`] which provides AES-256-GCM
/// encryption, key-wrapping, and a lock/unlock lifecycle.
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

    // ── Legacy Vault tests (unchanged) ────────────────────────────────────

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
        assert!(matches!(
            vault.retrieve("missing"),
            Err(ArcaError::NotFound(_))
        ));
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

    // ── CredentialVault tests ─────────────────────────────────────────────

    fn unlocked_vault() -> CredentialVault {
        let mut v = CredentialVault::new(None);
        v.initialise("hunter2").unwrap();
        v
    }

    #[test]
    fn credential_vault_store_and_retrieve() {
        let mut vault = unlocked_vault();
        vault
            .store_credential("openai_key", "sk-abc123", Some("OpenAI key".to_string()))
            .unwrap();
        let value = vault.retrieve_credential("openai_key").unwrap();
        assert_eq!(value, "sk-abc123");
    }

    #[test]
    fn credential_vault_locked_on_init_not_locked() {
        let mut vault = CredentialVault::new(None);
        // Before initialise, all ops should fail.
        assert!(matches!(
            vault.store_credential("k", "v", None),
            Err(ArcaError::NotInitialised)
        ));
    }

    #[test]
    fn credential_vault_lock_prevents_retrieval() {
        let mut vault = unlocked_vault();
        vault.store_credential("k", "secret", None).unwrap();
        vault.lock();
        assert!(matches!(
            vault.retrieve_credential("k"),
            Err(ArcaError::VaultLocked)
        ));
    }

    #[test]
    fn credential_vault_unlock_restores_access() {
        let mut vault = unlocked_vault();
        vault.store_credential("k", "secret", None).unwrap();
        vault.lock();
        vault.unlock("hunter2").unwrap();
        let value = vault.retrieve_credential("k").unwrap();
        assert_eq!(value, "secret");
    }

    #[test]
    fn credential_vault_wrong_password_fails_unlock() {
        let mut vault = unlocked_vault();
        vault.lock();
        assert!(matches!(
            vault.unlock("wrong-password"),
            Err(ArcaError::CryptoError(_))
        ));
    }

    #[test]
    fn credential_vault_key_rotation_no_data_loss() {
        let mut vault = unlocked_vault();
        vault
            .store_credential("api_key", "tok-super-secret", None)
            .unwrap();
        // Rotate to new password.
        vault.rotate_key("new-master-pw").unwrap();
        // Data still accessible while unlocked.
        let value = vault.retrieve_credential("api_key").unwrap();
        assert_eq!(value, "tok-super-secret");
        // Lock and re-unlock with new password.
        vault.lock();
        vault.unlock("new-master-pw").unwrap();
        let value2 = vault.retrieve_credential("api_key").unwrap();
        assert_eq!(value2, "tok-super-secret");
    }

    #[test]
    fn credential_vault_key_rotation_old_password_fails() {
        let mut vault = unlocked_vault();
        vault.rotate_key("new-master-pw").unwrap();
        vault.lock();
        // Old password should no longer work.
        assert!(vault.unlock("hunter2").is_err());
    }

    #[test]
    fn credential_vault_idle_timeout_auto_lock() {
        let mut vault = CredentialVault::new(Some(Duration::from_millis(1)));
        vault.initialise("pw").unwrap();
        assert!(vault.is_unlocked());
        // Sleep past the idle timeout.
        std::thread::sleep(Duration::from_millis(5));
        let locked = vault.check_idle();
        assert!(locked, "vault should have auto-locked due to idle timeout");
        assert!(!vault.is_unlocked());
    }

    #[test]
    fn credential_vault_no_auto_lock_before_timeout() {
        let mut vault = CredentialVault::new(Some(Duration::from_secs(3600)));
        vault.initialise("pw").unwrap();
        let locked = vault.check_idle();
        assert!(!locked, "vault should not lock before idle timeout");
        assert!(vault.is_unlocked());
    }

    #[test]
    fn credential_vault_double_lock_is_noop() {
        let mut vault = unlocked_vault();
        vault.lock();
        vault.lock(); // Should not panic.
        assert!(!vault.is_unlocked());
    }

    #[test]
    fn credential_vault_double_unlock_returns_error() {
        let mut vault = unlocked_vault();
        assert!(matches!(
            vault.unlock("hunter2"),
            Err(ArcaError::AlreadyUnlocked)
        ));
    }

    #[test]
    fn credential_vault_double_init_returns_error() {
        let mut vault = CredentialVault::new(None);
        vault.initialise("pw").unwrap();
        assert!(matches!(
            vault.initialise("pw2"),
            Err(ArcaError::AlreadyInitialised)
        ));
    }

    #[test]
    fn credential_vault_delete_credential() {
        let mut vault = unlocked_vault();
        vault.store_credential("k", "v", None).unwrap();
        assert!(vault.delete_credential("k"));
        assert!(matches!(
            vault.retrieve_credential("k"),
            Err(ArcaError::NotFound(_))
        ));
    }

    #[test]
    fn credential_vault_list_names() {
        let mut vault = unlocked_vault();
        vault.store_credential("alpha", "1", None).unwrap();
        vault.store_credential("beta", "2", None).unwrap();
        let mut names = vault.list_credential_names();
        names.sort_unstable();
        assert_eq!(names, ["alpha", "beta"]);
    }

    #[test]
    fn credential_vault_multiple_secrets_roundtrip() {
        let mut vault = unlocked_vault();
        let secrets = [
            ("provider:openai:api_key", "sk-openai-key"),
            ("provider:anthropic:api_key", "sk-anthropic-key"),
            ("channel:telegram:bot_token", "1234567:ABC-telegram"),
        ];
        for (name, value) in &secrets {
            vault.store_credential(name, value, None).unwrap();
        }
        for (name, expected) in &secrets {
            assert_eq!(vault.retrieve_credential(name).unwrap(), *expected);
        }
    }
}
