#![warn(missing_docs)]
//! `pares-agens-migrate` — OpenClaw → Pares Agens migration library.
//!
//! Provides the [`migrate::run`] function and supporting types for importing
//! data from an existing OpenClaw installation into pares-agens format.

pub mod migrate;
pub mod openclaw;

use std::path::PathBuf;

/// Top-level error type for migration operations.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// Failed to read a source file from the OpenClaw installation directory.
    #[error("failed to read {path}: {source}")]
    Read {
        /// Path of the file that could not be read.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to parse JSON from a source file.
    #[error("failed to parse {path}: {source}")]
    Parse {
        /// Path of the file that could not be parsed.
        path: PathBuf,
        /// Underlying JSON parse error.
        #[source]
        source: serde_json::Error,
    },
    /// Failed to write an output file.
    #[error("failed to write {path}: {source}")]
    Write {
        /// Path of the file that could not be written.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// Failed to serialize data to JSON for output.
    #[error("serialization failed: {0}")]
    Serialize(serde_json::Error),
}
