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
    pub fn register(
        &mut self,
        model_id: impl Into<String>,
        model_path: PathBuf,
        description: impl Into<String>,
    ) {
        let model_id = model_id.into();
        self.entries.insert(
            model_id.clone(),
            ModelEntry {
                model_id,
                model_path,
                description: description.into(),
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

    /// Populate the registry by scanning `model_dir` for `*.bin` files.
    ///
    /// Each file name (without extension) becomes the model ID.  Pre-existing
    /// entries are not removed; newly discovered files are added or updated.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the directory cannot be read.
    pub fn scan_dir(&mut self, model_dir: &Path) -> Result<usize, InferenceError> {
        let mut added = 0usize;
        for entry in std::fs::read_dir(model_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("bin") {
                let model_id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                self.register(model_id, path, "auto-discovered");
                added += 1;
            }
        }
        Ok(added)
    }
}
