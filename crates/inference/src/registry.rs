//! Local model registry — maps model IDs to on-disk file paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::InferenceError;

/// Metadata for a registered model.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    /// Stable identifier used to look up the model (e.g. `"bitnet-b1.58-3b"`).
    pub model_id: String,

    /// Absolute path to the model file on disk.
    pub model_path: PathBuf,

    /// Human-readable description of the model.
    pub description: String,

    /// Size of the model file in bytes, if known.
    pub file_size_bytes: Option<u64>,

    /// Short capability tags (e.g. `["chat", "instruct"]`).
    pub capabilities: Vec<String>,
}

/// Return the platform default model cache directory: `~/.pares-agens/models`.
///
/// Falls back to the current directory if the home directory cannot be
/// determined.
pub fn default_cache_dir() -> PathBuf {
    let base = dirs_sys_home().unwrap_or_else(|| PathBuf::from("."));
    base.join(".pares-agens").join("models")
}

/// Minimal cross-platform home-directory lookup used only by
/// [`default_cache_dir`].  Checks `$HOME` on Unix and `$USERPROFILE` on
/// Windows before falling back to `None`.
fn dirs_sys_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Registry of locally available models.
///
/// The registry maps model identifiers to their on-disk paths, letting
/// `Cerebellum` and other callers resolve a model ID to a concrete file path
/// before constructing a [`crate::runner::BitNetLocalRunner`].
///
/// # Example
///
/// ```rust
/// use pares_agens_inference::ModelRegistry;
/// use std::path::PathBuf;
///
/// let mut registry = ModelRegistry::new();
/// registry.register("bitnet-b1.58-3b", PathBuf::from("models/bitnet-3b.bin"), "BitNet 1.58-bit 3B");
///
/// let entry = registry.get("bitnet-b1.58-3b").unwrap();
/// assert_eq!(entry.model_id, "bitnet-b1.58-3b");
/// ```
#[derive(Debug, Default)]
pub struct ModelRegistry {
    entries: HashMap<String, ModelEntry>,
}

impl ModelRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a model.
    ///
    /// If a model with the same `model_id` already exists it is replaced.
    /// `file_size_bytes` and `capabilities` default to `None`/empty and can
    /// be updated directly on the returned entry.
    pub fn register(
        &mut self,
        model_id: impl Into<String>,
        model_path: PathBuf,
        description: impl Into<String>,
    ) {
        let model_id = model_id.into();
        let file_size_bytes = std::fs::metadata(&model_path).ok().map(|m| m.len());
        self.entries.insert(
            model_id.clone(),
            ModelEntry {
                model_id,
                model_path,
                description: description.into(),
                file_size_bytes,
                capabilities: Vec::new(),
            },
        );
    }

    /// Register a model with full metadata.
    pub fn register_full(
        &mut self,
        model_id: impl Into<String>,
        model_path: PathBuf,
        description: impl Into<String>,
        file_size_bytes: Option<u64>,
        capabilities: Vec<String>,
    ) {
        let model_id = model_id.into();
        self.entries.insert(
            model_id.clone(),
            ModelEntry {
                model_id,
                model_path,
                description: description.into(),
                file_size_bytes,
                capabilities,
            },
        );
    }

    /// Look up a model by ID.
    ///
    /// Returns `None` if no model with that ID has been registered.
    pub fn get(&self, model_id: &str) -> Option<&ModelEntry> {
        self.entries.get(model_id)
    }

    /// Remove a model from the registry.
    ///
    /// Returns the removed entry, or `None` if it was not present.
    pub fn remove(&mut self, model_id: &str) -> Option<ModelEntry> {
        self.entries.remove(model_id)
    }

    /// Iterate over all registered model entries.
    pub fn entries(&self) -> impl Iterator<Item = &ModelEntry> {
        self.entries.values()
    }

    /// Populate the registry by scanning `model_dir` for `.bin` and `.gguf`
    /// files.
    ///
    /// Each file name (without extension) becomes the model ID.  Pre-existing
    /// entries are not removed; newly discovered files are added or updated.
    /// The file size is read from the filesystem metadata.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the directory cannot be read.
    pub fn scan_dir(&mut self, model_dir: &Path) -> Result<usize, InferenceError> {
        let mut added = 0usize;
        for entry in std::fs::read_dir(model_dir)? {
            let entry = entry?;
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str());
            if matches!(ext, Some("bin") | Some("gguf")) {
                let model_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                let file_size_bytes = std::fs::metadata(&path).ok().map(|m| m.len());
                self.register_full(model_id, path, "auto-discovered", file_size_bytes, Vec::new());
                added += 1;
            }
        }
        Ok(added)
    }
}
