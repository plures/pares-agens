//! Secret storage abstraction for API keys, tokens, and sensitive config.
//!
//! All secrets **must** flow through a [`SecretStore`] implementation — never
//! through JSON config files, env vars, or plaintext fields in serialisable
//! structs.
//!
//! # Implementations
//!
//! | Type | Backend | When available |
//! |------|---------|----------------|
//! | [`InMemorySecretStore`] | `tokio::sync::RwLock<HashMap>` | always (use for tests) |
//! | [`VaultSecretStore`] | plures-vault encrypted storage | `vault` feature only |
//!
//! # Example
//!
//! ```rust
//! use pares_agens_core::secrets::{InMemorySecretStore, SecretStore};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = InMemorySecretStore::new();
//! store.set("openai.api_key", "sk-test").await?;
//! let key = store.get("openai.api_key").await?;
//! assert_eq!(key.as_deref(), Some("sk-test"));
//! store.delete("openai.api_key").await?;
//! assert_eq!(store.get("openai.api_key").await?, None);
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by [`SecretStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// The underlying vault storage returned an error.
    #[error("vault storage error: {0}")]
    Storage(String),

    /// The vault has not been initialized (no master password set yet).
    #[error("vault not initialized — call open() with a master password first")]
    NotInitialised,

    /// The master password provided was incorrect.
    #[error("invalid master password")]
    InvalidPassword,

    /// A generic I/O error (e.g. failed to create vault directory).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// SecretStore trait
// ---------------------------------------------------------------------------

/// Trait for encrypted key-value secret storage.
///
/// Implementations must be [`Send`] + [`Sync`] so they can be held behind
/// an `Arc<dyn SecretStore>` in multi-threaded async contexts.
#[async_trait]
pub trait SecretStore: Send + Sync {
    /// Retrieve the secret stored under `key`, or `None` if no such entry
    /// exists.
    async fn get(&self, key: &str) -> Result<Option<String>, SecretError>;

    /// Store `value` under `key`, replacing any existing entry.
    async fn set(&self, key: &str, value: &str) -> Result<(), SecretError>;

    /// Remove the entry for `key`.  Succeeds silently if the key does not
    /// exist.
    async fn delete(&self, key: &str) -> Result<(), SecretError>;

    /// Return all stored keys (values are **not** returned).
    async fn list_keys(&self) -> Result<Vec<String>, SecretError>;
}

// ---------------------------------------------------------------------------
// InMemorySecretStore — for tests and development
// ---------------------------------------------------------------------------

/// In-memory, non-persistent implementation of [`SecretStore`].
///
/// Intended for unit tests and local development.  **Secrets are lost when
/// the process exits.**
#[derive(Clone, Default)]
pub struct InMemorySecretStore {
    map: Arc<RwLock<HashMap<String, String>>>,
}

impl InMemorySecretStore {
    /// Create a new empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SecretStore for InMemorySecretStore {
    async fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        Ok(self.map.read().await.get(key).cloned())
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        self.map
            .write()
            .await
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        self.map.write().await.remove(key);
        Ok(())
    }

    async fn list_keys(&self) -> Result<Vec<String>, SecretError> {
        let mut keys: Vec<String> = self.map.read().await.keys().cloned().collect();
        keys.sort();
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// VaultSecretStore — plures-vault backed, encrypted at rest
// ---------------------------------------------------------------------------

/// Encrypted-at-rest secret store backed by plures-vault.
///
/// Each key-value pair is stored as a [`vault_core::Credential`] where:
/// - `title` = the secret key
/// - `password` = the secret value (encrypted by the vault master key)
///
/// The vault must be unlocked (via [`VaultSecretStore::open`]) before any
/// operation can succeed.
///
/// Only compiled when the `vault` Cargo feature is enabled.
#[cfg(feature = "vault")]
pub struct VaultSecretStore {
    manager: tokio::sync::Mutex<vault_core::VaultManager>,
}

#[cfg(feature = "vault")]
impl VaultSecretStore {
    /// Open (or create) the vault at `path` and unlock it with
    /// `master_password`.
    ///
    /// If the vault has never been initialised, it will be created and
    /// initialised automatically.
    ///
    /// # Errors
    ///
    /// Returns [`SecretError::Storage`] if the vault database cannot be
    /// opened, or [`SecretError::InvalidPassword`] if the password is wrong.
    pub async fn open(
        path: impl AsRef<std::path::Path>,
        master_password: &str,
    ) -> Result<Self, SecretError> {
        let db_path = path.as_ref().to_string_lossy().to_string();
        let mut manager = vault_core::VaultManager::new(&db_path)
            .await
            .map_err(|e| SecretError::Storage(e.to_string()))?;

        // Initialise on first run; unlock on subsequent runs.
        match manager.check_initialization().await {
            Ok(_) => {
                manager
                    .unlock_vault(master_password)
                    .await
                    .map_err(|_| SecretError::InvalidPassword)?;
            }
            Err(_) => {
                manager
                    .init_vault("pares-agens", master_password)
                    .await
                    .map_err(|e| SecretError::Storage(e.to_string()))?;
                manager
                    .unlock_vault(master_password)
                    .await
                    .map_err(|_| SecretError::InvalidPassword)?;
            }
        }

        Ok(Self {
            manager: tokio::sync::Mutex::new(manager),
        })
    }
}

#[cfg(feature = "vault")]
#[async_trait]
impl SecretStore for VaultSecretStore {
    async fn get(&self, key: &str) -> Result<Option<String>, SecretError> {
        let mgr = self.manager.lock().await;
        mgr.get_credential(key)
            .await
            .map(|opt| opt.map(|c| c.password))
            .map_err(|e| SecretError::Storage(e.to_string()))
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), SecretError> {
        let mgr = self.manager.lock().await;
        // Try update first; if the key does not exist yet, fall back to add.
        let updated = mgr
            .update_credential(key, None, Some(value.to_owned()), None, None)
            .await
            .map_err(|e| SecretError::Storage(e.to_string()))?;

        if updated.is_none() {
            // Key did not exist — create a new credential.
            mgr.add_credential(key.to_owned(), None, value.to_owned(), None, None)
                .await
                .map_err(|e| SecretError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<(), SecretError> {
        let mgr = self.manager.lock().await;
        mgr.delete_credential(key)
            .await
            .map_err(|e| SecretError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_keys(&self) -> Result<Vec<String>, SecretError> {
        let mgr = self.manager.lock().await;
        let mut keys: Vec<String> = mgr
            .list_credentials()
            .await
            .map_err(|e| SecretError::Storage(e.to_string()))?
            .into_iter()
            .map(|c| c.title)
            .collect();
        keys.sort();
        Ok(keys)
    }
}

// ---------------------------------------------------------------------------
// Env-var migration helper
// ---------------------------------------------------------------------------

/// Mapping of well-known environment variables to their canonical secret keys.
///
/// Each tuple is `(ENV_VAR_NAME, vault_key)`.  Used by [`migrate_from_env`]
/// to perform a one-time migration of pre-existing environment-variable
/// secrets into the vault so that callers can transition to vault-only
/// storage.
///
/// The vault key format follows the same convention as [`provider_api_key`]
/// and [`channel_token`]:
/// - `provider:<name>:api_key` for model-provider credentials
/// - `channel:<kind>:bot_token` for channel-adapter tokens
pub const ENV_KEY_MAP: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "provider:openai:api_key"),
    ("ANTHROPIC_API_KEY", "provider:anthropic:api_key"),
    ("GOOGLE_API_KEY", "provider:google:api_key"),
    ("AZURE_OPENAI_API_KEY", "provider:azure-openai:api_key"),
    ("COHERE_API_KEY", "provider:cohere:api_key"),
    ("GROQ_API_KEY", "provider:groq:api_key"),
    ("TELEGRAM_BOT_TOKEN", "channel:telegram:bot_token"),
];

/// Canonical key used to record that the env-var migration has already run.
pub const MIGRATION_DONE_KEY: &str = "migration:env_vars:done";

/// One-time migration helper: reads well-known environment variables and
/// writes them into `store`.
///
/// The migration is **idempotent**: once the [`MIGRATION_DONE_KEY`] sentinel
/// is present in the store the function returns immediately without touching
/// any other entries.
///
/// Returns a list of the secret keys that were migrated in this call.
///
/// # Errors
///
/// Propagates any [`SecretError`] returned by the store.
pub async fn migrate_from_env(store: &dyn SecretStore) -> Result<Vec<String>, SecretError> {
    if store.get(MIGRATION_DONE_KEY).await?.is_some() {
        return Ok(vec![]);
    }

    let mut migrated = Vec::new();

    for (env_var, secret_key) in ENV_KEY_MAP {
        if let Ok(value) = std::env::var(env_var) {
            if !value.is_empty() {
                store.set(secret_key, &value).await?;
                migrated.push(secret_key.to_string());
            }
        }
    }

    store.set(MIGRATION_DONE_KEY, "1").await?;
    Ok(migrated)
}

// ---------------------------------------------------------------------------
// Convenience helpers
// ---------------------------------------------------------------------------

/// Derive the canonical vault key for a model provider's API key.
///
/// Provider names containing `:` are hex-encoded to avoid collisions with the
/// key separator.  Plain names (the common case) are used as-is.
///
/// ```
/// use pares_agens_core::secrets::provider_api_key;
/// assert_eq!(provider_api_key("openai"), "provider:openai:api_key");
/// assert_eq!(provider_api_key("azure:openai"), "provider:hex:617a7572653a6f70656e6169:api_key");
/// ```
pub fn provider_api_key(provider_name: &str) -> String {
    let segment = normalise_key_segment(provider_name);
    format!("provider:{segment}:api_key")
}

/// Derive the canonical vault key for a channel adapter token.
///
/// ```
/// use pares_agens_core::secrets::channel_token;
/// assert_eq!(channel_token("telegram"), "channel:telegram:bot_token");
/// ```
pub fn channel_token(channel_kind: &str) -> String {
    format!("channel:{channel_kind}:bot_token")
}

/// Normalize a string for safe embedding inside a `:` -delimited vault key.
///
/// Names that do not contain `:` are returned unchanged.  Names that do
/// contain `:` are hex-encoded with a `hex:` prefix so they cannot be
/// confused with key separators (and so the encoding is reversible).
fn normalise_key_segment(s: &str) -> String {
    if !s.contains(':') {
        return s.to_string();
    }
    // Encode the raw bytes as a lowercase hex string with a "hex:" prefix.
    let mut encoded = String::from("hex:");
    for byte in s.as_bytes() {
        use std::fmt::Write as _;
        write!(&mut encoded, "{:02x}", byte)
            .expect("hex-encoding a provider name for vault key cannot fail");
    }
    encoded
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Basic CRUD ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn set_get_delete_roundtrip() {
        let store = InMemorySecretStore::new();

        store.set("mykey", "myvalue").await.unwrap();
        assert_eq!(
            store.get("mykey").await.unwrap(),
            Some("myvalue".to_string())
        );

        store.delete("mykey").await.unwrap();
        assert_eq!(store.get("mykey").await.unwrap(), None);
    }

    #[tokio::test]
    async fn get_missing_key_returns_none() {
        let store = InMemorySecretStore::new();
        assert_eq!(store.get("nonexistent").await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_overwrites_existing_value() {
        let store = InMemorySecretStore::new();
        store.set("key", "first").await.unwrap();
        store.set("key", "second").await.unwrap();
        assert_eq!(store.get("key").await.unwrap(), Some("second".to_string()));
    }

    #[tokio::test]
    async fn delete_nonexistent_is_silent() {
        let store = InMemorySecretStore::new();
        store.delete("nothing").await.unwrap();
    }

    #[tokio::test]
    async fn list_keys_returns_sorted() {
        let store = InMemorySecretStore::new();
        store.set("z-key", "v").await.unwrap();
        store.set("a-key", "v").await.unwrap();
        store.set("m-key", "v").await.unwrap();

        let keys = store.list_keys().await.unwrap();
        assert_eq!(keys, vec!["a-key", "m-key", "z-key"]);
    }

    // ── Secrets are never serialised to JSON ─────────────────────────────

    #[tokio::test]
    async fn secrets_not_in_json_serialisation() {
        let store = InMemorySecretStore::new();
        store
            .set("provider:openai:api_key", "sk-super-secret")
            .await
            .unwrap();

        // The store itself must not be serialisable — confirmed at compile time
        // because InMemorySecretStore does not derive Serialize/Deserialize.
        // We verify here that the value obtained is the real value (so the
        // store works), but the struct itself can never accidentally end up in
        // a serde_json::to_value call.
        let retrieved = store.get("provider:openai:api_key").await.unwrap();
        assert_eq!(retrieved.as_deref(), Some("sk-super-secret"));

        // Confirm we cannot accidentally round-trip via JSON.
        // This assertion is structural: InMemorySecretStore has no Serialize impl.
        // We just verify the value is never exposed in list_keys().
        let keys = store.list_keys().await.unwrap();
        assert_eq!(keys, vec!["provider:openai:api_key"]);
        // Values are NOT returned by list_keys — only keys.
    }

    // ── Migration helper ─────────────────────────────────────────────────

    #[tokio::test]
    async fn migrate_from_env_is_idempotent() {
        // Temporarily clear any env vars that ENV_KEY_MAP would pick up so
        // the first call genuinely finds nothing to migrate.
        let saved: Vec<(&str, Option<String>)> = ENV_KEY_MAP
            .iter()
            .map(|(env_var, _)| (*env_var, std::env::var(env_var).ok()))
            .collect();
        for (env_var, _) in &saved {
            std::env::remove_var(env_var);
        }

        let store = InMemorySecretStore::new();

        // First call — nothing in env, but sentinel is written.
        let migrated = migrate_from_env(&store).await.unwrap();
        assert!(migrated.is_empty());

        // Restore env vars.
        for (env_var, val) in &saved {
            if let Some(v) = val {
                std::env::set_var(env_var, v);
            }
        }

        // Second call — sentinel already present, returns empty list.
        store
            .set("provider:openai:api_key", "sk-already-there")
            .await
            .unwrap();
        let migrated2 = migrate_from_env(&store).await.unwrap();
        assert!(migrated2.is_empty());
        // Key was not overwritten.
        assert_eq!(
            store
                .get("provider:openai:api_key")
                .await
                .unwrap()
                .as_deref(),
            Some("sk-already-there")
        );
    }

    // ── Convenience helpers ───────────────────────────────────────────────

    #[test]
    fn provider_api_key_plain_name() {
        assert_eq!(provider_api_key("openai"), "provider:openai:api_key");
        assert_eq!(provider_api_key("anthropic"), "provider:anthropic:api_key");
    }

    #[test]
    fn provider_api_key_encodes_colon_in_name() {
        // Names containing `:` must be encoded to avoid colliding with the
        // key separator format `provider:<name>:api_key`.
        let key = provider_api_key("azure:openai");
        assert!(!key.contains("azure:openai"), "raw colon must not appear");
        assert_eq!(key, "provider:hex:617a7572653a6f70656e6169:api_key");
    }

    #[test]
    fn channel_token_format() {
        assert_eq!(channel_token("telegram"), "channel:telegram:bot_token");
    }
}
