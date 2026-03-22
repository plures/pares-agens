//! CLI subcommands for model management.
//!
//! Exposes `pares-agens model list|download|remove` via [`ModelCli`].
//!
//! # Usage
//!
//! ```text
//! pares-agens model list
//! pares-agens model download bitnet-2b
//! pares-agens model remove bitnet-2b
//! ```

use clap::{Parser, Subcommand};

use crate::{
    config::InferenceConfig,
    downloader::ModelDownloader,
    registry::ModelRegistry,
};

// ── CLI types ─────────────────────────────────────────────────────────────────

/// Model management commands.
#[derive(Debug, Parser)]
pub struct ModelCli {
    #[command(subcommand)]
    pub command: ModelCommand,
}

/// Available model management subcommands.
#[derive(Debug, Subcommand)]
pub enum ModelCommand {
    /// List known models and their local download status.
    List,

    /// Download a model from HuggingFace into the local cache.
    Download {
        /// Model identifier (e.g. `bitnet-2b`, `llama3-8b-bitnet`).
        id: String,
    },

    /// Remove a downloaded model from the local cache.
    Remove {
        /// Model identifier to delete.
        id: String,
    },
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Execute the requested model CLI command.
///
/// Returns `Ok(())` on success; the human-readable result is printed to
/// `stdout`.  Errors are returned for the caller to format.
///
/// # Errors
///
/// Propagates [`crate::error::InferenceError`] on failure.
pub async fn run(cli: ModelCli, config: InferenceConfig) -> Result<(), crate::error::InferenceError> {
    match cli.command {
        ModelCommand::List => cmd_list(&config),
        ModelCommand::Download { id } => cmd_download(&id, config).await,
        ModelCommand::Remove { id } => cmd_remove(&id, &config),
    }
}

fn cmd_list(config: &InferenceConfig) -> Result<(), crate::error::InferenceError> {
    let registry = ModelRegistry::new(config.clone());
    let known = registry.known_models();

    println!("{:<22} {:<10} {:<8} DESCRIPTION", "ID", "SIZE", "STATUS");
    println!("{}", "-".repeat(80));

    for model in known {
        let status = if registry.is_downloaded(model.id) {
            "✓ local"
        } else {
            "─ remote"
        };
        let size_gib = model.size_bytes as f64 / 1_073_741_824.0;
        println!(
            "{:<22} {:<10} {:<8} {}",
            model.id,
            format!("{:.2} GiB", size_gib),
            status,
            model.description
        );
    }

    Ok(())
}

async fn cmd_download(
    id: &str,
    config: InferenceConfig,
) -> Result<(), crate::error::InferenceError> {
    let dl = ModelDownloader::new(config);

    println!("Downloading model '{id}' …");

    let dest = dl
        .download(id, move |progress| {
            if let Some(pct) = progress.fraction() {
                print!("\r  {:.1}%  ({} MiB)", pct * 100.0, progress.downloaded / 1_048_576);
            } else {
                print!("\r  {} MiB", progress.downloaded / 1_048_576);
            }
        })
        .await?;

    println!("\nDownloaded to: {}", dest.display());
    Ok(())
}

fn cmd_remove(id: &str, config: &InferenceConfig) -> Result<(), crate::error::InferenceError> {
    let registry = ModelRegistry::new(config.clone());
    registry.remove(id)?;
    println!("Removed model '{id}'.");
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_list_command() {
        let cli = ModelCli::parse_from(["model", "list"]);
        assert!(matches!(cli.command, ModelCommand::List));
    }

    #[test]
    fn parse_download_command() {
        let cli = ModelCli::parse_from(["model", "download", "bitnet-2b"]);
        assert!(matches!(
            cli.command,
            ModelCommand::Download { id } if id == "bitnet-2b"
        ));
    }

    #[test]
    fn parse_remove_command() {
        let cli = ModelCli::parse_from(["model", "remove", "llama3-8b-bitnet"]);
        assert!(matches!(
            cli.command,
            ModelCommand::Remove { id } if id == "llama3-8b-bitnet"
        ));
    }
}
