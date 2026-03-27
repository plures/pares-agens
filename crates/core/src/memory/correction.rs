//! Correction detection and learning engine.
//!
//! When a user corrects the agent ("don't do that", "I prefer X"), the
//! correction is detected, persisted as a high-confidence memory, and
//! optionally compiled into a praxis constraint so the behaviour change is
//! durable.
//!
//! # Flow
//!
//! 1. [`is_correction`] — heuristic check on the user message.
//! 2. [`CorrectionEngine::apply`] — stores the correction as a
//!    [`MemoryCategory::Correction`] entry with max confidence and generates a
//!    [`CorrectionRecord`] that the caller can use to mutate praxis state and
//!    inject guidance.
//! 3. [`CorrectionEngine::undo`] — reverts a previously applied correction by
//!    removing both the memory entry and any associated praxis constraint.

use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    entry::{Exchange, MemoryCategory, MemoryEntry},
    store::MemoryStore,
    Error,
};

// ---------------------------------------------------------------------------
// Correction detection
// ---------------------------------------------------------------------------

/// Phrases that strongly signal user corrections.
///
/// Each entry must be specific enough to avoid matching normal questions and
/// statements.  Broad substrings like standalone `"never"` or `"wrong"` are
/// intentionally excluded because they produce false positives (e.g.
/// "I **never** used tokio before", "**wrong** type inference happens when…").
const CORRECTION_SIGNALS: &[&str] = &[
    // ── explicit "don't / do not" directives ─────────────────────────────
    "don't do",
    "dont do",
    "do not do",
    "stop doing",
    "don't use",
    "dont use",
    "do not use",
    // ── "never" + verb (avoids matching "I never used X before") ─────────
    "never do ",
    "never use ",
    "never again",
    // ── preference / directive ────────────────────────────────────────────
    "i prefer",
    "i'd prefer",
    "id prefer",
    "please use",
    "use instead",
    "instead of",
    "rather than",
    "not like that",
    "always use",
    "i want you to",
    // ── explicit wrongness indicators (require context) ──────────────────
    "that's incorrect",
    "thats incorrect",
    "that is incorrect",
    "that's wrong",
    "thats wrong",
    "that is wrong",
    "you're wrong",
    "youre wrong",
    "you are wrong",
    // ── conversational correction openers ─────────────────────────────────
    "i said",
    "i told you",
    "i already said",
    "remember to",
    "don't forget",
    "dont forget",
    // ── temporal / going-forward directives ───────────────────────────────
    "from now on",
    "going forward",
    "in the future",
    "change that to",
    "switch to",
];

/// Patterns that must appear at the **start** of the message to count as a
/// correction signal.  These are too broad as substring matches but are strong
/// signals when they open the sentence.
const CORRECTION_PREFIXES: &[&str] = &["no, ", "no. ", "actually,", "actually, "];

/// Return `true` when the user message looks like a correction rather than a
/// new request.
///
/// Uses keyword-based heuristics. In production this would be replaced (or
/// augmented) by an LLM classifier.
pub fn is_correction(user_message: &str) -> bool {
    let lower = user_message.to_lowercase();
    // Substring matches for specific multi-word phrases.
    if CORRECTION_SIGNALS.iter().any(|sig| lower.contains(sig)) {
        return true;
    }
    // Prefix-only matches for short tokens that are too broad as substrings.
    CORRECTION_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

// ---------------------------------------------------------------------------
// CorrectionRecord
// ---------------------------------------------------------------------------

/// Tag prefix used to persist the constraint ID inside a memory entry.
///
/// During [`CorrectionEngine::apply`] the constraint ID (if any) is stored as
/// a tag of the form `"constraint_id:<id>"`.  [`CorrectionEngine::undo`] reads
/// this tag back so it can return the constraint ID to the caller without
/// requiring the original [`CorrectionRecord`] to be kept around.
const CONSTRAINT_TAG_PREFIX: &str = "constraint_id:";

/// A record of an applied correction.
///
/// Returned by [`CorrectionEngine::apply`] so callers can track what changed
/// and, if needed, undo the correction later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrectionRecord {
    /// Unique correction ID (same as the memory entry ID).
    pub id: String,
    /// The original user message that triggered the correction.
    pub user_message: String,
    /// A short summary of the rule inferred from the correction.
    pub rule_summary: String,
    /// The constraint ID inserted into the praxis store (if any).
    pub constraint_id: Option<String>,
    /// Confirmation message to show the user.
    pub confirmation: String,
    /// Timestamp when the correction was applied.
    pub applied_at: String,
}

/// Outcome of [`CorrectionEngine::undo`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoOutcome {
    /// `true` if the memory entry was found and removed.
    pub removed: bool,
    /// The constraint ID that was associated with this correction (if any).
    ///
    /// The caller should use this to also remove the constraint from the
    /// praxis store and remove the associated guidance entry.
    pub constraint_id: Option<String>,
}

// ---------------------------------------------------------------------------
// CorrectionEngine
// ---------------------------------------------------------------------------

/// Ties together correction detection, memory persistence, and confirmation
/// generation.
///
/// This engine is deliberately store-agnostic with respect to the praxis
/// backend: it returns a [`CorrectionRecord`] containing all the data the
/// caller needs to mutate the praxis store and inject guidance.  This avoids
/// coupling the memory crate to the praxis crate directly.
pub struct CorrectionEngine {
    store: Arc<dyn MemoryStore>,
}

impl CorrectionEngine {
    /// Create a new correction engine backed by `store`.
    pub fn new(store: Arc<dyn MemoryStore>) -> Self {
        Self { store }
    }

    /// Apply a user correction.
    ///
    /// 1. Stores a [`MemoryCategory::Correction`] entry with maximum
    ///    confidence (`score = 1.0`) and a `"decay_protected"` tag.
    /// 2. Derives a concise rule summary and confirmation message.
    /// 3. Returns a [`CorrectionRecord`] the caller can use to mutate praxis
    ///    state and inject guidance.
    ///
    /// The optional `constraint_id` is provided when the caller intends to
    /// compile the correction into a praxis constraint (via
    /// [`pares_agens_praxis::db::procedures::compile_nl`]).
    ///
    /// # Errors
    /// Propagates store errors.
    pub async fn apply(
        &self,
        exchange: &Exchange,
        constraint_id: Option<String>,
    ) -> Result<CorrectionRecord, Error> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();

        let rule_summary = derive_rule_summary(&exchange.user);
        let confirmation = format!("Got it, I'll remember to {} going forward.", rule_summary);

        let content = format!(
            "CORRECTION: {}\nContext: {}",
            exchange.user, exchange.assistant
        );

        let mut tags = vec!["decay_protected".to_string(), "correction".to_string()];
        if let Some(ref cid) = constraint_id {
            tags.push(format!("{CONSTRAINT_TAG_PREFIX}{cid}"));
        }

        let entry = MemoryEntry {
            id: id.clone(),
            content,
            category: MemoryCategory::Correction,
            tags,
            // Zero-vector placeholder; the caller should embed before insert
            // if a real embedding provider is available.  For the memory
            // store layer it is fine to store a placeholder.
            embedding: vec![],
            // Corrections are always stored at max confidence so they rank
            // highest during recall and never lose relevance over time.
            score: 1.0,
            created_at: now.clone(),
        };

        self.store
            .insert(entry)
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        Ok(CorrectionRecord {
            id,
            user_message: exchange.user.clone(),
            rule_summary,
            constraint_id,
            confirmation,
            applied_at: now,
        })
    }

    /// Undo a previously applied correction.
    ///
    /// Looks up the memory entry for `correction_id`, extracts the persisted
    /// constraint ID (if any), then removes the entry.  Returns an
    /// [`UndoOutcome`] so the caller can also clean up the corresponding
    /// praxis constraint and guidance entry without needing to keep the
    /// original [`CorrectionRecord`] around.
    ///
    /// # Errors
    /// Propagates store errors.
    pub async fn undo(&self, correction_id: &str) -> Result<UndoOutcome, Error> {
        // Look up the entry to extract the constraint_id tag before removal.
        let constraint_id = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?
            .iter()
            .find(|e| e.id == correction_id)
            .and_then(|e| {
                e.tags
                    .iter()
                    .find_map(|t| t.strip_prefix(CONSTRAINT_TAG_PREFIX).map(|s| s.to_string()))
            });

        let removed = self
            .store
            .remove(correction_id)
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        Ok(UndoOutcome {
            removed,
            constraint_id,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Derive a short rule summary from the user's correction message.
///
/// This is a best-effort heuristic.  In production the LLM would generate a
/// more precise summary.
fn derive_rule_summary(user_message: &str) -> String {
    let lower = user_message.to_lowercase();

    // "don't / do not / never" → extract what to avoid
    for prefix in &["don't ", "dont ", "do not ", "never ", "stop "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_end_matches(|c: char| c.is_ascii_punctuation());
            if !rest.is_empty() {
                return format!("avoid: {rest}");
            }
        }
    }

    // "I prefer X" / "please use X" / "always use X"
    for prefix in &[
        "i prefer ",
        "please use ",
        "always use ",
        "use ",
        "switch to ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_end_matches(|c: char| c.is_ascii_punctuation());
            if !rest.is_empty() {
                return format!("prefer: {rest}");
            }
        }
    }

    // "from now on X" / "going forward X" / "in the future X"
    for prefix in &["from now on ", "going forward ", "in the future "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let rest = rest.trim_end_matches(|c: char| c.is_ascii_punctuation());
            if !rest.is_empty() {
                return format!("rule: {rest}");
            }
        }
    }

    // Fallback: use the original message (trimmed).
    let trimmed = user_message.trim();
    if trimmed.len() > 80 {
        // Find a safe UTF-8 boundary at or before byte position 77.
        let end = trimmed
            .char_indices()
            .take_while(|(i, _)| *i < 77)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(77);
        format!("follow user correction: {}…", &trimmed[..end])
    } else {
        format!("follow user correction: {trimmed}")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::store::InMemoryStore;

    // ── is_correction ────────────────────────────────────────────────────────

    #[test]
    fn detects_dont_corrections() {
        assert!(is_correction("Don't use tabs, use spaces"));
        assert!(is_correction("don't do that again"));
        assert!(is_correction("do not use println! in production code"));
    }

    #[test]
    fn detects_preference_corrections() {
        assert!(is_correction("I prefer snake_case for variables"));
        assert!(is_correction("please use async/await instead of threads"));
        assert!(is_correction("always use Result instead of unwrap"));
    }

    #[test]
    fn detects_temporal_corrections() {
        assert!(is_correction("From now on, format code with rustfmt"));
        assert!(is_correction("going forward use clippy"));
        assert!(is_correction("in the future, add doc comments"));
    }

    #[test]
    fn detects_negation_corrections() {
        assert!(is_correction("No, that's wrong"));
        assert!(is_correction("Actually, it should be a Vec not a slice"));
        assert!(is_correction(
            "That's incorrect, the function returns Option"
        ));
    }

    #[test]
    fn rejects_normal_requests() {
        assert!(!is_correction("How do I write async Rust?"));
        assert!(!is_correction("Show me an example of pattern matching"));
        assert!(!is_correction("What does the borrow checker do?"));
    }

    #[test]
    fn rejects_sentences_with_broad_keywords() {
        // "never" as part of normal description, not a directive
        assert!(!is_correction("I never used tokio before"));
        assert!(!is_correction("This has never been an issue until now"));
        // "wrong" as part of normal description, not a correction
        assert!(!is_correction(
            "wrong type inference happens when lifetimes are elided"
        ));
        assert!(!is_correction("What went wrong with the build?"));
        // "actually" mid-sentence, not a correction opener
        assert!(!is_correction("I actually want to learn about traits"));
        assert!(!is_correction("Can you actually explain that?"));
    }

    // ── derive_rule_summary ──────────────────────────────────────────────────

    #[test]
    fn summary_from_dont() {
        let s = derive_rule_summary("don't use unwrap in production");
        assert_eq!(s, "avoid: use unwrap in production");
    }

    #[test]
    fn summary_from_prefer() {
        let s = derive_rule_summary("I prefer spaces over tabs");
        assert_eq!(s, "prefer: spaces over tabs");
    }

    #[test]
    fn summary_from_temporal() {
        let s = derive_rule_summary("from now on add doc comments to all public functions");
        assert_eq!(s, "rule: add doc comments to all public functions");
    }

    #[test]
    fn summary_fallback() {
        let s = derive_rule_summary("That is wrong, fix it");
        assert!(s.starts_with("follow user correction:"));
    }

    // ── CorrectionEngine ─────────────────────────────────────────────────────

    fn test_store() -> Arc<dyn MemoryStore> {
        Arc::new(InMemoryStore::new())
    }

    #[tokio::test]
    async fn apply_stores_correction_memory() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));

        let exchange = Exchange {
            user: "Don't use unwrap in production code".to_string(),
            assistant: "I used unwrap() here for simplicity.".to_string(),
        };

        let record = engine.apply(&exchange, None).await.unwrap();

        // Verify the record
        assert!(!record.id.is_empty());
        assert_eq!(record.user_message, exchange.user);
        assert!(record.confirmation.contains("going forward"));
        assert!(record.rule_summary.contains("avoid"));

        // Verify the entry was stored
        let all = store.all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, MemoryCategory::Correction);
        assert!(all[0].tags.contains(&"decay_protected".to_string()));
        assert_eq!(all[0].score, 1.0);
    }

    #[tokio::test]
    async fn apply_with_constraint_id() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));

        let exchange = Exchange {
            user: "Always use Result instead of unwrap".to_string(),
            assistant: "Ok.".to_string(),
        };

        let record = engine
            .apply(&exchange, Some("C-CORR-001".to_string()))
            .await
            .unwrap();
        assert_eq!(record.constraint_id, Some("C-CORR-001".to_string()));

        // The constraint_id must also be persisted as a tag in the entry.
        let entries = store.all().await.unwrap();
        assert!(entries[0]
            .tags
            .contains(&"constraint_id:C-CORR-001".to_string()));
    }

    #[tokio::test]
    async fn undo_removes_correction() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));

        let exchange = Exchange {
            user: "Don't use tabs".to_string(),
            assistant: "Using tabs for indentation.".to_string(),
        };

        let record = engine.apply(&exchange, None).await.unwrap();
        assert_eq!(store.all().await.unwrap().len(), 1);

        let outcome = engine.undo(&record.id).await.unwrap();
        assert!(outcome.removed);
        assert_eq!(outcome.constraint_id, None);
        assert!(store.all().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn undo_returns_constraint_id() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));

        let exchange = Exchange {
            user: "Don't use unwrap".to_string(),
            assistant: "Ok.".to_string(),
        };

        let record = engine
            .apply(&exchange, Some("C-CORR-99".to_string()))
            .await
            .unwrap();

        let outcome = engine.undo(&record.id).await.unwrap();
        assert!(outcome.removed);
        assert_eq!(outcome.constraint_id, Some("C-CORR-99".to_string()));
    }

    #[tokio::test]
    async fn undo_nonexistent_returns_not_removed() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));
        let outcome = engine.undo("nonexistent-id").await.unwrap();
        assert!(!outcome.removed);
        assert_eq!(outcome.constraint_id, None);
    }

    #[tokio::test]
    async fn correction_has_decay_protection() {
        let store = test_store();
        let engine = CorrectionEngine::new(Arc::clone(&store));

        let exchange = Exchange {
            user: "I prefer spaces over tabs always".to_string(),
            assistant: "Noted.".to_string(),
        };

        engine.apply(&exchange, None).await.unwrap();

        let entries = store.all().await.unwrap();
        let entry = &entries[0];
        // Decay protection: high score + tag
        assert_eq!(entry.score, 1.0, "corrections must have max confidence");
        assert!(
            entry.tags.contains(&"decay_protected".to_string()),
            "corrections must be tagged for decay protection"
        );
    }
}
