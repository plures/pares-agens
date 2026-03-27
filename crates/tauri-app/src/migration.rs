//! Tauri IPC commands for the OpenClaw → Pares Agens migration wizard.
//!
//! Three commands are exposed:
//!
//! | Command              | Description                                               |
//! |----------------------|-----------------------------------------------------------|
//! | `migration_detect`   | Returns the auto-detected OpenClaw path (or `null`).      |
//! | `migration_preview`  | Dry-run: returns a [`MigrationSummary`] without writing.  |
//! | `migration_run`      | Full migration: writes output and returns the summary.    |
//!
//! The frontend invokes these via `__TAURI__.core.invoke(...)`.

use std::path::PathBuf;

use serde::Serialize;

use pares_agens_migrate::{migrate, openclaw};

// ── Serialisable summary ──────────────────────────────────────────────────────

/// JSON-serialisable migration summary returned by `migration_preview` and
/// `migration_run`.
#[derive(Debug, Serialize)]
pub struct MigrationSummary {
    /// Number of memory entries migrated (or that would be migrated).
    pub memories: usize,
    /// Number of channel configs migrated (or that would be migrated).
    pub channels: usize,
    /// Number of personality / state entries migrated.
    pub state_entries: usize,
    /// Number of cron jobs converted to timer procedures.
    pub procedures: usize,
    /// Whether this was a dry run (no files were written).
    pub dry_run: bool,
}

impl From<migrate::MigrationReport> for MigrationSummary {
    fn from(r: migrate::MigrationReport) -> Self {
        Self {
            memories: r.memories,
            channels: r.channels,
            state_entries: r.state_entries,
            procedures: r.procedures,
            dry_run: r.dry_run,
        }
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Return the auto-detected OpenClaw installation path, or `null` when no
/// installation is found under the default location (`~/.openclaw`).
///
/// The frontend calls this to decide whether to show the migration wizard
/// on first launch.
#[tauri::command]
pub fn migration_detect() -> Option<String> {
    openclaw::auto_detect().map(|p| p.to_string_lossy().into_owned())
}

/// Perform a dry-run migration from `source` and return a summary of what
/// *would* be imported, without writing any output files.
///
/// `source` defaults to the auto-detected path when not supplied.
#[tauri::command]
pub fn migration_preview(source: Option<String>) -> Result<MigrationSummary, String> {
    let path = resolve_source(source)?;
    migrate::run(&path, std::path::Path::new(""), true)
        .map(MigrationSummary::from)
        .map_err(|e| e.to_string())
}

/// Run the full migration from `source` into `output`.
///
/// `source` defaults to the auto-detected path when not supplied.
/// `output` defaults to `migration` in the current working directory.
#[tauri::command]
pub fn migration_run(
    source: Option<String>,
    output: Option<String>,
) -> Result<MigrationSummary, String> {
    let src = resolve_source(source)?;
    let out = output
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("migration"));
    migrate::run(&src, &out, false)
        .map(MigrationSummary::from)
        .map_err(|e| e.to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn resolve_source(source: Option<String>) -> Result<PathBuf, String> {
    match source.map(PathBuf::from).or_else(openclaw::auto_detect) {
        Some(p) => Ok(p),
        None => Err("No OpenClaw installation found. \
             Pass a `source` path or install OpenClaw first."
            .into()),
    }
}
