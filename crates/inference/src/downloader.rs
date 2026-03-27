//! Model file download and local cache management.

use std::path::{Path, PathBuf};

use crate::error::InferenceError;

/// Manages downloading and caching model files for local inference.
///
/// When the `hf-download` Cargo feature is enabled, [`ModelDownloader`] can
/// fetch models directly from Hugging Face Hub via HTTPS.  Without the
/// feature only the local-file helpers ([`verify_local`], [`install_from`],
/// [`evict`]) are available.
///
/// [`verify_local`]: ModelDownloader::verify_local
/// [`install_from`]: ModelDownloader::install_from
/// [`evict`]: ModelDownloader::evict
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
        Self {
            model_dir: model_dir.into(),
        }
    }

    /// Return the directory where models are cached.
    pub fn model_dir(&self) -> &Path {
        &self.model_dir
    }

    /// Return the expected on-disk path for `model_id`.
    ///
    /// The file name is `<model_id>.bin` inside `model_dir`.
    pub fn model_path(&self, model_id: &str) -> PathBuf {
        self.model_dir.join(format!("{model_id}.bin"))
    }

    /// Return the on-disk path for a model stored with a specific extension.
    pub fn model_path_with_ext(&self, model_id: &str, ext: &str) -> PathBuf {
        self.model_dir.join(format!("{model_id}.{ext}"))
    }

    /// Return `true` if the model file is already present on disk (checks
    /// both `.bin` and `.gguf` variants).
    pub fn verify_local(&self, model_id: &str) -> bool {
        self.model_path(model_id).is_file() || self.model_path_with_ext(model_id, "gguf").is_file()
    }

    /// Return the path of the cached model file, checking both `.gguf` and
    /// `.bin` variants.  Returns `None` if neither exists.
    pub fn cached_path(&self, model_id: &str) -> Option<PathBuf> {
        let gguf = self.model_path_with_ext(model_id, "gguf");
        if gguf.is_file() {
            return Some(gguf);
        }
        let bin = self.model_path(model_id);
        if bin.is_file() {
            return Some(bin);
        }
        None
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
    /// it was not present.  Both `.gguf` and `.bin` variants are checked.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::Io`] if the file exists but cannot be removed.
    pub fn evict(&self, model_id: &str) -> Result<bool, InferenceError> {
        let mut removed = false;
        for path in [
            self.model_path_with_ext(model_id, "gguf"),
            self.model_path(model_id),
        ] {
            if path.is_file() {
                std::fs::remove_file(&path)?;
                removed = true;
            }
        }
        Ok(removed)
    }
}

// ── Hugging Face download ─────────────────────────────────────────────────────

/// Information about a single file in a Hugging Face repository, as returned
/// by the Hub models API.
#[cfg(feature = "hf-download")]
#[derive(serde::Deserialize, Debug)]
struct HfSibling {
    rfilename: String,
}

/// Partial shape of the Hugging Face models API response that we care about.
#[cfg(feature = "hf-download")]
#[derive(serde::Deserialize, Debug)]
struct HfModelInfo {
    siblings: Vec<HfSibling>,
}

#[cfg(feature = "hf-download")]
impl ModelDownloader {
    /// Download a model from Hugging Face Hub.
    ///
    /// `repo` must be in `"{owner}/{name}"` format, e.g.
    /// `"microsoft/BitNet-b1.58-2B-4T"`.
    ///
    /// The method queries the Hub API to discover available model files,
    /// picks the first `.gguf` file (falling back to `.bin`), streams it to
    /// disk under `<model_dir>/<repo_name>.gguf` (or `.bin`), and returns the
    /// model ID and destination path.
    ///
    /// If the model is already cached it returns immediately without
    /// re-downloading.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::ModelNotFound`] — repository does not exist on the Hub.
    /// - [`InferenceError::Download`] — HTTP or I/O failure during transfer.
    pub async fn download_from_hf(&self, repo: &str) -> Result<(String, PathBuf), InferenceError> {
        use futures_util::StreamExt;
        use std::io::Write;

        // Derive a local model ID from the repository name (second path
        // component, e.g. "BitNet-b1.58-2B-4T").  Reject any component that
        // could lead to path traversal.
        let model_id = repo
            .split('/')
            .nth(1)
            .ok_or_else(|| InferenceError::Download {
                repo: repo.to_owned(),
                reason: "expected `owner/repo` format".to_owned(),
            })?
            .to_owned();

        if model_id.contains("..") || model_id.contains('/') || model_id.contains('\\') {
            return Err(InferenceError::Download {
                repo: repo.to_owned(),
                reason: "model ID contains illegal path characters".to_owned(),
            });
        }

        // Return early if already cached.
        if let Some(cached) = self.cached_path(&model_id) {
            tracing::info!(%model_id, path = %cached.display(), "model already cached — skipping download");
            return Ok((model_id, cached));
        }

        self.ensure_dir()?;

        let client = reqwest::Client::builder()
            .user_agent("pares-agens/model-downloader")
            .build()
            .map_err(|e| InferenceError::Download {
                repo: repo.to_owned(),
                reason: format!("failed to build HTTP client: {e}"),
            })?;

        // ── Step 1: Query the Hub API to find a downloadable model file ──────
        let api_url = format!("https://huggingface.co/api/models/{repo}");
        tracing::debug!(url = %api_url, "querying Hugging Face Hub API");

        let resp = client
            .get(&api_url)
            .send()
            .await
            .map_err(|e| InferenceError::Download {
                repo: repo.to_owned(),
                reason: format!("Hub API request failed: {e}"),
            })?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(InferenceError::ModelNotFound {
                repo: repo.to_owned(),
            });
        }
        if !resp.status().is_success() {
            return Err(InferenceError::Download {
                repo: repo.to_owned(),
                reason: format!("Hub API returned HTTP {}", resp.status()),
            });
        }

        let info: HfModelInfo = resp.json().await.map_err(|e| InferenceError::Download {
            repo: repo.to_owned(),
            reason: format!("failed to parse Hub API response: {e}"),
        })?;

        // Prefer .gguf; fall back to .bin.
        let chosen = info
            .siblings
            .iter()
            .find(|s| s.rfilename.ends_with(".gguf"))
            .or_else(|| info.siblings.iter().find(|s| s.rfilename.ends_with(".bin")))
            .ok_or_else(|| InferenceError::ModelNotFound {
                repo: repo.to_owned(),
            })?;

        let ext = if chosen.rfilename.ends_with(".gguf") {
            "gguf"
        } else {
            "bin"
        };
        let dest = self.model_path_with_ext(&model_id, ext);

        // ── Step 2: Stream the file to disk ───────────────────────────────────
        let file_url = format!(
            "https://huggingface.co/{repo}/resolve/main/{}",
            chosen.rfilename
        );
        tracing::info!(%model_id, url = %file_url, dest = %dest.display(), "downloading model");

        let download_resp =
            client
                .get(&file_url)
                .send()
                .await
                .map_err(|e| InferenceError::Download {
                    repo: repo.to_owned(),
                    reason: format!("download request failed: {e}"),
                })?;

        if !download_resp.status().is_success() {
            return Err(InferenceError::Download {
                repo: repo.to_owned(),
                reason: format!("file download returned HTTP {}", download_resp.status()),
            });
        }

        // Write the partial file to a temp path and rename on success to avoid
        // leaving a corrupt file behind if the download is interrupted.
        let tmp_dest = PathBuf::from(format!("{}.part", dest.display()));
        {
            let mut file =
                std::fs::File::create(&tmp_dest).map_err(|e| InferenceError::Download {
                    repo: repo.to_owned(),
                    reason: format!("failed to create temp file {}: {e}", tmp_dest.display()),
                })?;

            let mut stream = download_resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.map_err(|e| InferenceError::Download {
                    repo: repo.to_owned(),
                    reason: format!("stream error: {e}"),
                })?;
                file.write_all(&bytes)
                    .map_err(|e| InferenceError::Download {
                        repo: repo.to_owned(),
                        reason: format!("write error: {e}"),
                    })?;
            }
        }

        std::fs::rename(&tmp_dest, &dest).map_err(|e| InferenceError::Download {
            repo: repo.to_owned(),
            reason: format!("rename failed: {e}"),
        })?;

        tracing::info!(%model_id, dest = %dest.display(), "download complete");
        Ok((model_id, dest))
    }
}
