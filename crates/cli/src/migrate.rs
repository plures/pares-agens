//! Migration orchestration — converts an [`OpenClawInstallation`] into
//! pares-agens data and writes it to an output directory.
//!
//! # Output layout
//! ```text
//! <output>/
//!   memories.json     — [`pares_agens_core::memory::entry::MemoryEntry`] array
//!                       (imported from `memory/main.sqlite` chunks + any legacy
//!                       `memories.json`)
//!   channels.json     — channel configuration (Telegram bot token, etc.)
//!   state.json        — PluresDB state entries (personality files)
//!   procedures.json   — timer procedures converted from legacy cron jobs
//! ```
//!
//! In **dry-run** mode the output directory is never written; only
//! [`MigrationReport`] is produced. The report accounts for every source that
//! was found — channels, personality documents, and the memory corpus — so an
//! empty or partial installation is *visible*, never silently dropped.

use std::path::Path;

use pares_agens_core::memory::entry::{MemoryCategory, MemoryEntry};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    openclaw::{MemorySource, OpenClawChunk, OpenClawCronJob, OpenClawInstallation},
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
///
/// Every source discovered under the OpenClaw root is accounted for here so
/// that an empty or partial installation is *visible* in the output rather than
/// silently producing a hollow report.
#[derive(Debug, Default)]
pub struct MigrationReport {
    /// Total number of memory entries migrated (SQLite chunks + legacy export).
    pub memories: usize,
    /// Number of memory entries imported from `memory/main.sqlite` chunks.
    pub memory_chunks: usize,
    /// Number of memory entries imported from a legacy `memories.json`.
    pub legacy_memories: usize,
    /// Human-readable status of the memory sources (SQLite + pluresLM-store).
    pub memory_status: String,
    /// Number of channel configs migrated.
    pub channels: usize,
    /// Names of the channels that were migrated (e.g. `["telegram"]`).
    pub channel_names: Vec<String>,
    /// Number of personality files imported as state entries.
    pub state_entries: usize,
    /// State keys of the personality files imported (e.g. `["soul", "user"]`).
    pub personality_keys: Vec<String>,
    /// Number of cron jobs converted to timer procedures.
    pub procedures: usize,
    /// Whether this was a dry run (no files were written).
    pub dry_run: bool,
}

impl MigrationReport {
    /// Print a human-readable summary to stdout.
    pub fn print(&self) {
        let mode = if self.dry_run { " (dry run)" } else { "" };
        println!("Migration complete{mode}:");
        println!(
            "  memories   : {} (sqlite chunks: {}, legacy: {})",
            self.memories, self.memory_chunks, self.legacy_memories
        );
        println!("  memory src : {}", self.memory_status);
        if self.channel_names.is_empty() {
            println!("  channels   : 0");
        } else {
            println!(
                "  channels   : {} [{}]",
                self.channels,
                self.channel_names.join(", ")
            );
        }
        if self.personality_keys.is_empty() {
            println!("  state      : 0");
        } else {
            println!(
                "  state      : {} [{}]",
                self.state_entries,
                self.personality_keys.join(", ")
            );
        }
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

/// Convert a raw `chunks`-table row from OpenClaw's `main.sqlite` into a
/// pares-agens [`MemoryEntry`].
///
/// The chunk's `source` document `path` is preserved as a `source:<path>` tag
/// so provenance survives the import. Embeddings are intentionally left empty;
/// pares-agens recomputes them on first recall.
fn chunk_to_entry(chunk: &OpenClawChunk) -> MemoryEntry {
    let mut tags = Vec::new();
    if !chunk.path.is_empty() {
        tags.push(format!("source:{}", chunk.path));
    }
    if !chunk.source.is_empty() && chunk.source != chunk.path {
        tags.push(format!("origin:{}", chunk.source));
    }
    let created_at = if chunk.updated_at > 0 {
        chrono::DateTime::from_timestamp(chunk.updated_at, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
    } else {
        chrono::Utc::now().to_rfc3339()
    };
    MemoryEntry {
        id: if chunk.id.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            chunk.id.clone()
        },
        content: chunk.text.clone(),
        // Imported chunks are documentation-derived; classify as Conversation
        // (the neutral default) since OpenClaw chunks carry no agens category.
        category: MemoryCategory::Conversation,
        tags,
        embedding: vec![],
        score: 0.0,
        created_at,
    }
}

/// Build a one-line, honest description of the memory sources found.
///
/// This never reports "0" when a corpus exists on disk: if the SQLite file is
/// present its chunk count is stated, and the separate `pluresLM-store/` is
/// reported as present-but-not-yet-imported (import tracked as a follow-up).
fn memory_status_line(src: &MemorySource) -> String {
    let mut parts = Vec::new();
    match (&src.sqlite_path, src.sqlite_size_bytes) {
        (Some(_), size) => parts.push(format!(
            "main.sqlite: {} chunks imported ({})",
            src.chunks.len(),
            human_size(size.unwrap_or(0))
        )),
        _ => parts.push("main.sqlite: not found".to_string()),
    }
    if src.has_plureslm_store() {
        parts.push(format!(
            "pluresLM-store/: present ({}), import pending (see praxis-gap)",
            human_size(src.plureslm_store_size_bytes.unwrap_or(0))
        ));
    } else {
        parts.push("pluresLM-store/: not found".to_string());
    }
    parts.join("; ")
}

/// Format a byte count as a short human-readable size (KiB/MiB/GiB).
fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else {
        format!("{:.1} GiB", b / (KIB * KIB * KIB))
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
    let memory_status = memory_status_line(&inst.memory_source);
    println!(
        "  Config source: {}",
        if inst.config_source_openclaw_json {
            "openclaw.json"
        } else if inst.config.telegram.is_some() || !inst.config.extra.is_empty() {
            "config.json (legacy)"
        } else {
            "none found"
        }
    );
    println!(
        "  Found {} sqlite chunks, {} legacy memories, {} crons, {} personality files",
        inst.memory_source.chunks.len(),
        inst.memories.len(),
        inst.crons.len(),
        inst.personality_files.len(),
    );
    println!("  Memory source: {memory_status}");

    // ── Phase 2: Convert memories ──────────────────────────────────────────
    println!("Converting memories…");
    let chunk_entries: Vec<MemoryEntry> = inst
        .memory_source
        .chunks
        .iter()
        .map(chunk_to_entry)
        .collect();
    let legacy_entries: Vec<MemoryEntry> = inst
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
    let chunk_count = chunk_entries.len();
    let legacy_count = legacy_entries.len();
    let mut entries = chunk_entries;
    entries.extend(legacy_entries);
    println!(
        "  {} memories converted ({} from sqlite chunks, {} legacy)",
        entries.len(),
        chunk_count,
        legacy_count
    );

    // ── Phase 3: Convert channel configs ───────────────────────────────────
    println!("Converting channel configs…");
    let mut channels: Vec<ChannelConfig> = Vec::new();
    if let Some(tg) = &inst.config.telegram {
        let token = tg.resolved_token();
        if !token.is_empty() {
            let mut settings = serde_json::Map::new();
            settings.insert("token".into(), serde_json::Value::String(token.to_string()));
            settings.insert("enabled".into(), serde_json::Value::Bool(tg.enabled));
            let allow_from = tg.allow_from_strings();
            if !allow_from.is_empty() {
                settings.insert(
                    "allowFrom".into(),
                    serde_json::Value::Array(
                        allow_from
                            .into_iter()
                            .map(serde_json::Value::String)
                            .collect(),
                    ),
                );
            }
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
    let channel_names: Vec<String> = channels.iter().map(|c| c.channel.clone()).collect();
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
    let personality_keys: Vec<String> = state.iter().map(|s| s.key.clone()).collect();
    println!("  {} personality files converted", state.len());

    // ── Phase 5: Convert cron jobs → timer procedures ──────────────────────
    println!("Converting cron jobs…");
    let procedures: Vec<TimerProcedure> = inst.crons.iter().map(cron_to_procedure).collect();
    println!("  {} timer procedures converted", procedures.len());

    let report = MigrationReport {
        memories: entries.len(),
        memory_chunks: chunk_count,
        legacy_memories: legacy_count,
        memory_status,
        channels: channels.len(),
        channel_names,
        state_entries: state.len(),
        personality_keys,
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
                    enabled: true,
                    bot_token: "123:ABC".into(),
                    ..Default::default()
                }),
                extra: serde_json::Map::new(),
            },
            config_source_openclaw_json: false,
            memory_source: crate::openclaw::MemorySource::default(),
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
                    source_path: std::path::PathBuf::from("workspace/SOUL.md"),
                },
                PersonalityFile {
                    key: "identity".into(),
                    content: "# Identity\nPares Agens.".into(),
                    source_path: std::path::PathBuf::from("workspace/IDENTITY.md"),
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

    // ── chunk_to_entry provenance ─────────────────────────────────────

    #[test]
    fn chunk_to_entry_preserves_provenance_and_id() {
        let chunk = crate::openclaw::OpenClawChunk {
            id: "c42".into(),
            path: "AGENTS.md".into(),
            source: "file".into(),
            text: "the chunk body".into(),
            updated_at: 1_700_000_000,
        };
        let entry = chunk_to_entry(&chunk);
        assert_eq!(entry.id, "c42");
        assert_eq!(entry.content, "the chunk body");
        assert!(entry.tags.contains(&"source:AGENTS.md".to_string()));
        assert!(entry.tags.contains(&"origin:file".to_string()));
        assert!(entry.embedding.is_empty());
    }

    #[test]
    fn chunk_with_empty_id_gets_uuid() {
        let chunk = crate::openclaw::OpenClawChunk {
            id: String::new(),
            path: "x".into(),
            source: String::new(),
            text: "body".into(),
            updated_at: 0,
        };
        let entry = chunk_to_entry(&chunk);
        assert!(!entry.id.is_empty(), "empty chunk id must become a UUID");
    }

    // ── Real end-to-end: sqlite chunks import + honest memory status ────────

    #[test]
    fn run_imports_sqlite_chunks_and_reports_memory_status() {
        let src_dir = tempfile::tempdir().unwrap();
        let out_dir = tempfile::tempdir().unwrap();

        // Build a real chunks DB at memory/main.sqlite.
        let mem_dir = src_dir.path().join("memory");
        std::fs::create_dir_all(&mem_dir).unwrap();
        let conn = rusqlite::Connection::open(mem_dir.join("main.sqlite")).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (id TEXT PRIMARY KEY, path TEXT, source TEXT, \
             start_line INTEGER, end_line INTEGER, hash TEXT, model TEXT, \
             text TEXT, embedding TEXT, updated_at INTEGER)",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chunks (id, path, source, text, updated_at) VALUES \
             ('a','AGENTS.md','file','first',1700000000), \
             ('b','SOUL.md','file','second',1700000100)",
            [],
        )
        .unwrap();
        drop(conn);

        // Also a real openclaw.json with a telegram channel + workspace personality.
        std::fs::write(
            src_dir.path().join("openclaw.json"),
            r#"{"channels":{"telegram":{"enabled":true,"botToken":"999:TOK","allowFrom":["111"]}}}"#,
        )
        .unwrap();
        let ws = src_dir.path().join("workspace");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("SOUL.md"), "# Soul").unwrap();

        let report = run(src_dir.path(), out_dir.path(), true).unwrap();

        assert!(report.dry_run);
        assert_eq!(report.memory_chunks, 2, "both sqlite chunks imported");
        assert_eq!(report.legacy_memories, 0);
        assert_eq!(report.memories, 2);
        assert_eq!(report.channels, 1);
        assert_eq!(report.channel_names, vec!["telegram".to_string()]);
        assert_eq!(report.state_entries, 1);
        assert_eq!(report.personality_keys, vec!["soul".to_string()]);
        // Honest, non-zero memory status mentioning the imported chunks.
        assert!(
            report.memory_status.contains("2 chunks imported"),
            "memory_status should report chunk count, got: {}",
            report.memory_status
        );
    }
}
