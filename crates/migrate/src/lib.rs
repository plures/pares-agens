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
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("serialization failed: {0}")]
    Serialize(serde_json::Error),
}
