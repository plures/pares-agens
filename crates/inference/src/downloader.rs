use std::path::{Path, PathBuf};

use crate::error::InferenceError;

/// Manages downloading and caching model files for local inference.
///
/// `ModelDownloader` does not perform network I/O directly — it delegates to
/// the caller-supplied download function so that the inference crate stays
/// network-agnostic.  For convenience, [`ModelDownloader::verify_local`] and
/// [`ModelDownloader::model_path`] let callers check whether a model is
/// already cached before deciding to download it.
///
/// # Example
///
/// ```rust
/// use pares_agens_inference::ModelDownloader;
/// use std::path::PathBuf;
///
/// let dl = ModelDownloader::new(PathBuf::from("/var/lib/pares-agens/models"));
///
/// // Check whether the model is already present before fetching.
/// if !dl.verify_local("bitnet-b1.58-3b") {
///     println!("Model not cached; download required.");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct ModelDownloader {
    /// Directory where downloaded model files are stored.
    model_dir: PathBuf,
}

impl ModelDownloader {
    /// Create a new downloader that stores models under `model_dir`.
    pub fn new(model_dir: impl Into<PathBuf>) -> Self {
        Self { model_dir: model_dir.into() }
    }

    /// Return the expected on-disk path for `model_id`.
    ///
    /// The file name is `<model_id>.bin` inside `model_dir`.
    pub fn model_path(&self, model_id: &str) -> PathBuf {
        self.model_dir.join(format!("{model_id}.bin"))
    }

    /// Return `true` if the model file is already present on disk.
    pub fn verify_local(&self, model_id: &str) -> bool {
        self.model_path(model_id).is_file()
    }

    /// Ensure the model directory exists, creating it if necessary.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the directory cannot be created.
    pub fn ensure_dir(&self) -> Result<(), InferenceError> {
        std::fs::create_dir_all(&self.model_dir)?;
        Ok(())
    }

    /// Register a model that was downloaded externally into the downloader's
    /// model directory.
    ///
    /// This copies `src` to the expected path for `model_id` inside
    /// `model_dir`.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::Io`] — source file cannot be read or destination
    ///   cannot be written.
    pub fn install_from(&self, model_id: &str, src: &Path) -> Result<PathBuf, InferenceError> {
        self.ensure_dir()?;
        let dest = self.model_path(model_id);
        std::fs::copy(src, &dest)?;
        Ok(dest)
    }

    /// Remove a cached model file from disk.
    ///
    /// Returns `Ok(true)` if the file existed and was removed, `Ok(false)` if
    /// it was not present.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the file exists but cannot be removed.
    pub fn evict(&self, model_id: &str) -> Result<bool, InferenceError> {
        let path = self.model_path(model_id);
        if path.is_file() {
            std::fs::remove_file(&path)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
