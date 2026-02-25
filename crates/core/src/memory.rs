use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Minimum content length (in characters) that passes the quality gate.
pub const MIN_CONTENT_LEN: usize = 20;

/// Default number of memories to retrieve per recall.
pub const DEFAULT_RECALL_LIMIT: usize = 5;

/// Default character budget for injected recall context (≈25% of a typical
/// 8 k-token context window at ~4 chars/token).
pub const DEFAULT_BUDGET_CHARS: usize = 8_000;

/// Category of a stored memory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    /// User preferences ("always use X", "never do Y").
    Preference,
    /// Decisions made during the conversation ("we decided to use X").
    Decision,
    /// Named entities (people, projects, organisations).
    Entity,
    /// Long-lived project context injected by the user; excluded from
    /// auto-recall by default so it doesn't pollute the relevance window.
    ProjectContext,
    /// Catch-all for content that doesn't match other categories.
    Other,
}

/// A single recalled memory entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// Unique identifier.
    pub id: String,
    /// The stored content.
    pub content: String,
    /// Detected category.
    pub category: MemoryCategory,
    /// Relevance score returned by the vector search (higher = more relevant).
    pub score: f32,
}

/// An exchange between user and agent used for memory capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Exchange {
    /// The original user message.
    pub user_message: String,
    /// The agent's response.
    pub agent_response: String,
}

// ---------------------------------------------------------------------------
// MemoryStore trait
// ---------------------------------------------------------------------------

/// Abstraction over PluresLM for memory recall and capture.
///
/// In production this is backed by the real PluresLM client; in tests a
/// mock implementation is used.
#[async_trait]
pub trait MemoryStore: Send + Sync {
    /// Search for memories relevant to `query`.
    ///
    /// `limit` caps the number of results; `exclude_categories` filters out
    /// categories (e.g. `ProjectContext`) that should not appear in auto-recall.
    async fn recall(
        &self,
        query: &str,
        limit: usize,
        exclude_categories: &[MemoryCategory],
    ) -> Vec<Memory>;

    /// Store a meaningful exchange as one or more memories.
    ///
    /// Implementations should apply their own deduplication; callers are
    /// responsible for running the quality gate before calling this.
    async fn capture(&self, exchange: &Exchange);
}

// ---------------------------------------------------------------------------
// Quality gate
// ---------------------------------------------------------------------------

/// Noise prefixes that indicate git-generated output.
const GIT_NOISE_PREFIXES: &[&str] = &[
    "commit ",
    "diff --git",
    "index ",
    "--- a/",
    "+++ b/",
    "Author:",
    "Date:  ",
];

/// Returns `true` when the content is worth storing as a memory.
///
/// Rejects:
/// - Content shorter than [`MIN_CONTENT_LEN`] characters.
/// - The literal string `"HEARTBEAT_OK"`.
/// - Git-generated noise (commit headers, diff output, etc.).
pub fn passes_quality_gate(content: &str) -> bool {
    let trimmed = content.trim();

    // Reject empty / too-short content.
    if trimmed.len() < MIN_CONTENT_LEN {
        return false;
    }

    // Reject heartbeat signals.
    if trimmed == "HEARTBEAT_OK" {
        return false;
    }

    // Reject git noise: if the majority of lines start with known git prefixes.
    let lines: Vec<&str> = trimmed.lines().collect();
    if !lines.is_empty() {
        let git_lines = lines
            .iter()
            .filter(|l| GIT_NOISE_PREFIXES.iter().any(|p| l.starts_with(p)))
            .count();
        // More than half the lines look like git output → reject.
        if git_lines * 2 > lines.len() {
            return false;
        }
    }

    true
}

// ---------------------------------------------------------------------------
// Category detection
// ---------------------------------------------------------------------------

/// Detect the most appropriate [`MemoryCategory`] from content signals.
///
/// Uses simple keyword heuristics; a production implementation would use
/// the model itself to classify.
pub fn detect_category(content: &str) -> MemoryCategory {
    let lower = content.to_lowercase();

    // Preference signals.
    let preference_signals = [
        "i prefer",
        "i always",
        "i never",
        "i like",
        "i dislike",
        "i want",
        "i don't want",
        "i do not want",
        "please always",
        "please never",
        "use this",
        "avoid ",
    ];
    if preference_signals.iter().any(|s| lower.contains(s)) {
        return MemoryCategory::Preference;
    }

    // Decision signals.
    let decision_signals = [
        "decided",
        "decision",
        "we will use",
        "we're using",
        "we are using",
        "chosen",
        "selected",
        "going with",
        "agreed",
        "settled on",
        "will use",
    ];
    if decision_signals.iter().any(|s| lower.contains(s)) {
        return MemoryCategory::Decision;
    }

    // Entity signals (names, projects, organisations).
    let entity_signals = [
        "project ",
        "repository ",
        "repo ",
        "crate ",
        "library ",
        "service ",
        "team ",
        "organisation ",
        "organization ",
        "company ",
    ];
    if entity_signals.iter().any(|s| lower.contains(s)) {
        return MemoryCategory::Entity;
    }

    MemoryCategory::Other
}

// ---------------------------------------------------------------------------
// Format helpers
// ---------------------------------------------------------------------------

/// Format a slice of recalled memories as a context block that fits within
/// `budget_chars` characters.
///
/// Returns an empty string when `memories` is empty.
pub fn format_context(memories: &[Memory], budget_chars: usize) -> String {
    if memories.is_empty() {
        return String::new();
    }

    let mut out = String::from("## Relevant context from memory\n\n");
    let mut remaining = budget_chars.saturating_sub(out.len());

    for m in memories {
        let entry = format!("- {}\n", m.content);
        if entry.len() > remaining {
            break;
        }
        out.push_str(&entry);
        remaining -= entry.len();
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- quality gate ---

    #[test]
    fn passes_quality_gate_rejects_short_content() {
        assert!(!passes_quality_gate("hi"));
        assert!(!passes_quality_gate(""));
        assert!(!passes_quality_gate("   "));
    }

    #[test]
    fn passes_quality_gate_rejects_heartbeat() {
        assert!(!passes_quality_gate("HEARTBEAT_OK"));
        assert!(!passes_quality_gate("  HEARTBEAT_OK  "));
    }

    #[test]
    fn passes_quality_gate_rejects_git_noise() {
        let git_output = "commit abc123\nAuthor: Alice\nDate:   Mon Jan 1 00:00:00 2026\n\n    feat: something";
        assert!(!passes_quality_gate(git_output));
    }

    #[test]
    fn passes_quality_gate_accepts_normal_content() {
        assert!(passes_quality_gate(
            "I prefer to use Rust for systems programming projects."
        ));
        assert!(passes_quality_gate(
            "We decided to store configuration in PluresDB state."
        ));
    }

    // --- category detection ---

    #[test]
    fn detect_category_preference() {
        assert_eq!(
            detect_category("I prefer to use Rust for all backend work."),
            MemoryCategory::Preference
        );
        assert_eq!(
            detect_category("Please always format code before committing."),
            MemoryCategory::Preference
        );
    }

    #[test]
    fn detect_category_decision() {
        assert_eq!(
            detect_category("We decided to use PluresDB as the primary data store."),
            MemoryCategory::Decision
        );
        assert_eq!(
            detect_category("Going with Tokio for the async runtime."),
            MemoryCategory::Decision
        );
    }

    #[test]
    fn detect_category_entity() {
        assert_eq!(
            detect_category("The project pares-agens is the AI agent framework."),
            MemoryCategory::Entity
        );
        assert_eq!(
            detect_category("The team is working on the core crate first."),
            MemoryCategory::Entity
        );
    }

    #[test]
    fn detect_category_other() {
        assert_eq!(
            detect_category("The event loop polls PluresDB for new events every second."),
            MemoryCategory::Other
        );
    }

    // --- format_context ---

    #[test]
    fn format_context_empty_returns_empty_string() {
        assert_eq!(format_context(&[], 1000), "");
    }

    #[test]
    fn format_context_includes_memories() {
        let memories = vec![Memory {
            id: "1".into(),
            content: "I prefer Rust for all backend work.".into(),
            category: MemoryCategory::Preference,
            score: 0.9,
        }];
        let ctx = format_context(&memories, 1000);
        assert!(ctx.contains("I prefer Rust for all backend work."));
        assert!(ctx.contains("## Relevant context from memory"));
    }

    #[test]
    fn format_context_respects_budget() {
        let memories: Vec<Memory> = (0..20)
            .map(|i| Memory {
                id: i.to_string(),
                content: format!("Memory entry number {} with some extra content padding.", i),
                category: MemoryCategory::Other,
                score: 0.5,
            })
            .collect();
        // Tiny budget — should include far fewer than 20 entries.
        let ctx = format_context(&memories, 100);
        assert!(ctx.len() <= 100);
    }
}
