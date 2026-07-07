//! OpenClaw installation reader.
//!
//! Defines the data structures of an OpenClaw installation and provides
//! helpers to load them from an on-disk directory (`~/.openclaw` by default).
//!
//! # Real directory layout (as produced by current OpenClaw)
//! ```text
//! <root>/                         (the `.openclaw` directory)
//!   openclaw.json                 — top-level config; channels at `.channels.telegram`
//!   memory/
//!     main.sqlite                 — chunk index (table `chunks`) — the memory corpus
//!   pluresLM-store/               — separate PluresLM corpus (db, blobs/, snap.*, conf)
//!   workspace/
//!     SOUL.md, USER.md, IDENTITY.md, MEMORY.md, AGENTS.md
//!                                 — personality / identity documents
//! ```
//!
//! # Legacy layout (still read for back-compat)
//! ```text
//! <root>/
//!   memories.json                 — flat PluresLM memory export (array of [`OpenClawMemory`])
//!   config.json                   — channel configs (see [`OpenClawConfig`])
//!   crons.json                    — scheduled jobs (array of [`OpenClawCronJob`])
//!   SOUL.md / USER.md / IDENTITY.md — personality files at the root
//! ```
//!
//! Missing sources are skipped without error; only genuine read/parse failures
//! (e.g. corrupt JSON, an unreadable SQLite file) surface as [`MigrateError`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::MigrateError;

/// Personality document basenames imported from `workspace/` (or the root, for
/// back-compat), mapped to the state key they are stored under.
const PERSONALITY_FILES: &[(&str, &str)] = &[
    ("SOUL.md", "soul"),
    ("USER.md", "user"),
    ("IDENTITY.md", "identity"),
    ("MEMORY.md", "memory"),
    ("AGENTS.md", "agents"),
];

// ── Memory (legacy flat export) ────────────────────────────────────────────────

/// A single memory entry as stored in a *legacy* OpenClaw `memories.json` export.
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

// ── Memory (real chunk index: `memory/main.sqlite`) ─────────────────────────────

/// A single row from the `chunks` table of OpenClaw's `memory/main.sqlite`.
///
/// This is the real memory corpus: source documents are split into chunks,
/// each with its provenance (`path`, `source`) and text. Embeddings are stored
/// separately (`embedding` column / `chunks_vec` virtual table) and are **not**
/// carried here — pares-agens recomputes embeddings on first recall.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenClawChunk {
    /// Chunk identifier (`chunks.id`).
    pub id: String,
    /// Source document path this chunk was extracted from (`chunks.path`).
    pub path: String,
    /// Origin label for the chunk (`chunks.source`, e.g. a file or note kind).
    pub source: String,
    /// The chunk's textual content (`chunks.text`).
    pub text: String,
    /// Last-updated Unix timestamp in seconds (`chunks.updated_at`).
    pub updated_at: i64,
}

/// Status of the OpenClaw memory sources discovered under `root`.
///
/// Reading the SQLite `chunks` table is the real memory import; the separate
/// `pluresLM-store/` directory is detected and reported but not yet imported
/// (tracked as a follow-up — see the migration report).
#[derive(Debug, Default, Clone)]
pub struct MemorySource {
    /// Path to `memory/main.sqlite` if it exists.
    pub sqlite_path: Option<PathBuf>,
    /// Size of `memory/main.sqlite` in bytes, if present.
    pub sqlite_size_bytes: Option<u64>,
    /// Chunks read from the `chunks` table of `memory/main.sqlite`.
    pub chunks: Vec<OpenClawChunk>,
    /// Path to the `pluresLM-store/` directory if it exists.
    pub plureslm_store_path: Option<PathBuf>,
    /// Total size of the `pluresLM-store/` directory contents in bytes, if present.
    pub plureslm_store_size_bytes: Option<u64>,
}

impl MemorySource {
    /// Whether `memory/main.sqlite` was found on disk.
    pub fn has_sqlite(&self) -> bool {
        self.sqlite_path.is_some()
    }

    /// Whether a separate `pluresLM-store/` directory was found on disk.
    pub fn has_plureslm_store(&self) -> bool {
        self.plureslm_store_path.is_some()
    }
}

// ── Channel config ─────────────────────────────────────────────────────────────

/// Telegram-specific channel configuration.
///
/// Mirrors the real `openclaw.json` shape at `.channels.telegram`
/// (`enabled`, `botToken`, `allowFrom`, `groups`, …) while remaining
/// backward-compatible with the legacy `config.json` shape, which used a bare
/// `token` field.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawTelegramConfig {
    /// Whether the Telegram channel is enabled.
    #[serde(default)]
    pub enabled: bool,
    /// Telegram bot token supplied by BotFather (real key: `botToken`).
    #[serde(default, rename = "botToken", alias = "bot_token")]
    pub bot_token: String,
    /// Legacy bot-token field from old `config.json` exports.
    ///
    /// Kept for back-compat; [`OpenClawTelegramConfig::resolved_token`]
    /// prefers `bot_token` and falls back to this.
    #[serde(default)]
    pub token: String,
    /// Chat/user IDs permitted to DM the agent (real key: `allowFrom`).
    ///
    /// Stored as raw JSON values because real installations use **numeric**
    /// chat IDs (e.g. `8573852722`), while other exports use strings. Use
    /// [`OpenClawTelegramConfig::allow_from_strings`] to get normalized string
    /// IDs.
    #[serde(default, rename = "allowFrom", alias = "allow_from")]
    pub allow_from: Vec<serde_json::Value>,
    /// Group chats the agent participates in.
    ///
    /// Kept as a raw JSON value because real installations store this as a
    /// **map** keyed by group id (`{"-5240622952": {"enabled": true}}`), while
    /// other exports may use an array. Use
    /// [`OpenClawTelegramConfig::group_count`] for a shape-agnostic count.
    #[serde(default)]
    pub groups: serde_json::Value,
}

impl OpenClawTelegramConfig {
    /// The effective bot token, preferring the real `botToken` and falling
    /// back to the legacy `token` field. Empty if neither is set.
    pub fn resolved_token(&self) -> &str {
        if !self.bot_token.is_empty() {
            &self.bot_token
        } else {
            &self.token
        }
    }

    /// The `allowFrom` IDs normalized to strings, coercing numeric IDs
    /// (e.g. `8573852722`) into their decimal string form so downstream
    /// consumers see a uniform `Vec<String>`.
    pub fn allow_from_strings(&self) -> Vec<String> {
        self.allow_from
            .iter()
            .map(|v| match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect()
    }

    /// Number of configured group chats, handling both the real map shape
    /// (`{id: {..}}`) and an array shape. Returns `0` for null/other.
    pub fn group_count(&self) -> usize {
        match &self.groups {
            serde_json::Value::Object(m) => m.len(),
            serde_json::Value::Array(a) => a.len(),
            _ => 0,
        }
    }
}

/// Top-level channel configuration block.
///
/// Populated either from `openclaw.json`'s `.channels` object (real layout) or
/// from a legacy top-level `config.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OpenClawConfig {
    /// Telegram channel settings.
    #[serde(default)]
    pub telegram: Option<OpenClawTelegramConfig>,
    /// Additional arbitrary channel/config fields preserved verbatim.
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Minimal view of the real `openclaw.json` file — only the `channels` block is
/// extracted; everything else is intentionally ignored during migration.
#[derive(Debug, Clone, Deserialize, Default)]
struct OpenClawJson {
    #[serde(default)]
    channels: OpenClawConfig,
}

// ── Cron jobs ─────────────────────────────────────────────────────────────────

/// A scheduled (cron) job from a *legacy* OpenClaw `crons.json` export.
///
/// The current OpenClaw stores crons in its cron subsystem, not a flat file, so
/// this is only populated when a legacy `crons.json` is present.
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
    /// Absolute path the file was read from (for reporting/provenance).
    pub source_path: PathBuf,
}

// ── Top-level installation reader ─────────────────────────────────────────────

/// Represents the contents of an OpenClaw installation directory.
#[derive(Debug, Default)]
pub struct OpenClawInstallation {
    /// Legacy flat memory entries (`memories.json`), if present.
    pub memories: Vec<OpenClawMemory>,
    /// Memory source status: SQLite `chunks` + `pluresLM-store/` detection.
    pub memory_source: MemorySource,
    /// Channel configuration (`openclaw.json` `.channels`, or legacy `config.json`).
    pub config: OpenClawConfig,
    /// Whether channel config came from the real `openclaw.json` (vs legacy).
    pub config_source_openclaw_json: bool,
    /// Scheduled jobs (legacy `crons.json`).
    pub crons: Vec<OpenClawCronJob>,
    /// Personality/identity files (`workspace/*.md`, or the root for back-compat).
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

/// Read and JSON-parse a file into `T`, mapping I/O and parse failures to
/// [`MigrateError`].
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, MigrateError> {
    let raw = std::fs::read_to_string(path).map_err(|e| MigrateError::Read {
        path: path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_str(&raw).map_err(|e| MigrateError::Parse {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Sum the sizes of the immediate entries of a directory (one level deep, plus
/// nested files) in bytes. Best-effort: unreadable entries are skipped.
fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

impl OpenClawInstallation {
    /// Load an OpenClaw installation from `root`.
    ///
    /// Reads the real layout (`openclaw.json`, `memory/main.sqlite`,
    /// `workspace/*.md`, `pluresLM-store/`) and remains backward-compatible
    /// with the legacy flat layout (`memories.json`, `config.json`,
    /// `crons.json`, root-level `*.md`).
    ///
    /// Missing sources are skipped; only genuine read/parse failures return an
    /// `Err`.
    pub fn load(root: &Path) -> Result<Self, MigrateError> {
        let mut inst = Self::default();

        inst.config = Self::load_config(root, &mut inst.config_source_openclaw_json)?;
        inst.memories = Self::load_legacy_memories(root)?;
        inst.memory_source = Self::load_memory_source(root)?;
        inst.crons = Self::load_legacy_crons(root)?;
        inst.personality_files = Self::load_personality_files(root)?;

        Ok(inst)
    }

    /// Load channel config from the real `openclaw.json` (`.channels`), falling
    /// back to a legacy top-level `config.json`. Sets `from_openclaw_json` to
    /// `true` when the real file supplied the config.
    fn load_config(root: &Path, from_openclaw_json: &mut bool) -> Result<OpenClawConfig, MigrateError> {
        let openclaw_json = root.join("openclaw.json");
        if openclaw_json.exists() {
            let parsed: OpenClawJson = read_json(&openclaw_json)?;
            *from_openclaw_json = true;
            return Ok(parsed.channels);
        }

        let legacy = root.join("config.json");
        if legacy.exists() {
            let cfg: OpenClawConfig = read_json(&legacy)?;
            return Ok(cfg);
        }

        Ok(OpenClawConfig::default())
    }

    /// Load a legacy flat `memories.json` export if present.
    fn load_legacy_memories(root: &Path) -> Result<Vec<OpenClawMemory>, MigrateError> {
        let path = root.join("memories.json");
        if path.exists() {
            read_json(&path)
        } else {
            Ok(Vec::new())
        }
    }

    /// Detect and read the memory sources: the `chunks` table of
    /// `memory/main.sqlite` (real import) and the presence/size of the separate
    /// `pluresLM-store/` directory (reported, import pending).
    fn load_memory_source(root: &Path) -> Result<MemorySource, MigrateError> {
        let mut src = MemorySource::default();

        let sqlite = root.join("memory").join("main.sqlite");
        if sqlite.is_file() {
            if let Ok(meta) = std::fs::metadata(&sqlite) {
                src.sqlite_size_bytes = Some(meta.len());
            }
            src.chunks = read_chunks(&sqlite)?;
            src.sqlite_path = Some(sqlite);
        }

        let plureslm = root.join("pluresLM-store");
        if plureslm.is_dir() {
            src.plureslm_store_size_bytes = Some(dir_size_bytes(&plureslm));
            src.plureslm_store_path = Some(plureslm);
        }

        Ok(src)
    }

    /// Load a legacy `crons.json` export if present.
    fn load_legacy_crons(root: &Path) -> Result<Vec<OpenClawCronJob>, MigrateError> {
        let path = root.join("crons.json");
        if path.exists() {
            read_json(&path)
        } else {
            Ok(Vec::new())
        }
    }

    /// Load personality documents, preferring `workspace/*.md` (real layout) and
    /// falling back to the same basename at the root (legacy layout). Each
    /// document is only imported once, from whichever location is found first.
    fn load_personality_files(root: &Path) -> Result<Vec<PersonalityFile>, MigrateError> {
        let workspace = root.join("workspace");
        let mut files = Vec::new();

        for (filename, key) in PERSONALITY_FILES {
            // Prefer the real workspace/ location, fall back to the root.
            let candidate = {
                let ws = workspace.join(filename);
                if ws.is_file() {
                    Some(ws)
                } else {
                    let at_root = root.join(filename);
                    if at_root.is_file() {
                        Some(at_root)
                    } else {
                        None
                    }
                }
            };

            if let Some(path) = candidate {
                let content = std::fs::read_to_string(&path).map_err(|e| MigrateError::Read {
                    path: path.clone(),
                    source: e,
                })?;
                files.push(PersonalityFile {
                    key: (*key).to_string(),
                    content,
                    source_path: path,
                });
            }
        }

        Ok(files)
    }
}

/// Read the `chunks` table from an OpenClaw `main.sqlite` file into
/// [`OpenClawChunk`] rows.
///
/// Opens the database read-only. A missing `chunks` table (an unexpected schema)
/// is a genuine parse-level failure and surfaces as [`MigrateError::Sqlite`],
/// rather than silently returning zero rows — that distinction is what prevents
/// the "empty report" bug from recurring.
pub fn read_chunks(sqlite_path: &Path) -> Result<Vec<OpenClawChunk>, MigrateError> {
    use rusqlite::OpenFlags;

    let conn = rusqlite::Connection::open_with_flags(
        sqlite_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| MigrateError::Sqlite {
        path: sqlite_path.to_path_buf(),
        source: e,
    })?;

    let mut stmt = conn
        .prepare("SELECT id, path, source, text, updated_at FROM chunks")
        .map_err(|e| MigrateError::Sqlite {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?;

    let rows = stmt
        .query_map([], |row| {
            Ok(OpenClawChunk {
                id: row.get::<_, String>(0)?,
                path: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                source: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                text: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                updated_at: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            })
        })
        .map_err(|e| MigrateError::Sqlite {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?;

    let mut chunks = Vec::new();
    for row in rows {
        chunks.push(row.map_err(|e| MigrateError::Sqlite {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?);
    }
    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
    }

    /// Build a tiny SQLite database with the same `chunks` schema OpenClaw uses.
    fn make_chunks_db(path: &Path, rows: &[(&str, &str, &str, &str, i64)]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE chunks (\
                id TEXT PRIMARY KEY, path TEXT, source TEXT, \
                start_line INTEGER, end_line INTEGER, hash TEXT, \
                model TEXT, text TEXT, embedding TEXT, updated_at INTEGER)",
        )
        .unwrap();
        for (id, p, source, text, updated) in rows {
            conn.execute(
                "INSERT INTO chunks (id, path, source, text, updated_at) VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, p, source, text, updated],
            )
            .unwrap();
        }
    }

    // ── Empty / missing directory ───────────────────────────────────────────

    #[test]
    fn load_empty_dir_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.memories.is_empty());
        assert!(inst.crons.is_empty());
        assert!(inst.personality_files.is_empty());
        assert!(inst.config.telegram.is_none());
        assert!(!inst.config_source_openclaw_json);
        assert!(!inst.memory_source.has_sqlite());
        assert!(!inst.memory_source.has_plureslm_store());
        assert!(inst.memory_source.chunks.is_empty());
    }

    // ── Real openclaw.json channel parsing ──────────────────────────────────

    #[test]
    fn load_openclaw_json_telegram_channel() {
        let dir = tempfile::tempdir().unwrap();
        // Real shape: channels.telegram { enabled, botToken, allowFrom, groups }.
        write_file(
            dir.path(),
            "openclaw.json",
            r#"{
                "meta": {"version": 1},
                "channels": {
                    "telegram": {
                        "enabled": true,
                        "botToken": "853707:REAL-TOKEN",
                        "allowFrom": [8573852722],
                        "groups": [{"id": -100123}]
                    }
                }
            }"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.config_source_openclaw_json);
        let tg = inst.config.telegram.as_ref().expect("telegram channel");
        assert!(tg.enabled);
        assert_eq!(tg.resolved_token(), "853707:REAL-TOKEN");
        assert_eq!(tg.bot_token, "853707:REAL-TOKEN");
        // Real installations use NUMERIC chat IDs; they must coerce to strings.
        assert_eq!(tg.allow_from_strings(), vec!["8573852722".to_string()]);
        assert_eq!(tg.group_count(), 1);
    }

    #[test]
    fn openclaw_json_real_shape_numeric_ids_and_map_groups() {
        // Mirrors the REAL ~/.openclaw/openclaw.json shapes that broke the
        // first cut: numeric allowFrom IDs and a `groups` MAP (not array).
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "openclaw.json",
            r#"{
                "channels": {
                    "telegram": {
                        "enabled": true,
                        "botToken": "853707:TOK",
                        "allowFrom": [8573852722],
                        "groups": {"-5240622952": {"enabled": true}},
                        "groupAllowFrom": []
                    }
                }
            }"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        let tg = inst.config.telegram.as_ref().unwrap();
        assert_eq!(tg.allow_from_strings(), vec!["8573852722".to_string()]);
        assert_eq!(tg.group_count(), 1, "map-shaped groups must count");
    }

    #[test]
    fn openclaw_json_wins_over_legacy_config_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "openclaw.json",
            r#"{"channels":{"telegram":{"enabled":true,"botToken":"NEW:TOKEN"}}}"#,
        );
        write_file(
            dir.path(),
            "config.json",
            r#"{"telegram":{"token":"OLD:TOKEN"}}"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.config_source_openclaw_json);
        assert_eq!(
            inst.config.telegram.as_ref().unwrap().resolved_token(),
            "NEW:TOKEN"
        );
    }

    // ── Legacy back-compat ──────────────────────────────────────────────────

    #[test]
    fn legacy_config_json_still_read_when_no_openclaw_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "config.json", r#"{"telegram":{"token":"123:ABC"}}"#);
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(!inst.config_source_openclaw_json);
        assert_eq!(
            inst.config.telegram.as_ref().unwrap().resolved_token(),
            "123:ABC"
        );
    }

    #[test]
    fn load_legacy_memories_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "memories.json",
            r#"[{"id":"abc","content":"hello world","category":"conversation","tags":[],"created_at":"2026-01-01T00:00:00Z"}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.memories.len(), 1);
        assert_eq!(inst.memories[0].id, "abc");
        assert_eq!(inst.memories[0].category, "conversation");
    }

    #[test]
    fn load_legacy_crons_json() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "crons.json",
            r#"[{"name":"daily","schedule":"0 9 * * *","action":"summarise","recurring":true}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.crons.len(), 1);
        assert_eq!(inst.crons[0].name, "daily");
        assert!(inst.crons[0].recurring);
    }

    #[test]
    fn cron_recurring_defaults_to_true() {
        let dir = tempfile::tempdir().unwrap();
        write_file(
            dir.path(),
            "crons.json",
            r#"[{"name":"weekly","schedule":"0 9 * * 1","action":"report"}]"#,
        );
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert!(inst.crons[0].recurring);
    }

    // ── Personality: workspace/*.md (with root fallback) ────────────────────

    #[test]
    fn load_personality_from_workspace_dir() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "workspace/SOUL.md", "# Soul\nI am an AI assistant.");
        write_file(dir.path(), "workspace/USER.md", "# User\nName: Alice");
        write_file(dir.path(), "workspace/IDENTITY.md", "# Identity");
        write_file(dir.path(), "workspace/MEMORY.md", "# Memory");
        write_file(dir.path(), "workspace/AGENTS.md", "# Agents");
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.personality_files.len(), 5);
        let keys: Vec<&str> = inst
            .personality_files
            .iter()
            .map(|p| p.key.as_str())
            .collect();
        for expected in ["soul", "user", "identity", "memory", "agents"] {
            assert!(keys.contains(&expected), "missing key {expected}");
        }
        // Provenance points at workspace/, not the root.
        let soul = inst
            .personality_files
            .iter()
            .find(|p| p.key == "soul")
            .unwrap();
        assert!(soul.source_path.ends_with("workspace/SOUL.md") || soul.source_path.ends_with("workspace\\SOUL.md"));
    }

    #[test]
    fn personality_falls_back_to_root_when_no_workspace() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "SOUL.md", "# Soul at root");
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        assert_eq!(inst.personality_files.len(), 1);
        assert_eq!(inst.personality_files[0].key, "soul");
        assert_eq!(inst.personality_files[0].content, "# Soul at root");
    }

    #[test]
    fn personality_prefers_workspace_over_root() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "SOUL.md", "ROOT");
        write_file(dir.path(), "workspace/SOUL.md", "WORKSPACE");
        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        let soul = inst
            .personality_files
            .iter()
            .find(|p| p.key == "soul")
            .unwrap();
        assert_eq!(soul.content, "WORKSPACE");
    }

    // ── Memory source: real SQLite chunks + pluresLM-store detection ─────────

    #[test]
    fn read_chunks_from_temp_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("main.sqlite");
        make_chunks_db(
            &db,
            &[
                ("c1", "AGENTS.md", "file", "First chunk text.", 1_700_000_000),
                ("c2", "SOUL.md", "file", "Second chunk text.", 1_700_000_100),
            ],
        );
        let chunks = read_chunks(&db).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].id, "c1");
        assert_eq!(chunks[0].path, "AGENTS.md");
        assert_eq!(chunks[0].source, "file");
        assert_eq!(chunks[0].text, "First chunk text.");
        assert_eq!(chunks[0].updated_at, 1_700_000_000);
    }

    #[test]
    fn read_chunks_errors_on_missing_table() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("bad.sqlite");
        // A valid SQLite file, but WITHOUT a `chunks` table.
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE other (x)").unwrap();
        drop(conn);
        let err = read_chunks(&db).unwrap_err();
        assert!(matches!(err, MigrateError::Sqlite { .. }));
    }

    #[test]
    fn load_memory_source_reads_sqlite_and_detects_plureslm() {
        let dir = tempfile::tempdir().unwrap();
        make_chunks_db(
            &dir.path().join("memory").join("main.sqlite"),
            &[("c1", "MEMORY.md", "file", "hello", 1_700_000_000)],
        );
        // Fake a pluresLM-store dir with a couple of files.
        write_file(dir.path(), "pluresLM-store/db", "binary-ish");
        write_file(dir.path(), "pluresLM-store/blobs/a.bin", "blob");

        let inst = OpenClawInstallation::load(dir.path()).unwrap();
        let ms = &inst.memory_source;
        assert!(ms.has_sqlite());
        assert_eq!(ms.chunks.len(), 1);
        assert_eq!(ms.chunks[0].id, "c1");
        assert!(ms.sqlite_size_bytes.unwrap() > 0);
        assert!(ms.has_plureslm_store());
        assert!(ms.plureslm_store_size_bytes.unwrap() > 0);
    }

    // ── Invalid JSON surfaces as an error (fail-clear, not silent) ───────────

    #[test]
    fn load_invalid_openclaw_json_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "openclaw.json", "not json");
        let result = OpenClawInstallation::load(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_legacy_memories_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "memories.json", "not json");
        let result = OpenClawInstallation::load(dir.path());
        assert!(result.is_err());
    }

    // ── auto_detect helper ──────────────────────────────────────────────────

    #[test]
    fn auto_detect_returns_none_for_nonexistent_dir() {
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
}
