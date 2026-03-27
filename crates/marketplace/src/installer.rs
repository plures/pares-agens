//! Installation workflow for marketplace skills and extensions.
//!
//! [`Installer`] manages the lifecycle of locally installed skills:
//! downloading, verifying integrity via [`crate::security::SecurityChecker`],
//! persisting an installation record, and uninstalling.
//!
//! # Stub behaviour
//!
//! The current implementation does not perform real filesystem I/O or
//! network downloads.  It maintains an in-memory registry of installed
//! skills so that the full workflow can be exercised and tested without
//! external infrastructure.

use crate::{MarketplaceError, MetadataValidator, SkillMetadata};
use serde::{Deserialize, Serialize};

// ── Installed skill ───────────────────────────────────────────────────────────

/// A skill that has been installed on the local system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    /// Metadata describing the skill.
    pub metadata: SkillMetadata,

    /// Absolute path to the directory where the skill was installed.
    pub install_path: String,

    /// ISO 8601 timestamp of when the skill was installed.
    pub installed_at: String,

    /// ISO 8601 timestamp of the most recent execution of this skill, or
    /// `None` when the skill has never been used.
    pub last_used: Option<String>,
}

// ── Installer ─────────────────────────────────────────────────────────────────

/// Manages the download, verification, and installation of marketplace skills.
#[derive(Debug)]
pub struct Installer {
    /// Root directory under which skills are installed.
    install_dir: String,

    /// In-memory registry of currently installed skills.
    installed: Vec<InstalledSkill>,
}

impl Installer {
    /// Create a new `Installer` that stores skills under `install_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::InstallationFailed`] when `install_dir`
    /// is empty.
    pub fn new(install_dir: &str) -> Result<Self, MarketplaceError> {
        if install_dir.is_empty() {
            return Err(MarketplaceError::InstallationFailed(
                "install_dir must not be empty".to_string(),
            ));
        }
        Ok(Self {
            install_dir: install_dir.to_string(),
            installed: Vec::new(),
        })
    }

    /// Install `metadata` and register it in the local catalogue.
    ///
    /// The workflow is:
    /// 1. Validate metadata with [`MetadataValidator`].
    /// 2. (Stub) download and verify the skill archive.
    /// 3. Record the installation.
    ///
    /// # Errors
    ///
    /// - [`MarketplaceError::InvalidMetadata`] — metadata validation failed.
    /// - [`MarketplaceError::InstallationFailed`] — the skill is already
    ///   installed or the archive download/verification failed.
    pub fn install(&mut self, metadata: SkillMetadata) -> Result<InstalledSkill, MarketplaceError> {
        // 1. Validate metadata.
        MetadataValidator::new().validate(&metadata)?;

        // 2. Reject duplicate installations.
        if self.installed.iter().any(|s| s.metadata.id == metadata.id) {
            return Err(MarketplaceError::InstallationFailed(format!(
                "skill '{}' is already installed",
                metadata.id
            )));
        }

        // 3. Derive install path and record.
        let install_path = format!(
            "{}/{}-{}",
            self.install_dir,
            metadata.id.replace('/', "-"),
            metadata.version
        );

        let installed_skill = InstalledSkill {
            metadata,
            install_path,
            installed_at: timestamp_now(),
            last_used: None,
        };

        self.installed.push(installed_skill.clone());
        Ok(installed_skill)
    }

    /// Uninstall the skill identified by `skill_id`.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::NotFound`] when no skill with the given
    /// `skill_id` is currently installed.
    pub fn uninstall(&mut self, skill_id: &str) -> Result<(), MarketplaceError> {
        let pos = self
            .installed
            .iter()
            .position(|s| s.metadata.id == skill_id)
            .ok_or_else(|| MarketplaceError::NotFound(skill_id.to_string()))?;

        self.installed.remove(pos);
        Ok(())
    }

    /// Return a list of all currently installed skills.
    #[must_use]
    pub fn list_installed(&self) -> &[InstalledSkill] {
        &self.installed
    }

    /// Return `true` when a skill with the given `skill_id` is installed.
    #[must_use]
    pub fn is_installed(&self, skill_id: &str) -> bool {
        self.installed.iter().any(|s| s.metadata.id == skill_id)
    }

    /// Record that the skill identified by `skill_id` was used now.
    ///
    /// Updates the `last_used` timestamp to the current placeholder value.
    ///
    /// # Errors
    ///
    /// Returns [`MarketplaceError::NotFound`] when no skill with the given
    /// `skill_id` is currently installed.
    pub fn record_use(&mut self, skill_id: &str) -> Result<(), MarketplaceError> {
        let skill = self
            .installed
            .iter_mut()
            .find(|s| s.metadata.id == skill_id)
            .ok_or_else(|| MarketplaceError::NotFound(skill_id.to_string()))?;
        skill.last_used = Some(timestamp_now());
        Ok(())
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Returns a fixed placeholder timestamp.
///
/// A production implementation would use the system clock or `chrono`.
fn timestamp_now() -> String {
    "2026-01-01T00:00:00Z".to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::valid_metadata;

    fn make_installer() -> Installer {
        Installer::new("/skills").unwrap()
    }

    // ── construction ─────────────────────────────────────────────────────────

    #[test]
    fn new_rejects_empty_install_dir() {
        assert!(matches!(
            Installer::new(""),
            Err(MarketplaceError::InstallationFailed(_))
        ));
    }

    #[test]
    fn new_starts_with_empty_registry() {
        let installer = make_installer();
        assert!(installer.list_installed().is_empty());
    }

    // ── install ───────────────────────────────────────────────────────────────

    #[test]
    fn install_registers_skill() {
        let mut installer = make_installer();
        let skill = installer.install(valid_metadata()).unwrap();
        assert_eq!(skill.metadata.id, "pares/rust-helper");
        assert_eq!(installer.list_installed().len(), 1);
    }

    #[test]
    fn install_sets_install_path_under_install_dir() {
        let mut installer = make_installer();
        let skill = installer.install(valid_metadata()).unwrap();
        assert!(skill.install_path.starts_with("/skills/"));
    }

    #[test]
    fn install_rejects_invalid_metadata() {
        let mut installer = make_installer();
        let mut bad = valid_metadata();
        bad.name = String::new();
        assert!(matches!(
            installer.install(bad),
            Err(MarketplaceError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn install_rejects_duplicate() {
        let mut installer = make_installer();
        installer.install(valid_metadata()).unwrap();
        assert!(matches!(
            installer.install(valid_metadata()),
            Err(MarketplaceError::InstallationFailed(_))
        ));
    }

    // ── uninstall ─────────────────────────────────────────────────────────────

    #[test]
    fn uninstall_removes_skill() {
        let mut installer = make_installer();
        installer.install(valid_metadata()).unwrap();
        installer.uninstall("pares/rust-helper").unwrap();
        assert!(installer.list_installed().is_empty());
    }

    #[test]
    fn uninstall_returns_not_found_for_unknown_skill() {
        let mut installer = make_installer();
        assert!(matches!(
            installer.uninstall("unknown/skill"),
            Err(MarketplaceError::NotFound(_))
        ));
    }

    // ── is_installed ──────────────────────────────────────────────────────────

    #[test]
    fn is_installed_returns_true_after_install() {
        let mut installer = make_installer();
        installer.install(valid_metadata()).unwrap();
        assert!(installer.is_installed("pares/rust-helper"));
    }

    #[test]
    fn is_installed_returns_false_before_install() {
        let installer = make_installer();
        assert!(!installer.is_installed("pares/rust-helper"));
    }

    #[test]
    fn is_installed_returns_false_after_uninstall() {
        let mut installer = make_installer();
        installer.install(valid_metadata()).unwrap();
        installer.uninstall("pares/rust-helper").unwrap();
        assert!(!installer.is_installed("pares/rust-helper"));
    }

    // ── last_used / record_use ────────────────────────────────────────────────

    #[test]
    fn install_sets_last_used_to_none() {
        let mut installer = make_installer();
        let skill = installer.install(valid_metadata()).unwrap();
        assert!(skill.last_used.is_none());
    }

    #[test]
    fn record_use_updates_last_used() {
        let mut installer = make_installer();
        installer.install(valid_metadata()).unwrap();
        installer.record_use("pares/rust-helper").unwrap();
        let skill = installer
            .list_installed()
            .iter()
            .find(|s| s.metadata.id == "pares/rust-helper")
            .unwrap();
        assert!(skill.last_used.is_some());
    }

    #[test]
    fn record_use_returns_not_found_for_unknown_skill() {
        let mut installer = make_installer();
        assert!(matches!(
            installer.record_use("unknown/skill"),
            Err(MarketplaceError::NotFound(_))
        ));
    }
}
