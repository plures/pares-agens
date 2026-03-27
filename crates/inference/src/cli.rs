//! CLI module — `pares-agens model` sub-commands.
//!
//! Provides the [`ModelCommand`] enum and the async [`run_cli`] dispatcher
//! that implements:
//!
//! ```text
//! pares-agens model list
//! pares-agens model download <REPO>
//! pares-agens model remove <MODEL_ID>
//! ```
//!
//! The `download` sub-command requires the `hf-download` Cargo feature.

use std::path::PathBuf;
use std::str::FromStr;

use crate::{
    downloader::ModelDownloader,
    error::InferenceError,
    registry::{default_cache_dir, ModelRegistry},
};

// ── ModelCommand ──────────────────────────────────────────────────────────────

/// Sub-commands available under the `model` CLI group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCommand {
    /// List all locally cached models.
    List,
    /// Download a model from Hugging Face Hub.
    ///
    /// `repo` must be in `"{owner}/{name}"` format, e.g.
    /// `"microsoft/BitNet-b1.58-2B-4T"`.
    Download {
        /// HuggingFace repository (`owner/name`).
        repo: String,
    },
    /// Remove a locally cached model.
    Remove {
        /// Model ID as shown by `model list`.
        model_id: String,
    },
}

/// Error returned when an unknown or malformed model command string is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownModelCommand(pub String);

impl std::fmt::Display for UnknownModelCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown model command: '{}'", self.0)
    }
}
impl std::error::Error for UnknownModelCommand {}

impl FromStr for ModelCommand {
    type Err = UnknownModelCommand;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "list" => Ok(Self::List),
            other => Err(UnknownModelCommand(other.to_owned())),
        }
    }
}

// ── run_cli ───────────────────────────────────────────────────────────────────

/// Dispatch a [`ModelCommand`] using `cache_dir` as the model directory.
///
/// Writes human-readable output to `stdout`.  Pass `None` for `cache_dir`
/// to use the platform default (`~/.pares-agens/models`).
///
/// The `hf-download` feature must be enabled to use [`ModelCommand::Download`].
pub async fn run_cli(cmd: &ModelCommand, cache_dir: Option<PathBuf>) -> Result<(), InferenceError> {
    let dir = cache_dir.unwrap_or_else(default_cache_dir);
    match cmd {
        ModelCommand::List => cmd_list(&dir),
        ModelCommand::Download { repo } => cmd_download(&dir, repo).await,
        ModelCommand::Remove { model_id } => cmd_remove(&dir, model_id),
    }
}

// ── list ──────────────────────────────────────────────────────────────────────

// ── Column layout constants ───────────────────────────────────────────────────

const COL_MODEL_ID: usize = 40;
const COL_SIZE: usize = 12;
const SEPARATOR_WIDTH: usize = COL_MODEL_ID + 1 + COL_SIZE + 2 + "Description".len();

fn cmd_list(cache_dir: &std::path::Path) -> Result<(), InferenceError> {
    if !cache_dir.exists() {
        println!(
            "No models cached (directory does not exist: {}).",
            cache_dir.display()
        );
        return Ok(());
    }

    let mut registry = ModelRegistry::new();
    let count = registry.scan_dir(cache_dir)?;

    if count == 0 {
        println!("No models cached in {}.", cache_dir.display());
        return Ok(());
    }

    let mut entries: Vec<_> = registry.entries().collect();
    entries.sort_by(|a, b| a.model_id.cmp(&b.model_id));

    println!(
        "{:<COL_MODEL_ID$} {:>COL_SIZE$}  Description",
        "Model ID", "Size"
    );
    println!("{}", "-".repeat(SEPARATOR_WIDTH));
    for entry in entries {
        let size_str = match entry.file_size_bytes {
            Some(bytes) => format_size(bytes),
            None => "unknown".to_string(),
        };
        println!(
            "{:<COL_MODEL_ID$} {:>COL_SIZE$}  {}",
            entry.model_id, size_str, entry.description
        );
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    const KB: u64 = 1 << 10;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ── download ──────────────────────────────────────────────────────────────────

async fn cmd_download(cache_dir: &std::path::Path, repo: &str) -> Result<(), InferenceError> {
    #[cfg(feature = "hf-download")]
    {
        let dl = ModelDownloader::new(cache_dir);
        println!("Downloading `{repo}` …");
        let (model_id, dest) = dl.download_from_hf(repo).await?;
        let size = std::fs::metadata(&dest).ok().map(|m| m.len());
        let size_str = size.map_or("unknown size".to_string(), format_size);
        println!("✓ Saved `{model_id}` ({size_str}) → {}", dest.display());
        Ok(())
    }

    #[cfg(not(feature = "hf-download"))]
    {
        let _ = (cache_dir, repo);
        eprintln!(
            "error: the `hf-download` feature is not enabled.\n\
             Recompile with `--features hf-download` to enable Hugging Face downloads."
        );
        Err(InferenceError::Download {
            repo: repo.to_owned(),
            reason: "`hf-download` feature not enabled".to_owned(),
        })
    }
}

// ── remove ────────────────────────────────────────────────────────────────────

fn cmd_remove(cache_dir: &std::path::Path, model_id: &str) -> Result<(), InferenceError> {
    let dl = ModelDownloader::new(cache_dir);
    if dl.evict(model_id)? {
        println!("✓ Removed model `{model_id}`.");
    } else {
        println!("Model `{model_id}` is not cached — nothing to remove.");
    }
    Ok(())
}

// ── print_usage ───────────────────────────────────────────────────────────────

/// Print usage information for the `model` CLI group.
pub fn print_usage() {
    println!("Usage: pares-agens model <COMMAND> [ARGS]");
    println!();
    println!("Commands:");
    println!("  list                    Show all locally cached models");
    println!("  download <REPO>         Download a model from Hugging Face Hub");
    println!("  remove <MODEL_ID>       Remove a locally cached model");
    println!();
    println!("Examples:");
    println!("  pares-agens model list");
    println!("  pares-agens model download microsoft/BitNet-b1.58-2B-4T");
    println!("  pares-agens model remove BitNet-b1.58-2B-4T");
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_list() {
        assert!(matches!(
            "list".parse::<ModelCommand>(),
            Ok(ModelCommand::List)
        ));
        assert!(matches!(
            "LIST".parse::<ModelCommand>(),
            Ok(ModelCommand::List)
        ));
        assert!(matches!(
            "List".parse::<ModelCommand>(),
            Ok(ModelCommand::List)
        ));
    }

    #[test]
    fn from_str_unknown_returns_err() {
        assert!("download".parse::<ModelCommand>().is_err());
        assert!("remove".parse::<ModelCommand>().is_err());
        assert!("".parse::<ModelCommand>().is_err());
    }

    #[tokio::test]
    async fn run_cli_list_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_cli(&ModelCommand::List, Some(tmp.path().to_path_buf())).await;
        assert!(
            result.is_ok(),
            "list on empty dir should not error: {result:?}"
        );
    }

    #[tokio::test]
    async fn run_cli_list_shows_models() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a fake .gguf model file with known size.
        let model_path = tmp.path().join("my-model.gguf");
        std::fs::write(&model_path, vec![0u8; 1024]).unwrap();

        let result = run_cli(&ModelCommand::List, Some(tmp.path().to_path_buf())).await;
        assert!(result.is_ok(), "list should succeed: {result:?}");
    }

    #[tokio::test]
    async fn run_cli_remove_missing_model_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let result = run_cli(
            &ModelCommand::Remove {
                model_id: "nonexistent".to_string(),
            },
            Some(tmp.path().to_path_buf()),
        )
        .await;
        assert!(
            result.is_ok(),
            "removing missing model should not error: {result:?}"
        );
    }

    #[tokio::test]
    async fn run_cli_remove_existing_model() {
        let tmp = tempfile::tempdir().unwrap();
        let model_path = tmp.path().join("test-model.gguf");
        std::fs::write(&model_path, b"fake model").unwrap();
        assert!(model_path.exists());

        let result = run_cli(
            &ModelCommand::Remove {
                model_id: "test-model".to_string(),
            },
            Some(tmp.path().to_path_buf()),
        )
        .await;
        assert!(result.is_ok(), "remove should succeed: {result:?}");
        assert!(!model_path.exists(), "model file should be deleted");
    }

    #[tokio::test]
    async fn run_cli_list_no_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        let result = run_cli(&ModelCommand::List, Some(nonexistent)).await;
        assert!(
            result.is_ok(),
            "list with missing dir should not error: {result:?}"
        );
    }

    #[test]
    fn format_size_scales_correctly() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }
}
