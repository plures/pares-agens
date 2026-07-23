//! Local skill authoring lifecycle - the "skill workshop".
//!
//! Where [`crate::installer::Installer`] and [`crate::discovery`] handle the
//! *consume-remote* side of skills (download a signed archive published by
//! someone else), this module handles the complementary *author-local* side:
//! a human or agent drafts a skill as a **proposal**, revises it, lists and
//! inspects pending proposals, and then explicitly **applies** it - installing
//! the procedure into a live-skills directory as `SKILL.md` plus its support
//! files. Proposals that are not wanted are **rejected** or **quarantined**.
//!
//! This mirrors OpenClaw's `skill_workshop` tool: proposals are *pending by
//! default*, and `apply` is a separate, deliberate step. It is not a duplicate
//! of the installer - the installer never authors content and never writes a
//! human-drafted `SKILL.md`; the workshop never downloads or verifies a remote
//! archive. Both live in the marketplace crate because the marketplace is the
//! single source of truth for skill concepts in Pares Agens.
//!
//! # Real I/O, no stubs
//!
//! Every method here performs real filesystem work against a proposals root and
//! a live-skills root. There are no placeholder timestamps and no canned return
//! values; missing directories and disallowed paths surface as real
//! [`WorkshopError`] values, never silent successes.

use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Limits & allowed layout ────────────────────────────────────────────────────

/// Maximum length, in bytes, of a proposal description (matches OpenClaw's
/// `skill_workshop` 160-byte cap).
pub const MAX_DESCRIPTION_BYTES: usize = 160;

/// Maximum size, in bytes, of a proposal body (`PROPOSAL.md`). Mirrors
/// OpenClaw's default `skills.workshop.maxSkillBytes` of 40 000 bytes.
pub const MAX_BODY_BYTES: usize = 40_000;

/// Top-level subdirectories a support file is allowed to live under, relative to
/// the proposal (and, once applied, the installed skill) root.
///
/// Anything outside these prefixes - or any path that escapes the root via `..`,
/// an absolute path, or a drive/prefix component - is rejected at validation
/// time so that authoring can never write outside the skill directory.
pub const ALLOWED_SUPPORT_DIRS: [&str; 5] =
    ["assets", "examples", "references", "scripts", "templates"];

/// Filename of the proposal body inside a proposal directory.
const PROPOSAL_BODY_FILE: &str = "PROPOSAL.md";

/// Filename of the proposal metadata sidecar inside a proposal directory.
const PROPOSAL_META_FILE: &str = "meta.json";

/// Filename the body is installed as inside a live-skill directory.
const INSTALLED_SKILL_FILE: &str = "SKILL.md";

/// Subdirectory (under the proposal root) that holds a proposal's support files,
/// mirroring the eventual installed layout.
const SUPPORT_ROOT: &str = "files";

// ── Errors ─────────────────────────────────────────────────────────────────────

/// Errors that can occur while authoring, storing, or applying a skill proposal.
#[derive(Debug, Error)]
pub enum WorkshopError {
    /// A proposal field failed validation (empty name, oversized description or
    /// body, disallowed support-file path, ...).
    #[error("invalid proposal: {0}")]
    Invalid(String),

    /// No proposal with the requested id exists in the store.
    #[error("proposal not found: {0}")]
    NotFound(String),

    /// A proposal with the requested id already exists.
    #[error("proposal already exists: {0}")]
    AlreadyExists(String),

    /// The proposal is not in a state that permits the requested transition
    /// (e.g. applying an already-applied proposal).
    #[error("invalid state transition: {0}")]
    InvalidState(String),

    /// The live-skills directory required by `apply` does not exist.
    #[error("skills directory not found: {0}")]
    SkillsDirMissing(String),

    /// An underlying filesystem operation failed.
    #[error("filesystem error at {path}: {source}")]
    Io {
        /// Path the failing operation targeted.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// JSON (de)serialisation of the proposal metadata failed.
    #[error("metadata error: {0}")]
    Json(#[from] serde_json::Error),
}

impl WorkshopError {
    /// Build an [`WorkshopError::Io`] tagged with the offending `path`.
    fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }
}

/// Convenience result alias for workshop operations.
pub type Result<T> = std::result::Result<T, WorkshopError>;

// ── Proposal model ─────────────────────────────────────────────────────────────

/// Lifecycle state of a [`SkillProposal`].
///
/// A proposal is [`Pending`](ProposalStatus::Pending) on creation and moves to
/// exactly one terminal-ish state via an explicit action:
/// [`Applied`](ProposalStatus::Applied) once installed into live skills,
/// [`Rejected`](ProposalStatus::Rejected) if declined, or
/// [`Quarantined`](ProposalStatus::Quarantined) if held for review.
/// [`Stale`](ProposalStatus::Stale) marks a pending proposal that has aged out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    /// Drafted and awaiting a decision. The default on creation.
    Pending,
    /// Installed into the live-skills directory.
    Applied,
    /// Declined; will not be installed.
    Rejected,
    /// Held for review; neither applied nor discarded.
    Quarantined,
    /// A pending proposal that has aged out and should be revisited.
    Stale,
}

impl ProposalStatus {
    /// Human-readable, lowercase label for this status.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Rejected => "rejected",
            Self::Quarantined => "quarantined",
            Self::Stale => "stale",
        }
    }
}

/// A support file attached to a proposal: a path relative to the skill root plus
/// its full text content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportFile {
    /// Path relative to the skill root. Must live under one of
    /// [`ALLOWED_SUPPORT_DIRS`] and must not escape the root.
    pub path: String,
    /// Full text content to write at `path`.
    pub content: String,
}

/// A locally-authored skill proposal.
///
/// The proposal owns its identity, human metadata, the procedure body (stored as
/// `PROPOSAL.md`), any support files, and its lifecycle [`ProposalStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillProposal {
    /// Stable, unique identifier for this proposal (slug of the name plus a
    /// short disambiguating suffix).
    pub id: String,
    /// Skill name (used as the installed directory name on `apply`).
    pub name: String,
    /// Short description (≤ [`MAX_DESCRIPTION_BYTES`] bytes).
    pub description: String,
    /// Procedure markdown, persisted as `PROPOSAL.md` (≤ [`MAX_BODY_BYTES`]).
    pub body: String,
    /// Optional support files bundled with the proposal.
    #[serde(default)]
    pub support_files: Vec<SupportFile>,
    /// Current lifecycle status.
    pub status: ProposalStatus,
    /// Name/key of an existing live skill this proposal updates, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_skill: Option<String>,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 timestamp of the last mutation.
    pub updated_at: String,
    /// Absolute path the proposal was installed to, once `Applied`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_path: Option<String>,
}

/// The mutable inputs used to draft or revise a proposal.
///
/// Kept separate from [`SkillProposal`] so callers construct a small, obviously
/// user-supplied value at the boundary; the store stamps identity/timestamps.
#[derive(Debug, Clone, Default)]
pub struct ProposalDraft {
    /// Skill name (required).
    pub name: String,
    /// Short description (required, ≤ [`MAX_DESCRIPTION_BYTES`] bytes).
    pub description: String,
    /// Procedure body (required, ≤ [`MAX_BODY_BYTES`]).
    pub body: String,
    /// Optional support files.
    pub support_files: Vec<SupportFile>,
    /// Optional existing live-skill this proposal targets.
    pub target_skill: Option<String>,
}

// ── Validation ─────────────────────────────────────────────────────────────────

/// Validates [`ProposalDraft`]s at the authoring boundary.
///
/// This is the workshop counterpart to [`crate::MetadataValidator`]: it enforces
/// the byte limits and the allowed support-file layout so that invalid states
/// never reach the filesystem.
#[derive(Debug, Default)]
pub struct ProposalValidator;

impl ProposalValidator {
    /// Create a new validator.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Validate a draft, returning the first problem found.
    ///
    /// Checks, in order: non-empty name; name usable as a directory (no path
    /// separators or traversal); description non-empty and ≤
    /// [`MAX_DESCRIPTION_BYTES`] bytes; body non-empty and ≤ [`MAX_BODY_BYTES`]
    /// bytes; every support-file path is relative, escapes nothing, and lives
    /// under an [`ALLOWED_SUPPORT_DIRS`] prefix.
    ///
    /// # Errors
    ///
    /// Returns [`WorkshopError::Invalid`] describing the first failing check.
    pub fn validate(&self, draft: &ProposalDraft) -> Result<()> {
        if draft.name.trim().is_empty() {
            return Err(WorkshopError::Invalid("name must not be empty".to_string()));
        }
        if !is_valid_skill_name(&draft.name) {
            return Err(WorkshopError::Invalid(format!(
                "name '{}' is not a valid skill name (allowed: alphanumeric, '-', '_', '.', \
                 no path separators or '..')",
                draft.name
            )));
        }
        if draft.description.trim().is_empty() {
            return Err(WorkshopError::Invalid(
                "description must not be empty".to_string(),
            ));
        }
        let desc_len = draft.description.len();
        if desc_len > MAX_DESCRIPTION_BYTES {
            return Err(WorkshopError::Invalid(format!(
                "description is {desc_len} bytes; limit is {MAX_DESCRIPTION_BYTES} bytes"
            )));
        }
        if draft.body.trim().is_empty() {
            return Err(WorkshopError::Invalid("body must not be empty".to_string()));
        }
        let body_len = draft.body.len();
        if body_len > MAX_BODY_BYTES {
            return Err(WorkshopError::Invalid(format!(
                "body is {body_len} bytes; limit is {MAX_BODY_BYTES} bytes"
            )));
        }
        for sf in &draft.support_files {
            validate_support_path(&sf.path)?;
        }
        Ok(())
    }
}

/// Returns `true` when `name` is safe to use as a single directory component:
/// non-empty, only `A-Za-z0-9._-`, and not `.`/`..`.
fn is_valid_skill_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Validate that a support-file path is relative, does not escape the skill
/// root, and lives under an allowed top-level directory.
///
/// # Errors
///
/// Returns [`WorkshopError::Invalid`] when the path is absolute, empty, uses
/// `..`/prefix/root components, or is not under [`ALLOWED_SUPPORT_DIRS`].
fn validate_support_path(rel: &str) -> Result<()> {
    if rel.trim().is_empty() {
        return Err(WorkshopError::Invalid(
            "support file path must not be empty".to_string(),
        ));
    }
    let path = Path::new(rel);
    if path.is_absolute() {
        return Err(WorkshopError::Invalid(format!(
            "support file path '{rel}' must be relative"
        )));
    }
    let mut components = path.components();
    // First component must be an allowed directory name.
    let first = match components.next() {
        Some(Component::Normal(os)) => os.to_string_lossy().to_string(),
        _ => {
            return Err(WorkshopError::Invalid(format!(
                "support file path '{rel}' must start with one of {ALLOWED_SUPPORT_DIRS:?}"
            )));
        }
    };
    if !ALLOWED_SUPPORT_DIRS.contains(&first.as_str()) {
        return Err(WorkshopError::Invalid(format!(
            "support file path '{rel}' must be under one of {ALLOWED_SUPPORT_DIRS:?}"
        )));
    }
    // A file directly at the allowed dir with no child is not a file path.
    let mut saw_child = false;
    for comp in components {
        match comp {
            Component::Normal(_) => saw_child = true,
            // Reject any traversal, absolute, or prefix component anywhere.
            _ => {
                return Err(WorkshopError::Invalid(format!(
                    "support file path '{rel}' must not contain '..', '.', or absolute segments"
                )));
            }
        }
    }
    if !saw_child {
        return Err(WorkshopError::Invalid(format!(
            "support file path '{rel}' must name a file inside '{first}/'"
        )));
    }
    Ok(())
}

// ── Timestamps ─────────────────────────────────────────────────────────────────

/// Current time as an RFC 3339 / ISO 8601 UTC string.
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Derive a stable proposal id from a skill name: a lowercase slug plus a short
/// timestamp-derived suffix for disambiguation.
fn derive_id(name: &str) -> String {
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "skill" } else { slug };
    // Millisecond suffix keeps ids unique for rapid successive creates while
    // remaining human-readable.
    let suffix = chrono::Utc::now().timestamp_millis();
    format!("{slug}-{suffix}")
}

// ── Filesystem-backed store ────────────────────────────────────────────────────

/// A filesystem-backed store of skill proposals.
///
/// Each proposal is persisted as a subdirectory under `root`:
///
/// ```text
/// <root>/<id>/
///   ├── meta.json      # serialised SkillProposal (sans body)
///   ├── PROPOSAL.md    # the procedure body
///   └── files/         # support files, preserving their relative layout
///       └── scripts/...
/// ```
///
/// The store also knows the live-skills `skills_dir` used by [`Self::apply`].
#[derive(Debug, Clone)]
pub struct ProposalStore {
    root: PathBuf,
    skills_dir: PathBuf,
}

impl ProposalStore {
    /// Open (creating if needed) a proposal store rooted at `root`, installing
    /// applied skills under `skills_dir`.
    ///
    /// The proposals `root` is created on demand. `skills_dir` is **not** created
    /// here - its absence is only an error at [`Self::apply`] time, so that
    /// listing/inspecting proposals works even before any skills dir exists.
    ///
    /// # Errors
    ///
    /// Returns [`WorkshopError::Io`] if the proposals `root` cannot be created.
    pub fn open(root: impl Into<PathBuf>, skills_dir: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let skills_dir = skills_dir.into();
        fs::create_dir_all(&root).map_err(|e| WorkshopError::io(&root, e))?;
        Ok(Self { root, skills_dir })
    }

    /// The proposals root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The live-skills directory applied proposals install into.
    #[must_use]
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Directory holding a single proposal's files.
    fn proposal_dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// Create a new proposal from `draft`, validate it, persist it, and return
    /// the stored [`SkillProposal`] (status [`ProposalStatus::Pending`]).
    ///
    /// # Errors
    ///
    /// - [`WorkshopError::Invalid`] - the draft failed validation.
    /// - [`WorkshopError::AlreadyExists`] - id collision (extremely unlikely).
    /// - [`WorkshopError::Io`] / [`WorkshopError::Json`] - persistence failed.
    pub fn create(&self, draft: ProposalDraft) -> Result<SkillProposal> {
        ProposalValidator::new().validate(&draft)?;
        let now = now_rfc3339();
        let proposal = SkillProposal {
            id: derive_id(&draft.name),
            name: draft.name,
            description: draft.description,
            body: draft.body,
            support_files: draft.support_files,
            status: ProposalStatus::Pending,
            target_skill: draft.target_skill,
            created_at: now.clone(),
            updated_at: now,
            applied_path: None,
        };
        let dir = self.proposal_dir(&proposal.id);
        if dir.exists() {
            return Err(WorkshopError::AlreadyExists(proposal.id));
        }
        self.persist(&proposal)?;
        Ok(proposal)
    }

    /// Load a single proposal by `id`.
    ///
    /// # Errors
    ///
    /// - [`WorkshopError::NotFound`] - no such proposal.
    /// - [`WorkshopError::Io`] / [`WorkshopError::Json`] - read/parse failed.
    pub fn get(&self, id: &str) -> Result<SkillProposal> {
        let dir = self.proposal_dir(id);
        if !dir.is_dir() {
            return Err(WorkshopError::NotFound(id.to_string()));
        }
        let meta_path = dir.join(PROPOSAL_META_FILE);
        let meta_raw =
            fs::read_to_string(&meta_path).map_err(|e| WorkshopError::io(&meta_path, e))?;
        let mut proposal: SkillProposal = serde_json::from_str(&meta_raw)?;
        // The body is the source of truth on disk; reload it so external edits to
        // PROPOSAL.md are honoured.
        let body_path = dir.join(PROPOSAL_BODY_FILE);
        if body_path.is_file() {
            proposal.body =
                fs::read_to_string(&body_path).map_err(|e| WorkshopError::io(&body_path, e))?;
        }
        Ok(proposal)
    }

    /// List proposals, optionally filtered by `status`, newest first.
    ///
    /// Malformed proposal directories are skipped rather than failing the whole
    /// listing, so one bad entry never hides the rest.
    ///
    /// # Errors
    ///
    /// Returns [`WorkshopError::Io`] if the proposals root cannot be read.
    pub fn list(&self, status: Option<ProposalStatus>) -> Result<Vec<SkillProposal>> {
        let mut out = Vec::new();
        let entries = fs::read_dir(&self.root).map_err(|e| WorkshopError::io(&self.root, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| WorkshopError::io(&self.root, e))?;
            if !entry.path().is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            match self.get(&id) {
                Ok(p) => {
                    if status.is_none_or(|s| p.status == s) {
                        out.push(p);
                    }
                }
                // Skip unreadable/foreign directories; do not abort the listing.
                Err(WorkshopError::NotFound(_)) => continue,
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// Revise a pending proposal in place from a new `draft`, preserving id and
    /// `created_at` while refreshing `updated_at`.
    ///
    /// Only [`ProposalStatus::Pending`] or [`ProposalStatus::Stale`] proposals
    /// may be revised — a decided proposal (applied/rejected/quarantined) is
    /// immutable.
    ///
    /// # Errors
    ///
    /// - [`WorkshopError::NotFound`] — no such proposal.
    /// - [`WorkshopError::Invalid`] — the new draft failed validation.
    /// - [`WorkshopError::InvalidState`] — the proposal is not revisable.
    /// - [`WorkshopError::Io`] / [`WorkshopError::Json`] — persistence failed.
    pub fn revise(&self, id: &str, draft: ProposalDraft) -> Result<SkillProposal> {
        ProposalValidator::new().validate(&draft)?;
        let mut existing = self.get(id)?;
        if !matches!(existing.status, ProposalStatus::Pending | ProposalStatus::Stale) {
            return Err(WorkshopError::InvalidState(format!(
                "proposal '{id}' is '{}' and cannot be revised (only pending/stale)",
                existing.status.as_str()
            )));
        }
        // Clear any stale support files from the previous revision before
        // rewriting, so removed files do not linger on disk.
        let support_root = self.proposal_dir(id).join(SUPPORT_ROOT);
        if support_root.exists() {
            fs::remove_dir_all(&support_root).map_err(|e| WorkshopError::io(&support_root, e))?;
        }
        existing.name = draft.name;
        existing.description = draft.description;
        existing.body = draft.body;
        existing.support_files = draft.support_files;
        existing.target_skill = draft.target_skill;
        // Revising a stale proposal returns it to the pending queue.
        existing.status = ProposalStatus::Pending;
        existing.updated_at = now_rfc3339();
        self.persist(&existing)?;
        Ok(existing)
    }

    /// Set a proposal's `status` directly (used by reject/quarantine and to mark
    /// proposals stale), refreshing `updated_at`.
    ///
    /// # Errors
    ///
    /// - [`WorkshopError::NotFound`] — no such proposal.
    /// - [`WorkshopError::Io`] / [`WorkshopError::Json`] — persistence failed.
    pub fn set_status(&self, id: &str, status: ProposalStatus) -> Result<SkillProposal> {
        let mut proposal = self.get(id)?;
        proposal.status = status;
        proposal.updated_at = now_rfc3339();
        self.persist(&proposal)?;
        Ok(proposal)
    }

    /// Mark a proposal [`ProposalStatus::Rejected`].
    ///
    /// # Errors
    ///
    /// See [`Self::set_status`].
    pub fn reject(&self, id: &str) -> Result<SkillProposal> {
        self.set_status(id, ProposalStatus::Rejected)
    }

    /// Mark a proposal [`ProposalStatus::Quarantined`].
    ///
    /// # Errors
    ///
    /// See [`Self::set_status`].
    pub fn quarantine(&self, id: &str) -> Result<SkillProposal> {
        self.set_status(id, ProposalStatus::Quarantined)
    }

    /// Apply a pending proposal: install it into the live-skills directory as
    /// `<skills_dir>/<name>/SKILL.md` plus its support files, then mark the
    /// proposal [`ProposalStatus::Applied`] and record `applied_path`.
    ///
    /// The install is atomic-ish: the skill directory is (re)created and fully
    /// written before the status flips. The `skills_dir` must already exist —
    /// its absence is a real [`WorkshopError::SkillsDirMissing`], never a silent
    /// success.
    ///
    /// # Errors
    ///
    /// - [`WorkshopError::NotFound`] — no such proposal.
    /// - [`WorkshopError::InvalidState`] — the proposal is not pending/stale.
    /// - [`WorkshopError::SkillsDirMissing`] — `skills_dir` does not exist.
    /// - [`WorkshopError::Invalid`] — a support path failed re-validation.
    /// - [`WorkshopError::Io`] / [`WorkshopError::Json`] — install failed.
    pub fn apply(&self, id: &str) -> Result<SkillProposal> {
        let mut proposal = self.get(id)?;
        if !matches!(proposal.status, ProposalStatus::Pending | ProposalStatus::Stale) {
            return Err(WorkshopError::InvalidState(format!(
                "proposal '{id}' is '{}' and cannot be applied (only pending/stale)",
                proposal.status.as_str()
            )));
        }
        if !self.skills_dir.is_dir() {
            return Err(WorkshopError::SkillsDirMissing(
                self.skills_dir.display().to_string(),
            ));
        }
        // Defence in depth: re-validate the name and every support path before
        // touching the live-skills tree.
        if !is_valid_skill_name(&proposal.name) {
            return Err(WorkshopError::Invalid(format!(
                "stored proposal name '{}' is not a valid skill directory name",
                proposal.name
            )));
        }
        let skill_dir = self.skills_dir.join(&proposal.name);
        // Replace any prior install of the same skill name.
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir).map_err(|e| WorkshopError::io(&skill_dir, e))?;
        }
        fs::create_dir_all(&skill_dir).map_err(|e| WorkshopError::io(&skill_dir, e))?;

        let skill_file = skill_dir.join(INSTALLED_SKILL_FILE);
        fs::write(&skill_file, &proposal.body).map_err(|e| WorkshopError::io(&skill_file, e))?;

        for sf in &proposal.support_files {
            validate_support_path(&sf.path)?;
            let dest = skill_dir.join(&sf.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| WorkshopError::io(parent, e))?;
            }
            fs::write(&dest, &sf.content).map_err(|e| WorkshopError::io(&dest, e))?;
        }

        proposal.status = ProposalStatus::Applied;
        proposal.applied_path = Some(skill_dir.display().to_string());
        proposal.updated_at = now_rfc3339();
        self.persist(&proposal)?;
        Ok(proposal)
    }

    /// Persist `proposal` to its directory: write `meta.json`, `PROPOSAL.md`, and
    /// all support files under `files/`.
    fn persist(&self, proposal: &SkillProposal) -> Result<()> {
        let dir = self.proposal_dir(&proposal.id);
        fs::create_dir_all(&dir).map_err(|e| WorkshopError::io(&dir, e))?;

        // Body lives in PROPOSAL.md; keep meta.json's copy but PROPOSAL.md is the
        // authoritative body on reload.
        let body_path = dir.join(PROPOSAL_BODY_FILE);
        fs::write(&body_path, &proposal.body).map_err(|e| WorkshopError::io(&body_path, e))?;

        let meta_path = dir.join(PROPOSAL_META_FILE);
        let meta_json = serde_json::to_string_pretty(proposal)?;
        fs::write(&meta_path, meta_json).map_err(|e| WorkshopError::io(&meta_path, e))?;

        // Support files, preserving their relative layout under files/.
        let support_root = dir.join(SUPPORT_ROOT);
        for sf in &proposal.support_files {
            validate_support_path(&sf.path)?;
            let dest = support_root.join(&sf.path);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| WorkshopError::io(parent, e))?;
            }
            fs::write(&dest, &sf.content).map_err(|e| WorkshopError::io(&dest, e))?;
        }
        Ok(())
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Monotonic counter so concurrent tests get distinct temp directories.
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A self-cleaning temporary directory (no external `tempfile` dependency).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let pid = std::process::id();
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!("agens-workshop-{tag}-{pid}-{nanos}-{n}"));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn join(&self, sub: &str) -> PathBuf {
            self.path.join(sub)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Build a store whose proposals root and skills dir both live under a fresh
    /// temp dir. The skills dir is created (so `apply` succeeds); tests that need
    /// the missing-dir path use [`store_without_skills_dir`].
    fn store(tag: &str) -> (TempDir, ProposalStore) {
        let tmp = TempDir::new(tag);
        let skills = tmp.join("skills");
        fs::create_dir_all(&skills).expect("create skills dir");
        let s = ProposalStore::open(tmp.join("proposals"), skills).expect("open store");
        (tmp, s)
    }

    /// Build a store whose skills dir does NOT exist, to exercise the
    /// `SkillsDirMissing` error on apply.
    fn store_without_skills_dir(tag: &str) -> (TempDir, ProposalStore) {
        let tmp = TempDir::new(tag);
        let s = ProposalStore::open(tmp.join("proposals"), tmp.join("does-not-exist"))
            .expect("open store");
        (tmp, s)
    }

    fn valid_draft() -> ProposalDraft {
        ProposalDraft {
            name: "rust-helper".to_string(),
            description: "Helps write idiomatic Rust.".to_string(),
            body: "# Rust Helper\n\nDo the thing, then the other thing.\n".to_string(),
            support_files: vec![SupportFile {
                path: "scripts/run.sh".to_string(),
                content: "#!/usr/bin/env bash\necho hi\n".to_string(),
            }],
            target_skill: None,
        }
    }

    // ── validation ──────────────────────────────────────────────────────────

    #[test]
    fn valid_draft_passes_validation() {
        assert!(ProposalValidator::new().validate(&valid_draft()).is_ok());
    }

    #[test]
    fn rejects_empty_name() {
        let mut d = valid_draft();
        d.name = String::new();
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_name_with_path_separator() {
        let mut d = valid_draft();
        d.name = "evil/../name".to_string();
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_empty_description() {
        let mut d = valid_draft();
        d.description = "   ".to_string();
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_description_over_160_bytes() {
        let mut d = valid_draft();
        d.description = "x".repeat(MAX_DESCRIPTION_BYTES + 1);
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn accepts_description_at_exactly_160_bytes() {
        let mut d = valid_draft();
        d.description = "x".repeat(MAX_DESCRIPTION_BYTES);
        assert!(ProposalValidator::new().validate(&d).is_ok());
    }

    #[test]
    fn rejects_empty_body() {
        let mut d = valid_draft();
        d.body = "\n\t  \n".to_string();
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_body_over_max_bytes() {
        let mut d = valid_draft();
        d.body = "y".repeat(MAX_BODY_BYTES + 1);
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_support_path_outside_allowed_dirs() {
        let mut d = valid_draft();
        d.support_files = vec![SupportFile {
            path: "secrets/creds.txt".to_string(),
            content: "nope".to_string(),
        }];
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_support_path_with_traversal() {
        let mut d = valid_draft();
        d.support_files = vec![SupportFile {
            path: "scripts/../../etc/passwd".to_string(),
            content: "nope".to_string(),
        }];
        assert!(matches!(
            ProposalValidator::new().validate(&d),
            Err(WorkshopError::Invalid(_))
        ));
    }

    #[test]
    fn rejects_absolute_support_path() {
        assert!(validate_support_path("/etc/passwd").is_err());
    }

    #[test]
    fn rejects_support_path_that_is_only_a_dir() {
        assert!(validate_support_path("scripts").is_err());
    }

    #[test]
    fn accepts_nested_support_path_under_allowed_dir() {
        assert!(validate_support_path("references/api/v1/spec.md").is_ok());
    }

    // ── create / list / inspect / apply happy path ──────────────────────

    #[test]
    fn create_persists_pending_proposal() {
        let (_tmp, s) = store("create");
        let p = s.create(valid_draft()).unwrap();
        assert_eq!(p.status, ProposalStatus::Pending);
        assert_eq!(p.name, "rust-helper");
        assert!(p.applied_path.is_none());
        let dir = s.root().join(&p.id);
        assert!(dir.join("meta.json").is_file());
        assert!(dir.join("PROPOSAL.md").is_file());
        assert!(dir.join("files/scripts/run.sh").is_file());
    }

    #[test]
    fn create_rejects_invalid_draft() {
        let (_tmp, s) = store("create-invalid");
        let mut d = valid_draft();
        d.description = "z".repeat(MAX_DESCRIPTION_BYTES + 1);
        assert!(matches!(s.create(d), Err(WorkshopError::Invalid(_))));
    }

    #[test]
    fn list_returns_created_proposal() {
        let (_tmp, s) = store("list");
        let p = s.create(valid_draft()).unwrap();
        let all = s.list(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, p.id);
    }

    #[test]
    fn list_filters_by_status() {
        let (_tmp, s) = store("list-filter");
        let pending = s.create(valid_draft()).unwrap();
        let mut d2 = valid_draft();
        d2.name = "second-skill".to_string();
        let second = s.create(d2).unwrap();
        s.reject(&second.id).unwrap();

        let pendings = s.list(Some(ProposalStatus::Pending)).unwrap();
        assert_eq!(pendings.len(), 1);
        assert_eq!(pendings[0].id, pending.id);

        let rejected = s.list(Some(ProposalStatus::Rejected)).unwrap();
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].id, second.id);
    }

    #[test]
    fn inspect_roundtrips_body_and_support() {
        let (_tmp, s) = store("inspect");
        let created = s.create(valid_draft()).unwrap();
        let got = s.get(&created.id).unwrap();
        assert_eq!(got.body, created.body);
        assert_eq!(got.support_files.len(), 1);
        assert_eq!(got.support_files[0].path, "scripts/run.sh");
    }

    #[test]
    fn get_unknown_is_not_found() {
        let (_tmp, s) = store("get-missing");
        assert!(matches!(s.get("nope-123"), Err(WorkshopError::NotFound(_))));
    }

    #[test]
    fn apply_installs_skill_md_and_support_files() {
        let (_tmp, s) = store("apply");
        let created = s.create(valid_draft()).unwrap();
        let applied = s.apply(&created.id).unwrap();

        assert_eq!(applied.status, ProposalStatus::Applied);
        let skill_dir = s.skills_dir().join("rust-helper");
        let skill_md = skill_dir.join("SKILL.md");
        assert!(skill_md.is_file(), "SKILL.md must exist after apply");
        let body = fs::read_to_string(&skill_md).unwrap();
        assert!(body.contains("Rust Helper"));
        assert!(skill_dir.join("scripts/run.sh").is_file());
        assert_eq!(
            applied.applied_path.as_deref(),
            Some(skill_dir.display().to_string().as_str())
        );
        assert_eq!(s.get(&created.id).unwrap().status, ProposalStatus::Applied);
    }

    #[test]
    fn apply_twice_is_invalid_state() {
        let (_tmp, s) = store("apply-twice");
        let created = s.create(valid_draft()).unwrap();
        s.apply(&created.id).unwrap();
        assert!(matches!(
            s.apply(&created.id),
            Err(WorkshopError::InvalidState(_))
        ));
    }

    #[test]
    fn apply_without_skills_dir_errors() {
        let (_tmp, s) = store_without_skills_dir("apply-no-dir");
        let created = s.create(valid_draft()).unwrap();
        assert!(matches!(
            s.apply(&created.id),
            Err(WorkshopError::SkillsDirMissing(_))
        ));
    }

    // ── reject / quarantine ─────────────────────────────────────────

    #[test]
    fn reject_sets_status_and_persists() {
        let (_tmp, s) = store("reject");
        let created = s.create(valid_draft()).unwrap();
        let r = s.reject(&created.id).unwrap();
        assert_eq!(r.status, ProposalStatus::Rejected);
        assert_eq!(s.get(&created.id).unwrap().status, ProposalStatus::Rejected);
    }

    #[test]
    fn quarantine_sets_status_and_persists() {
        let (_tmp, s) = store("quarantine");
        let created = s.create(valid_draft()).unwrap();
        let q = s.quarantine(&created.id).unwrap();
        assert_eq!(q.status, ProposalStatus::Quarantined);
        assert_eq!(
            s.get(&created.id).unwrap().status,
            ProposalStatus::Quarantined
        );
    }

    #[test]
    fn rejected_proposal_cannot_be_applied() {
        let (_tmp, s) = store("reject-then-apply");
        let created = s.create(valid_draft()).unwrap();
        s.reject(&created.id).unwrap();
        assert!(matches!(
            s.apply(&created.id),
            Err(WorkshopError::InvalidState(_))
        ));
    }

    // ── revise ────────────────────────────────────────────────────────────

    #[test]
    fn revise_updates_body_and_clears_old_support() {
        let (_tmp, s) = store("revise");
        let created = s.create(valid_draft()).unwrap();
        let mut d = valid_draft();
        d.body = "# Rust Helper v2\n\nNew procedure.\n".to_string();
        d.support_files = vec![SupportFile {
            path: "examples/demo.md".to_string(),
            content: "example".to_string(),
        }];
        let revised = s.revise(&created.id, d).unwrap();
        assert_eq!(revised.id, created.id);
        assert_eq!(revised.created_at, created.created_at);
        assert!(revised.body.contains("v2"));
        let files_dir = s.root().join(&created.id).join("files");
        assert!(!files_dir.join("scripts/run.sh").exists());
        assert!(files_dir.join("examples/demo.md").is_file());
    }

    #[test]
    fn revise_rejects_applied_proposal() {
        let (_tmp, s) = store("revise-applied");
        let created = s.create(valid_draft()).unwrap();
        s.apply(&created.id).unwrap();
        assert!(matches!(
            s.revise(&created.id, valid_draft()),
            Err(WorkshopError::InvalidState(_))
        ));
    }

    // ── status label ────────────────────────────────────────────────

    #[test]
    fn status_labels_are_stable() {
        assert_eq!(ProposalStatus::Pending.as_str(), "pending");
        assert_eq!(ProposalStatus::Applied.as_str(), "applied");
        assert_eq!(ProposalStatus::Rejected.as_str(), "rejected");
        assert_eq!(ProposalStatus::Quarantined.as_str(), "quarantined");
        assert_eq!(ProposalStatus::Stale.as_str(), "stale");
    }
}
