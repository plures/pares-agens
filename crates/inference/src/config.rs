//! Configuration types for the local inference backend.
//!
//! The `[models.local]` section of the agent configuration deserialises into
//! [`InferenceConfig`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the local BitNet inference backend.
///
/// # TOML example
///
/// ```toml
/// [models.local]
/// models_dir     = "~/.pares-agens/models"
/// thread_count   = 8
/// context_size   = 2048
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceConfig {
    /// Directory where downloaded models are cached.
    ///
    /// Tilde expansion is applied at runtime: `~` is replaced with the user's
    /// home directory.  Defaults to `~/.pares-agens/models`.
    #[serde(default = "InferenceConfig::default_models_dir")]
    pub models_dir: PathBuf,

    /// Number of threads used by the inference engine.
    ///
    /// Defaults to the number of logical CPU cores reported by the OS.
    #[serde(default = "InferenceConfig::default_thread_count")]
    pub thread_count: u32,

    /// Maximum context size (in tokens) for the inference session.
    ///
    /// Larger values allow longer conversations but use more memory.
    /// Defaults to `2048`.
    #[serde(default = "InferenceConfig::default_context_size")]
    pub context_size: u32,
}

impl InferenceConfig {
    fn default_models_dir() -> PathBuf {
        dirs_home().join(".pares-agens").join("models")
    }

    fn default_thread_count() -> u32 {
        std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4)
    }

    fn default_context_size() -> u32 {
        2048
    }

    /// Return the fully resolved models directory path (tilde expanded).
    #[must_use]
    pub fn resolved_models_dir(&self) -> PathBuf {
        let raw = self.models_dir.to_string_lossy();
        if raw.starts_with('~') {
            let suffix = raw.trim_start_matches('~').trim_start_matches('/');
            dirs_home().join(suffix)
        } else {
            self.models_dir.clone()
        }
    }
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            models_dir: Self::default_models_dir(),
            thread_count: Self::default_thread_count(),
            context_size: Self::default_context_size(),
        }
    }
}

/// Return the current user's home directory, falling back to `/tmp` if
/// the home directory cannot be determined.
fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let cfg = InferenceConfig::default();
        assert!(cfg.thread_count >= 1);
        assert_eq!(cfg.context_size, 2048);
    }

    #[test]
    fn resolved_models_dir_expands_tilde() {
        let cfg = InferenceConfig {
            models_dir: PathBuf::from("~/.pares-agens/models"),
            ..InferenceConfig::default()
        };
        let resolved = cfg.resolved_models_dir();
        assert!(!resolved.starts_with("~"), "tilde should have been expanded");
    }

    #[test]
    fn resolved_models_dir_passthrough_absolute() {
        let cfg = InferenceConfig {
            models_dir: PathBuf::from("/var/lib/pares-agens/models"),
            ..InferenceConfig::default()
        };
        assert_eq!(
            cfg.resolved_models_dir(),
            PathBuf::from("/var/lib/pares-agens/models")
        );
    }

    #[test]
    fn serde_round_trip() {
        let original = InferenceConfig {
            models_dir: PathBuf::from("/tmp/models"),
            thread_count: 4,
            context_size: 4096,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: InferenceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.thread_count, 4);
        assert_eq!(restored.context_size, 4096);
    }
}
