//! Model registry — catalogue of known BitNet models and local auto-detection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::{config::InferenceConfig, error::InferenceError};

// ── KnownModel ────────────────────────────────────────────────────────────────

/// A statically-known BitNet model that can be downloaded from HuggingFace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownModel {
    /// Stable short identifier used as a key in config and CLI commands.
    pub id: &'static str,

    /// HuggingFace repository slug (e.g. `"microsoft/BitNet-b1.58-2B-4T"`).
    pub hf_repo: &'static str,

    /// GGUF filename within the repository.
    pub filename: &'static str,

    /// Total parameter count.
    pub param_count: u64,

    /// Expected on-disk size in bytes after download.
    pub size_bytes: u64,

    /// Human-readable description.
    pub description: &'static str,
}

impl KnownModel {
    /// Return the HuggingFace direct-download URL for this model.
    #[must_use]
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{}/resolve/main/{}",
            self.hf_repo, self.filename
        )
    }
}

// ── Catalogue ─────────────────────────────────────────────────────────────────

/// All models supported out of the box.
pub static KNOWN_MODELS: &[KnownModel] = &[
    KnownModel {
        id: "bitnet-2b",
        hf_repo: "microsoft/BitNet-b1.58-2B-4T",
        filename: "ggml-model-i2_s.gguf",
        param_count: 2_400_000_000,
        size_bytes: 536_870_912, // ~0.5 GB
        description: "BitNet b1.58 2B 4T — 2.4B params, ~0.5 GB, best for CPU-only setups",
    },
    KnownModel {
        id: "llama3-8b-bitnet",
        hf_repo: "HF1BitLLM/Llama3-8B-1.58-100B-tokens",
        filename: "ggml-model-i2_s.gguf",
        param_count: 8_000_000_000,
        size_bytes: 2_147_483_648, // ~2 GB
        description: "Llama 3 8B 1.58-bit — 8B params, ~2 GB, higher quality",
    },
    KnownModel {
        id: "falcon3-1b-bitnet",
        hf_repo: "tiiuae/Falcon3-1B-Instruct-1.58bit",
        filename: "ggml-model-i2_s.gguf",
        param_count: 1_000_000_000,
        size_bytes: 268_435_456, // ~0.25 GB
        description: "Falcon 3 1B 1.58-bit — 1B params, ~0.25 GB, lowest footprint",
    },
    KnownModel {
        id: "falcon3-3b-bitnet",
        hf_repo: "tiiuae/Falcon3-3B-Instruct-1.58bit",
        filename: "ggml-model-i2_s.gguf",
        param_count: 3_000_000_000,
        size_bytes: 805_306_368, // ~0.75 GB
        description: "Falcon 3 3B 1.58-bit — 3B params, ~0.75 GB",
    },
    KnownModel {
        id: "falcon3-7b-bitnet",
        hf_repo: "tiiuae/Falcon3-7B-Instruct-1.58bit",
        filename: "ggml-model-i2_s.gguf",
        param_count: 7_000_000_000,
        size_bytes: 1_879_048_192, // ~1.75 GB
        description: "Falcon 3 7B 1.58-bit — 7B params, ~1.75 GB",
    },
    KnownModel {
        id: "falcon3-10b-bitnet",
        hf_repo: "tiiuae/Falcon3-10B-Instruct-1.58bit",
        filename: "ggml-model-i2_s.gguf",
        param_count: 10_000_000_000,
        size_bytes: 2_684_354_560, // ~2.5 GB
        description: "Falcon 3 10B 1.58-bit — 10B params, ~2.5 GB",
    },
];

// ── LocalModel ────────────────────────────────────────────────────────────────

/// A model that has been found (or downloaded) on the local filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    /// Model identifier (matches [`KnownModel::id`] for known models).
    pub id: String,

    /// Absolute path to the GGUF file.
    pub path: PathBuf,

    /// On-disk file size in bytes.
    pub size_bytes: u64,

    /// `true` if this model is listed in [`KNOWN_MODELS`].
    pub is_known: bool,
}

// ── ModelRegistry ─────────────────────────────────────────────────────────────

/// Manages the catalogue of known models and the locally available models.
pub struct ModelRegistry {
    config: InferenceConfig,
}

impl ModelRegistry {
    /// Create a new registry using the given configuration.
    #[must_use]
    pub fn new(config: InferenceConfig) -> Self {
        Self { config }
    }

    /// Return all statically-known models regardless of local availability.
    #[must_use]
    pub fn known_models(&self) -> &'static [KnownModel] {
        KNOWN_MODELS
    }

    /// Look up a known model by its [`id`](KnownModel::id).
    #[must_use]
    pub fn find_known(&self, id: &str) -> Option<&'static KnownModel> {
        KNOWN_MODELS.iter().find(|m| m.id == id)
    }

    /// Scan the local models directory and return all `.gguf` files found.
    ///
    /// Files whose stem matches a [`KnownModel::id`] are marked as known.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the directory cannot be read.
    pub fn scan_local(&self) -> Result<Vec<LocalModel>, InferenceError> {
        let dir = self.config.resolved_models_dir();

        if !dir.exists() {
            debug!(?dir, "models directory does not exist; returning empty list");
            return Ok(Vec::new());
        }

        let mut models = Vec::new();

        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }

            let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into());

            let id = stem.clone();
            let is_known = KNOWN_MODELS.iter().any(|m| m.id == id);

            models.push(LocalModel {
                id,
                path,
                size_bytes,
                is_known,
            });
        }

        models.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(models)
    }

    /// Return the expected local path for a known model.
    #[must_use]
    pub fn model_path(&self, model: &KnownModel) -> PathBuf {
        self.config
            .resolved_models_dir()
            .join(format!("{}.gguf", model.id))
    }

    /// Return `true` if the model with the given id is already downloaded.
    #[must_use]
    pub fn is_downloaded(&self, id: &str) -> bool {
        if let Some(known) = self.find_known(id) {
            self.model_path(known).exists()
        } else {
            false
        }
    }

    /// Remove a downloaded model from disk.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::UnknownModel`] if `id` is not in [`KNOWN_MODELS`].
    /// - [`InferenceError::ModelNotFound`] if the file is not present.
    /// - [`InferenceError::Io`] if deletion fails.
    pub fn remove(&self, id: &str) -> Result<(), InferenceError> {
        let known = self
            .find_known(id)
            .ok_or_else(|| InferenceError::UnknownModel(id.to_owned()))?;

        let path = self.model_path(known);

        if !path.exists() {
            return Err(InferenceError::ModelNotFound(path.display().to_string()));
        }

        std::fs::remove_file(&path)?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_are_non_empty() {
        assert!(!KNOWN_MODELS.is_empty());
    }

    #[test]
    fn known_model_download_urls_are_https() {
        for m in KNOWN_MODELS {
            let url = m.download_url();
            assert!(
                url.starts_with("https://huggingface.co/"),
                "unexpected URL format for {}: {url}",
                m.id
            );
        }
    }

    #[test]
    fn find_known_returns_correct_model() {
        let reg = ModelRegistry::new(InferenceConfig::default());
        let m = reg.find_known("bitnet-2b").expect("bitnet-2b should exist");
        assert_eq!(m.id, "bitnet-2b");
    }

    #[test]
    fn find_known_returns_none_for_unknown_id() {
        let reg = ModelRegistry::new(InferenceConfig::default());
        assert!(reg.find_known("does-not-exist").is_none());
    }

    #[test]
    fn scan_local_returns_empty_for_missing_dir() {
        let cfg = InferenceConfig {
            models_dir: PathBuf::from("/nonexistent/path/models"),
            ..InferenceConfig::default()
        };
        let reg = ModelRegistry::new(cfg);
        let result = reg.scan_local().expect("missing dir should yield empty list");
        assert!(result.is_empty());
    }

    #[test]
    fn scan_local_finds_gguf_files() {
        let tmp = std::env::temp_dir().join("pares_agens_registry_test");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("bitnet-2b.gguf"), b"dummy").unwrap();
        std::fs::write(tmp.join("unrelated.txt"), b"nope").unwrap();

        let cfg = InferenceConfig {
            models_dir: tmp.clone(),
            ..InferenceConfig::default()
        };
        let reg = ModelRegistry::new(cfg);
        let local = reg.scan_local().unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].id, "bitnet-2b");
        assert!(local[0].is_known);

        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn remove_unknown_model_errors() {
        let reg = ModelRegistry::new(InferenceConfig::default());
        assert!(matches!(
            reg.remove("ghost-model"),
            Err(InferenceError::UnknownModel(_))
        ));
    }
}
