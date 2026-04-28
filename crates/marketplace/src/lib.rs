//! `pares-agens-marketplace` — Skill and extension marketplace for Pares Agens.
//!
//! Provides skill/extension discovery, metadata parsing and validation,
//! installation workflows, security checks for third-party code, and
//! LoRA adapter packaging and distribution.
//!
//! # Modules
//!
//! - [`adapter`] — [`Marketplace`](adapter::Marketplace) for packaging and listing LoRA adapters.
//! - [`discovery`] — [`MarketplaceClient`](discovery::MarketplaceClient) for listing and searching skills.
//! - [`installer`] — [`Installer`](installer::Installer) for managing skill installations.
//! - [`security`] — [`SecurityChecker`](security::SecurityChecker) for validating third-party code.

#![warn(missing_docs)]

pub mod adapter;
pub mod cli;
pub mod discovery;
pub mod installer;
pub mod permissions;
pub mod ratings;
pub mod security;
pub mod update;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod lora_types;
pub mod skill_category;
pub use lora_types::LoRAAdapter;
pub use skill_category::SkillCategory;

// ── Error type ───────────────────────────────────────────────────────────────

/// Errors that can occur during marketplace operations.
#[derive(Debug, Error)]
pub enum MarketplaceError {
    /// A network request to the marketplace API failed.
    #[error("network error: {0}")]
    NetworkError(String),

    /// Skill metadata is missing required fields or has invalid values.
    #[error("invalid metadata: {0}")]
    InvalidMetadata(String),

    /// The installation process failed.
    #[error("installation failed: {0}")]
    InstallationFailed(String),

    /// A security check rejected the skill or extension.
    #[error("security violation: {0}")]
    SecurityViolation(String),

    /// A requested skill was not found in the marketplace.
    #[error("skill not found: {0}")]
    NotFound(String),

    /// Packaging a LoRA adapter for distribution failed.
    #[error("packaging failed: {0}")]
    PackagingFailed(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

// ── Skill metadata ────────────────────────────────────────────────────────────

/// Metadata for a skill or extension listed in the marketplace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Unique identifier for this skill (e.g. `"pares/rust-helper"`).
    pub id: String,

    /// Human-readable display name.
    pub name: String,

    /// Semantic version string (e.g. `"1.2.0"`).
    pub version: String,

    /// Short description of the skill's capabilities.
    pub description: String,

    /// Publisher or author of the skill.
    pub author: String,

    /// One or more skill categories that describe the domain.
    pub categories: Vec<SkillCategory>,

    /// SHA-256 hex digest of the skill archive (used for integrity checks).
    pub checksum: String,

    /// URL from which the skill archive can be downloaded.
    pub download_url: String,

    /// Optional detached signature over the skill archive (base64-encoded).
    pub signature: Option<String>,
}

// ── Metadata validator ────────────────────────────────────────────────────────

/// Validates [`SkillMetadata`] for completeness and correctness.
#[derive(Debug, Default)]
pub struct MetadataValidator;

impl MetadataValidator {
    /// Create a new `MetadataValidator`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Validate `metadata`, returning an error that describes the first
    /// problem found.
    ///
    /// Checks performed (in order):
    ///
    /// 1. `id` — non-empty and contains only alphanumeric characters, hyphens,
    ///    underscores, or `/`.
    /// 2. `name` — non-empty.
    /// 3. `version` — non-empty and roughly semver-shaped (`x.y.z`).
    /// 4. `description` — non-empty.
    /// 5. `author` — non-empty.
    /// 6. `categories` — at least one entry.
    /// 7. `checksum` — exactly 64 hex characters (SHA-256).
    /// 8. `download_url` — non-empty and starts with `https://`.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when any check fails.
    pub fn validate(&self, metadata: &SkillMetadata) -> Result<(), MarketplaceError> {
        if metadata.id.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "id must not be empty".to_string(),
            ));
        }
        if !metadata
            .id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/')
        {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "id '{}' contains invalid characters (allowed: alphanumeric, '-', '_', '/')",
                metadata.id
            )));
        }
        if metadata.name.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "name must not be empty".to_string(),
            ));
        }
        if !is_valid_semver(&metadata.version) {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "version '{}' is not a valid semver string (expected x.y.z)",
                metadata.version
            )));
        }
        if metadata.description.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "description must not be empty".to_string(),
            ));
        }
        if metadata.author.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "author must not be empty".to_string(),
            ));
        }
        if metadata.categories.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "categories must contain at least one entry".to_string(),
            ));
        }
        if !is_valid_sha256_hex(&metadata.checksum) {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "checksum '{}' is not a valid SHA-256 hex digest (expected 64 hex characters)",
                metadata.checksum
            )));
        }
        if metadata.download_url.is_empty() {
            return Err(MarketplaceError::InvalidMetadata(
                "download_url must not be empty".to_string(),
            ));
        }
        if !metadata.download_url.starts_with("https://") {
            return Err(MarketplaceError::InvalidMetadata(format!(
                "download_url '{}' must use HTTPS",
                metadata.download_url
            )));
        }
        Ok(())
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Returns `true` for strings of the form `major.minor.patch` where each
/// component is a non-negative integer.
fn is_valid_semver(version: &str) -> bool {
    if version.is_empty() {
        return false;
    }
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() < 3 {
        return false;
    }
    parts[..3]
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Returns `true` when `s` is exactly 64 lowercase hexadecimal characters.
fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A valid `SkillMetadata` fixture used across multiple tests.
    pub(crate) fn valid_metadata() -> SkillMetadata {
        SkillMetadata {
            id: "pares/rust-helper".to_string(),
            name: "Rust Helper".to_string(),
            version: "1.0.0".to_string(),
            description: "Helps write idiomatic Rust code.".to_string(),
            author: "pares".to_string(),
            categories: vec![SkillCategory::Coding("rust".to_string())],
            checksum: "a".repeat(64),
            download_url: "https://marketplace.example.com/skills/rust-helper-1.0.0.tar.gz"
                .to_string(),
            signature: None,
        }
    }

    // ── MetadataValidator ────────────────────────────────────────────────────

    #[test]
    fn valid_metadata_passes_validation() {
        let v = MetadataValidator::new();
        assert!(v.validate(&valid_metadata()).is_ok());
    }

    #[test]
    fn rejects_empty_id() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.id = String::new();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_id_with_invalid_characters() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.id = "bad id!".to_string();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_empty_name() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.name = String::new();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_invalid_version() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.version = "not-a-version".to_string();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_short_version() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.version = "1.0".to_string();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_empty_description() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.description = String::new();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_empty_author() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.author = String::new();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_empty_categories() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.categories = vec![];
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_malformed_checksum() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.checksum = "tooshort".to_string();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_non_hex_checksum() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.checksum = "z".repeat(64);
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_empty_download_url() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.download_url = String::new();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn rejects_http_download_url() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.download_url = "http://insecure.example.com/skill.tar.gz".to_string();
        assert!(matches!(
            v.validate(&m),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn accepts_metadata_with_signature() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.signature = Some("c2lnbmF0dXJl".to_string());
        assert!(v.validate(&m).is_ok());
    }

    #[test]
    fn accepts_metadata_with_multiple_categories() {
        let v = MetadataValidator::new();
        let mut m = valid_metadata();
        m.categories = vec![
            SkillCategory::Coding("rust".to_string()),
            SkillCategory::Analysis("technical".to_string()),
        ];
        assert!(v.validate(&m).is_ok());
    }
}
