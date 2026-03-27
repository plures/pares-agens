//! Auto-update checking for installed marketplace procedures.
//!
//! [`UpdateChecker`] compares the versions of locally installed skills against
//! a remote catalogue and reports which ones have newer versions available.
//!
//! # Version comparison
//!
//! Versions are compared component-by-component as unsigned integers
//! (`major.minor.patch`).  Pre-release suffixes are ignored for simplicity.
//!
//! # Stub behaviour
//!
//! The checker operates against an in-memory remote catalogue that is seeded
//! at construction time.  A production implementation would fetch the
//! catalogue from the marketplace HTTP API.

use crate::{installer::InstalledSkill, MarketplaceError, SkillMetadata};
use serde::{Deserialize, Serialize};

// ── UpdateAvailable ───────────────────────────────────────────────────────────

/// Describes an available update for an installed skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAvailable {
    /// Identifier of the skill that can be updated.
    pub skill_id: String,

    /// Version currently installed on this system.
    pub installed_version: String,

    /// Newer version available in the remote catalogue.
    pub available_version: String,

    /// Release notes for the new version, if provided.
    pub release_notes: Option<String>,
}

// ── UpdateChecker ─────────────────────────────────────────────────────────────

/// Checks installed skills against the remote catalogue for newer versions.
#[derive(Debug)]
pub struct UpdateChecker {
    /// Remote catalogue of latest available skill versions.
    remote_catalogue: Vec<SkillMetadata>,
}

impl UpdateChecker {
    /// Create a new [`UpdateChecker`] with an empty remote catalogue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            remote_catalogue: Vec::new(),
        }
    }

    /// Seed the remote catalogue used for version comparisons.
    #[must_use]
    pub fn with_catalogue(mut self, catalogue: Vec<SkillMetadata>) -> Self {
        self.remote_catalogue = catalogue;
        self
    }

    /// Register a single entry in the remote catalogue.
    pub fn register_remote(&mut self, skill: SkillMetadata) {
        self.remote_catalogue.push(skill);
    }

    /// Check `installed` skills against the remote catalogue.
    ///
    /// Returns a list of [`UpdateAvailable`] entries for skills that have a
    /// newer version in the catalogue.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InvalidMetadata`] when an installed skill
    /// carries a version string that cannot be parsed as `major.minor.patch`.
    pub fn check_updates(
        &self,
        installed: &[InstalledSkill],
    ) -> Result<Vec<UpdateAvailable>, MarketplaceError> {
        let mut updates = Vec::new();

        for local in installed {
            let local_ver = parse_semver(&local.metadata.version).ok_or_else(|| {
                MarketplaceError::InvalidMetadata(format!(
                    "installed skill '{}' has unparseable version '{}'",
                    local.metadata.id, local.metadata.version
                ))
            })?;

            if let Some(remote) = self
                .remote_catalogue
                .iter()
                .find(|r| r.id == local.metadata.id)
            {
                if let Some(remote_ver) = parse_semver(&remote.version) {
                    if remote_ver > local_ver {
                        updates.push(UpdateAvailable {
                            skill_id: local.metadata.id.clone(),
                            installed_version: local.metadata.version.clone(),
                            available_version: remote.version.clone(),
                            release_notes: None,
                        });
                    }
                }
            }
        }

        Ok(updates)
    }

    /// Return metadata from the remote catalogue for a single skill id, or
    /// `None` when the skill is not in the catalogue.
    #[must_use]
    pub fn remote_metadata(&self, skill_id: &str) -> Option<&SkillMetadata> {
        self.remote_catalogue.iter().find(|r| r.id == skill_id)
    }
}

impl Default for UpdateChecker {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Parse `"major.minor.patch"` into a comparable tuple.
///
/// Returns `None` when the string does not match the pattern.
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let parts: Vec<&str> = version.splitn(4, '.').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts[1].parse::<u64>().ok()?;
    // Strip any pre-release suffix from patch component.
    let patch_str = parts[2].split('-').next().unwrap_or(parts[2]);
    let patch = patch_str.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{installer::InstalledSkill, SkillCategory, SkillMetadata};

    fn make_skill(id: &str, version: &str) -> SkillMetadata {
        SkillMetadata {
            id: id.to_string(),
            name: id.to_string(),
            version: version.to_string(),
            description: "test skill".to_string(),
            author: "pares".to_string(),
            categories: vec![SkillCategory::Coding("rust".to_string())],
            checksum: "a".repeat(64),
            download_url: format!("https://marketplace.example.com/{id}.tar.gz"),
            signature: None,
        }
    }

    fn make_installed(id: &str, version: &str) -> InstalledSkill {
        InstalledSkill {
            metadata: make_skill(id, version),
            install_path: format!("/skills/{id}-{version}"),
            installed_at: "2026-01-01T00:00:00Z".to_string(),
            last_used: None,
        }
    }

    // ── parse_semver ──────────────────────────────────────────────────────────

    #[test]
    fn parse_semver_parses_valid_version() {
        assert_eq!(parse_semver("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_semver_returns_none_for_invalid() {
        assert_eq!(parse_semver("not-a-version"), None);
        assert_eq!(parse_semver("1.0"), None);
    }

    #[test]
    fn parse_semver_strips_prerelease_suffix() {
        assert_eq!(parse_semver("1.2.3-beta"), Some((1, 2, 3)));
    }

    // ── UpdateChecker::check_updates ──────────────────────────────────────────

    #[test]
    fn no_updates_when_catalogue_empty() {
        let checker = UpdateChecker::new();
        let installed = vec![make_installed("pares/rust-helper", "1.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn no_updates_when_versions_are_equal() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.0")]);
        let installed = vec![make_installed("pares/rust-helper", "1.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn detects_patch_update() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.1")]);
        let installed = vec![make_installed("pares/rust-helper", "1.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].skill_id, "pares/rust-helper");
        assert_eq!(updates[0].installed_version, "1.0.0");
        assert_eq!(updates[0].available_version, "1.0.1");
    }

    #[test]
    fn detects_minor_update() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.1.0")]);
        let installed = vec![make_installed("pares/rust-helper", "1.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert_eq!(updates.len(), 1);
    }

    #[test]
    fn detects_major_update() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "2.0.0")]);
        let installed = vec![make_installed("pares/rust-helper", "1.9.9")];
        let updates = checker.check_updates(&installed).unwrap();
        assert_eq!(updates.len(), 1);
    }

    #[test]
    fn no_update_when_local_is_newer() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.0")]);
        let installed = vec![make_installed("pares/rust-helper", "2.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn skill_not_in_catalogue_is_skipped() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/other-skill", "2.0.0")]);
        let installed = vec![make_installed("pares/rust-helper", "1.0.0")];
        let updates = checker.check_updates(&installed).unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn multiple_skills_independently_checked() {
        let checker = UpdateChecker::new().with_catalogue(vec![
            make_skill("pares/rust-helper", "1.0.1"),
            make_skill("pares/essay-writer", "2.0.0"),
        ]);
        let installed = vec![
            make_installed("pares/rust-helper", "1.0.0"),
            make_installed("pares/essay-writer", "2.0.0"),
        ];
        let updates = checker.check_updates(&installed).unwrap();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].skill_id, "pares/rust-helper");
    }

    #[test]
    fn invalid_installed_version_returns_error() {
        let checker = UpdateChecker::new();
        let mut installed = make_installed("pares/rust-helper", "1.0.0");
        installed.metadata.version = "bad-version".to_string();
        let result = checker.check_updates(&[installed]);
        assert!(matches!(result, Err(MarketplaceError::InvalidMetadata(_))));
    }

    #[test]
    fn remote_metadata_returns_correct_entry() {
        let checker =
            UpdateChecker::new().with_catalogue(vec![make_skill("pares/rust-helper", "1.0.1")]);
        let meta = checker.remote_metadata("pares/rust-helper");
        assert!(meta.is_some());
        assert_eq!(meta.unwrap().version, "1.0.1");
    }

    #[test]
    fn remote_metadata_returns_none_for_unknown() {
        let checker = UpdateChecker::new();
        assert!(checker.remote_metadata("unknown/skill").is_none());
    }
}
