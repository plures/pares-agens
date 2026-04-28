//! Plugin error types.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin '{0}' already installed")]
    AlreadyInstalled(String),

    #[error("plugin '{0}' not found")]
    NotFound(String),

    #[error("invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("schema registration failed: {0}")]
    SchemaRegistration(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("TOML parse error: {0}")]
    TomlParse(String),
}
