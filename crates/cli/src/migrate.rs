//! Migration orchestration — converts an [`OpenClawInstallation`] into
//! pares-agens data and writes it to an output directory.
//!
//! # Output layout
//! ```text
//! <output>/
//!   memories.json     — [`pares_agens_core::memory::entry::MemoryEntry`] array
//!   channels.json     — channel configuration (Telegram token, etc.)
//!   state.json        — PluresDB state entries (personality files)
//!   procedures.json   — timer procedures converted from cron jobs
//! ```
//!
//! In **dry-run** mode the output directory is never written; only
//! [`MigrationReport`] is produced.

use std::path::Path;

use pares_agens_core::memory::entry::{MemoryCategory, MemoryEntry};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    openclaw::{OpenClawCronJob, OpenClawInstallation},
    MigrateError,
};

// ── Output types ──────────────────────────────────────────────────────────────

/// Channel configuration written to `channels.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    /// Channel adapter name (e.g. `"telegram"`).
    pub channel: String,
    /// Channel-specific configuration values.
    pub settings: serde_json::Map<String, serde_json::Value>,
}

/// A timer procedure written to `procedures.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerProcedure {
    /// Procedure name (taken from the cron job name).
    pub name: String,
    /// Original cron schedule expression.
    pub schedule: String,
    /// Action identifier / script to execute when the timer fires.
    pub action: String,
    /// Whether the timer repeats.
    pub recurring: bool,
}

/// A single PluresDB state entry written to `state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    /// State key (e.g. `"soul"`, `"user"`, `"identity"`).
    pub key: String,
    /// Markdown content.
    pub value: String,
}

// ── Report ────────────────────────────────────────────────────────────────────

/// Summary of a completed (or simulated) migration.
#[derive(Debug, Default)]
pub struct MigrationReport {
    /// Number of memory entries migrated.
    pub memories: usize,
    /// Number of channel configs migrated.
    pub channels: usize,
    /// Number of personality files imported as state entries.
    pub state_entries: usize,
    /// Number of cron jobs converted to timer procedures.
    pub procedures: usize,
    /// Whether this was a dry run (no files were written).
    pub dry_run: bool,
}

impl MigrationReport {
    /// Print a human-readable progress summary to stdout.
    pub fn print(&self) {
        let mode = if self.dry_run { " (dry run)" } else { "" };
        println!("Migration complete{mode}:");
        println!("  memories   : {}", self.memories);
        println!("  channels   : {}", self.channels);
        println!("  state      : {}", self.state_entries);
        println!("  procedures : {}", self.procedures);
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────────

/// Convert an [`OpenClawMemory`] category string to a [`MemoryCategory`].
///
/// Unknown category strings fall back to [`MemoryCategory::Conversation`].
fn parse_category(s: &str) -> MemoryCategory {
    match s {
        "code-pattern" => MemoryCategory::CodePattern,
        "error-fix" => MemoryCategory::ErrorFix,
        "preference" => MemoryCategory::Preference,
        "decision" => MemoryCategory::Decision,
        "procedure" => MemoryCategory::Procedure,
        "ui-interaction" => MemoryCategory::UiInteraction,
        "app-state" => MemoryCategory::AppState,
        "screen-capture" => MemoryCategory::ScreenCapture,
        "automation-trace" => MemoryCategory::AutomationTrace,
        "build-result" => MemoryCategory::BuildResult,
        "demo-checkpoint" => MemoryCategory::DemoCheckpoint,
        _ => MemoryCategory::Conversation,
    }
}

fn cron_to_procedure(cron: &OpenClawCronJob) -> TimerProcedure {
    TimerProcedure {
        name: cron.name.clone(),
        schedule: cron.schedule.clone(),
        action: cron.action.clone(),
        recurring: cron.recurring,
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Run the full migration from `source` to `output`.
///
/// If `dry_run` is `true` the output directory is never touched; the function
/// still performs all conversions and returns a [`MigrationReport`] that
/// reflects what *would* have been written.
///
/// Progress is printed to stdout as each phase completes.
pub fn run(source: &Path, output: &Path, dry_run: bool) -> Result<MigrationReport, MigrateError> {
    // ── Phase 1: Load ──────────────────────────────────────────────────────
    println!("Loading OpenClaw installation from: {}", source.display());
    let inst = OpenClawInstallation::load(source)?;
    println!(
        "  Found {} memories, {} crons, {} personality files",
        inst.memories.len(),
        inst.crons.len(),
        inst.personality_files.len(),
    );

    // ── Phase 2: Convert memories ──────────────────────────────────────────
    println!("Converting memories…");
    let entries: Vec<MemoryEntry> = inst
        .memories
        .iter()
        .map(|m| MemoryEntry {
            id: if m.id.is_empty() {
                Uuid::new_v4().to_string()
            } else {
                m.id.clone()
            },
            content: m.content.clone(),
            category: parse_category(&m.category),
            tags: m.tags.clone(),
            // Embeddings are left empty; they will be computed on first recall.
            embedding: vec![],
            score: 0.0,
            created_at: if m.created_at.is_empty() {
                chrono::Utc::now().to_rfc3339()
            } else {
                m.created_at.clone()
            },
        })
        .collect();
    println!("  {} memories converted", entries.len());

    // ── Phase 3: Convert channel configs ───────────────────────────────────
    println!("Converting channel configs…");
    let mut channels: Vec<ChannelConfig> = Vec::new();
    if let Some(tg) = &inst.config.telegram {
        if !tg.token.is_empty() {
            let mut settings = serde_json::Map::new();
            settings.insert("token".into(), serde_json::Value::String(tg.token.clone()));
            channels.push(ChannelConfig {
                channel: "telegram".into(),
                settings,
            });
        }
    }
    // Preserve any extra top-level config fields as a generic "extra" channel entry.
    if !inst.config.extra.is_empty() {
        channels.push(ChannelConfig {
            channel: "extra".into(),
            settings: inst.config.extra.clone(),
        });
    }
    println!("  {} channel configs converted", channels.len());

    // ── Phase 4: Convert personality files → state ─────────────────────────
    println!("Converting personality files…");
    let state: Vec<StateEntry> = inst
        .personality_files
        .iter()
        .map(|p| StateEntry {
            key: p.key.clone(),
            value: p.content.clone(),
        })
        .collect();
    println!("  {} personality files converted", state.len());

    // ── Phase 5: Convert cron jobs → timer procedures ──────────────────────
    println!("Converting cron jobs…");
    let procedures: Vec<TimerProcedure> = inst.crons.iter().map(cron_to_procedure).collect();
    println!("  {} timer procedures converted", procedures.len());

    let report = MigrationReport {
        memories: entries.len(),
        channels: channels.len(),
        state_entries: state.len(),
        procedures: procedures.len(),
        dry_run,
    };

    // ── Phase 6: Write output ──────────────────────────────────────────────
    if !dry_run {
        write_output(output, &entries, &channels, &state, &procedures)?;
    } else {
        println!("Dry run — no files written.");
    }

    Ok(report)
}

fn write_json_file(path: &Path, json: &str) -> Result<(), MigrateError> {
    println!("Writing {}…", path.display());
    std::fs::write(path, json).map_err(|e| MigrateError::Write {
        path: path.to_path_buf(),
        source: e,
    })
}

fn write_output(
    output: &Path,
    entries: &[MemoryEntry],
    channels: &[ChannelConfig],
    state: &[StateEntry],
    procedures: &[TimerProcedure],
) -> Result<(), MigrateError> {
    std::fs::create_dir_all(output).map_err(|e| MigrateError::Write {
        path: output.to_path_buf(),
        source: e,
    })?;

    write_json_file(
        &output.join("memories.json"),
        &serde_json::to_string_pretty(entries).map_err(MigrateError::Serialize)?,
    )?;
    write_json_file(
        &output.join("channels.json"),
        &serde_json::to_string_pretty(channels).map_err(MigrateError::Serialize)?,
    )?;
    write_json_file(
        &output.join("state.json"),
        &serde_json::to_string_pretty(state).map_err(MigrateError::Serialize)?,
    )?;
    write_json_file(
        &output.join("procedures.json"),
        &serde_json::to_string_pretty(procedures).map_err(MigrateError::Serialize)?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openclaw::{
        OpenClawConfig, OpenClawCronJob, OpenClawInstallation, OpenClawMemory,
        OpenClawTelegramConfig, PersonalityFile,
    };
    use std::io::Write;

    // ── parse_category ──────────────────────────────────────────────────────

    #[test]
    fn parse_known_categories() {
        assert_eq!(parse_category("code-pattern"), MemoryCategory::CodePattern);
        assert_eq!(parse_category("error-fix"), MemoryCategory::ErrorFix);
        assert_eq!(parse_category("preference"), MemoryCategory::Preference);
        assert_eq!(parse_category("decision"), MemoryCategory::Decision);
        assert_eq!(parse_category("procedure"), MemoryCategory::Procedure);
        assert_eq!(parse_category("conversation"), MemoryCategory::Conversation);
    }

    #[test]
    fn parse_unknown_category_falls_back_to_conversation() {
        assert_eq!(parse_category(""), MemoryCategory::Conversation);
        assert_eq!(parse_category("random-tag"), MemoryCategory::Conversation);
    }

    // ── cron_to_procedure ───────────────────────────────────────────────────

    #[test]
    fn cron_to_procedure_preserves_fields() {
        let cron = OpenClawCronJob {
            name: "daily_summary".into(),
            schedule: "0 9 * * *".into(),
            action: "summarise".into(),
            recurring: true,
        };
        let proc = cron_to_procedure(&cron);
        assert_eq!(proc.name, "daily_summary");
        assert_eq!(proc.schedule, "0 9 * * *");
        assert_eq!(proc.action, "summarise");
        assert!(proc.recurring);
    }

    // ── run — dry-run ───────────────────────────────────────────────────────

    fn make_installation() -> OpenClawInstallation {
        OpenClawInstallation {
            memories: vec![
                OpenClawMemory {
                    id: "mem1".into(),
                    content: "Use cargo test to run tests.".into(),
                    category: "code-pattern".into(),
                    tags: vec!["tool:cargo".into()],
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                OpenClawMemory {
                    id: "mem2".into(),
                    content: "I prefer snake_case conventions.".into(),
                    category: "preference".into(),
                    tags: vec![],
                    created_at: "2026-01-02T00:00:00Z".into(),
                },
            ],
            config: OpenClawConfig {
                telegram: Some(OpenClawTelegramConfig {
                    token: "123:ABC".into(),
                }),
                extra: serde_json::Map::new(),
            },
            crons: vec![OpenClawCronJob {
                name: "daily".into(),
                schedule: "0 9 * * *".into(),
                action: "summarise".into(),
                recurring: true,
            }],
            personality_files: vec![
                PersonalityFile {
                    key: "soul".into(),
                    content: "# Soul\nI am helpful.".into(),
                },
                PersonalityFile {
                    key: "identity".into(),
                    content: "# Identity\nPares Agens.".into(),
                },
            ],
        }
    }

    #[test]
    fn dry_run_does_not_write_files() {
        let src_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();

        // Write a minimal OpenClaw installation
        {
            let inst = make_installation();
            let mem_json = serde_json::to_string(&inst.memories).unwrap();
            std::fs::write(src_dir.path().join("memories.json"), mem_json).unwrap();
            let cfg_json = serde_json::to_string(&inst.config).unwrap();
            std::fs::write(src_dir.path().join("config.json"), cfg_json).unwrap();
            let cron_json = serde_json::to_string(&inst.crons).unwrap();
            std::fs::write(src_dir.path().join("crons.json"), cron_json).unwrap();
            let soul_path = src_dir.path().join("SOUL.md");
            let mut f = std::fs::File::create(soul_path).unwrap();
            f.write_all(b"# Soul").unwrap();
        }

        let report = run(src_dir.path(), out_dir.path(), /* dry_run */ true).unwrap();

        assert!(report.dry_run);
        assert_eq!(report.memories, 2);
        assert_eq!(report.channels, 1);
        assert_eq!(report.procedures, 1);
        assert_eq!(report.state_entries, 1); // only SOUL.md written

        // No output files should exist
        assert!(!out_dir.path().join("memories.json").exists());
        assert!(!out_dir.path().join("channels.json").exists());
    }

    #[test]
    fn wet_run_writes_all_files() {
        let src_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();

        let inst = make_installation();
        std::fs::write(
            src_dir.path().join("memories.json"),
            serde_json::to_string(&inst.memories).unwrap(),
        )
        .unwrap();
        std::fs::write(
            src_dir.path().join("config.json"),
            serde_json::to_string(&inst.config).unwrap(),
        )
        .unwrap();
        std::fs::write(
            src_dir.path().join("crons.json"),
            serde_json::to_string(&inst.crons).unwrap(),
        )
        .unwrap();
        {
            let mut f = std::fs::File::create(src_dir.path().join("SOUL.md")).unwrap();
            f.write_all(b"# Soul\nI am helpful.").unwrap();
            let mut f = std::fs::File::create(src_dir.path().join("IDENTITY.md")).unwrap();
            f.write_all(b"# Identity\nPares Agens.").unwrap();
        }

        let report = run(src_dir.path(), out_dir.path(), /* dry_run */ false).unwrap();

        assert!(!report.dry_run);
        assert_eq!(report.memories, 2);
        assert_eq!(report.channels, 1);
        assert_eq!(report.state_entries, 2); // SOUL.md + IDENTITY.md
        assert_eq!(report.procedures, 1);

        // All four output files must exist
        for name in &[
            "memories.json",
            "channels.json",
            "state.json",
            "procedures.json",
        ] {
            assert!(
                out_dir.path().join(name).exists(),
                "{name} should have been written"
            );
        }

        // Validate memories.json content
        let mem_raw = std::fs::read_to_string(out_dir.path().join("memories.json")).unwrap();
        let mems: Vec<MemoryEntry> = serde_json::from_str(&mem_raw).unwrap();
        assert_eq!(mems.len(), 2);
        assert_eq!(mems[0].id, "mem1");
        assert_eq!(mems[0].category, MemoryCategory::CodePattern);
        assert_eq!(mems[1].category, MemoryCategory::Preference);

        // Validate channels.json
        let ch_raw = std::fs::read_to_string(out_dir.path().join("channels.json")).unwrap();
        let chs: Vec<ChannelConfig> = serde_json::from_str(&ch_raw).unwrap();
        assert_eq!(chs.len(), 1);
        assert_eq!(chs[0].channel, "telegram");
        assert_eq!(chs[0].settings["token"], "123:ABC");

        // Validate state.json
        let st_raw = std::fs::read_to_string(out_dir.path().join("state.json")).unwrap();
        let state: Vec<StateEntry> = serde_json::from_str(&st_raw).unwrap();
        assert_eq!(state.len(), 2);

        // Validate procedures.json
        let pr_raw = std::fs::read_to_string(out_dir.path().join("procedures.json")).unwrap();
        let procs: Vec<TimerProcedure> = serde_json::from_str(&pr_raw).unwrap();
        assert_eq!(procs.len(), 1);
        assert_eq!(procs[0].name, "daily");
        assert_eq!(procs[0].schedule, "0 9 * * *");
    }

    #[test]
    fn missing_id_gets_generated_uuid() {
        let src_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            src_dir.path().join("memories.json"),
            r#"[{"id":"","content":"some memory content here","category":"","tags":[],"created_at":""}]"#,
        )
        .unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let report = run(src_dir.path(), out_dir.path(), false).unwrap();
        assert_eq!(report.memories, 1);

        let mem_raw = std::fs::read_to_string(out_dir.path().join("memories.json")).unwrap();
        let mems: Vec<MemoryEntry> = serde_json::from_str(&mem_raw).unwrap();
        assert!(
            !mems[0].id.is_empty(),
            "empty id must be replaced with a UUID"
        );
    }

    #[test]
    fn empty_telegram_token_skipped() {
        let src_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            src_dir.path().join("config.json"),
            r#"{"telegram":{"token":""}}"#,
        )
        .unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let report = run(src_dir.path(), out_dir.path(), false).unwrap();
        assert_eq!(
            report.channels, 0,
            "empty token should not produce a channel config"
        );
    }
}
