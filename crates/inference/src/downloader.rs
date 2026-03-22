//! HuggingFace model downloader.
//!
//! Downloads GGUF model files from HuggingFace into the local cache directory
//! (`~/.pares-agens/models/` by default).  Progress is reported through a
//! [`DownloadProgress`] callback so callers can display a progress bar.

use std::path::PathBuf;

use reqwest::Client;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info, warn};

use crate::{
    config::InferenceConfig,
    error::InferenceError,
    registry::{KnownModel, ModelRegistry},
};

// ── DownloadProgress ──────────────────────────────────────────────────────────

/// Progress snapshot emitted during a model download.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    /// Bytes downloaded so far.
    pub downloaded: u64,

    /// Total expected bytes, or `None` if the server did not send
    /// `Content-Length`.
    pub total: Option<u64>,
}

impl DownloadProgress {
    /// Return the download fraction `[0.0, 1.0]`, or `None` if the total is
    /// unknown.
    #[must_use]
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|&t| t > 0)
            .map(|t| self.downloaded as f32 / t as f32)
    }
}

// ── ModelDownloader ───────────────────────────────────────────────────────────

/// Downloads models from HuggingFace and stores them in the local cache.
pub struct ModelDownloader {
    registry: ModelRegistry,
    client: Client,
}

impl ModelDownloader {
    /// Create a new downloader with the given configuration.
    #[must_use]
    pub fn new(config: InferenceConfig) -> Self {
        let client = Client::builder()
            .user_agent(concat!(
                "pares-agens-inference/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("failed to build reqwest client");

        Self {
            registry: ModelRegistry::new(config),
            client,
        }
    }

    /// Download a model by its [`id`](KnownModel::id).
    ///
    /// If the file already exists in the cache it is returned immediately
    /// without re-downloading.
    ///
    /// `progress_cb` is called periodically with the current download progress.
    /// Pass `|_| {}` to ignore progress.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::UnknownModel`] if `model_id` is not in the catalogue.
    /// - [`InferenceError::Http`] if the HTTP request fails.
    /// - [`InferenceError::DownloadIncomplete`] if the downloaded size does not
    ///   match the `Content-Length` header.
    /// - [`InferenceError::Io`] on filesystem errors.
    pub async fn download(
        &self,
        model_id: &str,
        progress_cb: impl Fn(DownloadProgress) + Send + 'static,
    ) -> Result<PathBuf, InferenceError> {
        let known = self
            .registry
            .find_known(model_id)
            .ok_or_else(|| InferenceError::UnknownModel(model_id.to_owned()))?;

        let dest = self.registry.model_path(known);

        if dest.exists() {
            debug!(?dest, "model already cached, skipping download");
            return Ok(dest);
        }

        self.download_known(known, &dest, progress_cb).await?;
        Ok(dest)
    }

    async fn download_known(
        &self,
        model: &KnownModel,
        dest: &PathBuf,
        progress_cb: impl Fn(DownloadProgress) + Send + 'static,
    ) -> Result<(), InferenceError> {
        let url = model.download_url();
        info!(%url, ?dest, "starting model download");

        // Ensure the models directory exists.
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let response = self.client.get(&url).send().await?.error_for_status()?;
        let total = response.content_length();

        // Write to a `.tmp` file first; rename on success.
        let tmp_dest = dest.with_extension("gguf.tmp");
        let mut file = tokio::fs::File::create(&tmp_dest).await?;

        let mut downloaded: u64 = 0;
        let mut response = response;

        while let Some(chunk) = response.chunk().await? {
            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;
            progress_cb(DownloadProgress { downloaded, total });
        }

        file.flush().await?;
        drop(file);

        // Verify size when `Content-Length` was provided.
        if let Some(expected) = total {
            if downloaded != expected {
                warn!(downloaded, expected, "size mismatch after download");
                tokio::fs::remove_file(&tmp_dest).await.ok();
                return Err(InferenceError::DownloadIncomplete {
                    expected,
                    got: downloaded,
                });
            }
        }

        tokio::fs::rename(&tmp_dest, dest).await?;
        info!(?dest, bytes = downloaded, "model download complete");
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_progress_fraction_with_total() {
        let p = DownloadProgress {
            downloaded: 50,
            total: Some(100),
        };
        assert!((p.fraction().unwrap() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn download_progress_fraction_without_total() {
        let p = DownloadProgress {
            downloaded: 50,
            total: None,
        };
        assert!(p.fraction().is_none());
    }

    #[tokio::test]
    async fn download_unknown_model_errors() {
        let dl = ModelDownloader::new(InferenceConfig::default());
        let result = dl.download("ghost-model", |_| {}).await;
        assert!(matches!(result, Err(InferenceError::UnknownModel(_))));
    }
}
