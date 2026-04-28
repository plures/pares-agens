//! OpenClaw installation reader.
//!
//! Defines the data structures of an OpenClaw installation and provides
//! helpers to load them from an on-disk directory (`~/.openclaw` by default).
//!
//! # Directory layout expected
//! ```text
//! <root>/
//!   memories.json        — PluresLM memory entries (array of [`OpenClawMemory`])
//!   config.json          — channel configs (see [`OpenClawConfig`])
//!   crons.json           — scheduled jobs (array of [`OpenClawCronJob`])
//!   SOUL.md              — personality / soul definition (optional)
//!   USER.md              — user profile (optional)
//!   IDENTITY.md          — agent identity (optional)
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::MigrateError;

// ── Memory ────────────────────────────────────────────────────────────────────

/// A single memory entry as stored in an OpenClaw installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawMemory {
    /// Unique identifier (UUID v4 string).
    pub id: String,
    /// The textual content of the memory.
    pub content: String,
    /// Semantic category label (e.g. `"conversation"`, `"code-pattern"`).
    #[serde(default)]
    pub category: String,
    /// Arbitrary keyword tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// ISO 8601 creation timestamp.
    #[serde(default)]
    pub created_at: String,
}

// ── Channel config ─────────────────────────────────────────────────────────────

/// Telegram-specific channel configuration from OpenClaw.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawTelegramConfig {
    /// Telegram bot token supplied by BotFather.
    pub token: String,
}

/// Top-level channel configuration block stored in `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawConfig {
    /// Telegram channel settings.
    #[serde(default)]
    pub telegram: Option<OpenClawTelegramConfig>,
    /// Additional arbitrary key/value settings.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

// ── Cron jobs ─────────────────────────────────────────────────────────────────

/// A scheduled (cron) job from OpenClaw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawCronJob {
    /// Human-readable name used as the timer procedure name.
    pub name: String,
    /// Cron expression (e.g. `"0 9 * * *"` for daily at 09:00).
    pub schedule: String,
    /// Action identifier or script to run.
    pub action: String,
    /// Whether the job repeats (defaults to `true`).
    #[serde(default = "default_true")]
    pub recurring: bool,
}

fn default_true() -> bool {
    true
}

// ── Personality files ─────────────────────────────────────────────────────────

/// A personality / identity file loaded from the OpenClaw directory.
#[derive(Debug, Clone)]
pub struct PersonalityFile {
    /// State key to store this content under (e.g. `"soul"`, `"user"`, `"identity"`).
    pub key: String,
    /// Markdown content of the file.
    pub content: String,
}

// ── Top-level installation reader ─────────────────────────────────────────────

/// Represents the contents of an OpenClaw installation directory.
#[derive(Debug, Default)]
pub struct OpenClawInstallation {
    /// All loaded memory entries (`memories.json`).
    pub memories: Vec<OpenClawMemory>,
    /// Channel configuration (`config.json`).
    pub config: OpenClawConfig,
    /// Scheduled jobs (`crons.json`).
    pub crons: Vec<OpenClawCronJob>,
    /// Personality/identity files (`SOUL.md`, `USER.md`, `IDENTITY.md`).
    pub personality_files: Vec<PersonalityFile>,
}

/// Return the default OpenClaw installation directory (`~/.openclaw`), or
/// `None` if the directory does not exist.
///
/// The home directory is resolved from the `HOME` environment variable on
/// Unix and `USERPROFILE` on Windows.
pub fn auto_detect() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    openclaw_dir_under(std::path::Path::new(&home))
}

/// Return the `.openclaw` subdirectory under `home` if it exists, else `None`.
///
/// Extracted so tests can call it without modifying global environment state.
fn openclaw_dir_under(home: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = home.join(".openclaw");
    if path.is_dir() {
        Some(path)
    } else {
        None
    }
}

impl OpenClawInstallation {
    /// Load an OpenClaw installation from `root`.
    ///
    /// Files that are missing are silently skipped; only genuine parse errors
    /// cause an `Err` return.
    pub fn load(root: &Path) -> Result<Self, MigrateError> {
        let mut inst = Self::default();

        // ── memories.json ──────────────────────────────────────────────────
        let memories_path = root.join("memories.json");
        if memories_path.exists() {
            let raw = std::fs::read_to_string(&memories_path).map_err(|e| MigrateError::Read {
                path: memories_path.clone(),
                source: e,
            })?;
            inst.memories = serde_json::from_str(&raw).map_err(|e| MigrateError::Parse {
                path: memories_path,
                source: e,
            })?;
        }

        // ── config.json ────────────────────────────────────────────────────
        let config_path = root.join("config.json");
        if config_path.exists() {
            let raw = std::fs::read_to_string(&config_path).map_err(|e| MigrateError::Read {
                path: config_path.clone(),
                source: e,
            })?;
            inst.config = serde_json::from_str(&raw).map_err(|e| MigrateError::Parse {
                path: config_path,
                source: e,
            })?;
        }

        // ── crons.json ─────────────────────────────────────────────────────
        let crons_path = root.join("crons.json");
        if crons_path.exists() {
            let raw = std::fs::read_to_string(&crons_path).map_err(|e| MigrateError::Read {
                path: crons_path.clone(),
                source: e,
            })?;
            inst.crons = serde_json::from_str(&raw).map_err(|e| MigrateError::Parse {
                path: crons_path,
                source: e,
            })?;
        }

        // ── personality files ──────────────────────────────────────────────
        for (filename, key) in &[
            ("SOUL.md", "soul"),
            ("USER.md", "user"),
            ("IDENTITY.md", "identity"),
        ] {
            let file_path = root.join(filename);
            if file_path.exists() {
                let content =
                    std::fs::read_to_string(&file_path).map_err(|e| MigrateError::Read {
                        path: file_path,
                        source: e,
                    })?;
                inst.personality_files.push(PersonalityFile {
                    key: (*key).to_string(),
                    content,
                });
            }
        }

        Ok(inst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &str) {
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn load_empty_dir_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.memories.is_empty());
        assert!(inst.crons.is_empty());
        assert!(inst.personality_files.is_empty());
        assert!(inst.config.telegram.is_none());
    }

    #[test]
    fn load_memories_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir,
            "memories.json",
            r#"[{"id":"abc","content":"hello world","category":"conversation","tags":[],"created_at":"2026-01-01T00:00:00Z"}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.memories.len(), 1);
        assert_eq!(inst.memories[0].id, "abc");
        assert_eq!(inst.memories[0].content, "hello world");
        assert_eq!(inst.memories[0].category, "conversation");
    }

    #[test]
    fn load_config_json_with_telegram() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir, "config.json", r#"{"telegram":{"token":"123:ABC"}}"#);
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.config.telegram.as_ref().unwrap().token, "123:ABC");
    }

    #[test]
    fn load_crons_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            &dir,
            "crons.json",
            r#"[{"name":"daily","schedule":"0 9 * * *","action":"summarise","recurring":true}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.crons.len(), 1);
        assert_eq!(inst.crons[0].name, "daily");
        assert_eq!(inst.crons[0].schedule, "0 9 * * *");
        assert!(inst.crons[0].recurring);
    }

    #[test]
    fn load_personality_files() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir, "SOUL.md", "# Soul\nI am an AI assistant.");
        write_file(&dir, "USER.md", "# User\nName: Alice");
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.personality_files.len(), 2);
        let keys: Vec<&str> = inst
            .personality_files
            .iter()
            .map(|p| p.key.as_str())
            .collect();
        assert!(keys.contains(&"soul"));
        assert!(keys.contains(&"user"));
    }

    #[test]
    fn load_invalid_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        write_file(&dir, "memories.json", "not json");
        let result = OpenClawInstallation::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn auto_detect_returns_none_for_nonexistent_dir() {
        // Use a temp dir with no .openclaw sub-directory.
        let dir = tempfile::tempdir().unwrap();
        assert!(openclaw_dir_under(dir.path()).is_none());
    }

    #[test]
    fn auto_detect_returns_path_when_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        let openclaw = dir.path().join(".openclaw");
        std::fs::create_dir(&openclaw).unwrap();
        assert_eq!(openclaw_dir_under(dir.path()), Some(openclaw));
    }

    #[test]
    fn cron_recurring_defaults_to_true() {
        let dir = tempfile::tempdir().unwrap();
        // No "recurring" field — should default to true
        write_file(
            &dir,
            "crons.json",
            r#"[{"name":"weekly","schedule":"0 9 * * 1","action":"report"}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.crons[0].recurring);
    }
}
