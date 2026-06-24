//! Built-in procedure pipelines for the cerebellum.
//!
//! These are the standard procedures that ship with pares-radix:
//!
//! - **autorecall** — retrieve + compress memories before agent execution
//! - **primitive-extract** — extract typed primitives (decisions, facts, risks)
//!   from a conversation exchange
//! - **cerebellum-sweep** — periodic background maintenance (prune stale,
//!   consolidate duplicates, update ledger)

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use pares_radix_core::event::Event;
use crate::memory::PluresLm;
use pares_radix_core::procedure::Procedure;

// ── stop-word list ────────────────────────────────────────────────────────────

const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "must", "shall",
    "can", "to", "of", "in", "on", "at", "by", "for", "with", "about", "as", "into", "through",
    "during", "before", "after", "above", "below", "from", "up", "down", "out", "off", "over",
    "under", "again", "further", "then", "once", "i", "me", "my", "we", "our", "you", "your", "he",
    "him", "his", "she", "her", "it", "its", "they", "them", "their", "this", "that", "these",
    "those", "and", "but", "or", "not", "no", "so", "if", "how", "what", "which", "who", "when",
    "where", "why",
];

/// Extract key terms from `text` by tokenizing on non-alphanumeric characters,
/// removing short tokens, and filtering common stop-words.
///
/// Returns a sorted, deduplicated list of lowercase terms.
fn extract_key_terms(text: &str) -> Vec<String> {
    let unique_terms: HashSet<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect();
    let mut terms: Vec<String> = unique_terms.into_iter().collect();
    terms.sort();
    terms
}

// ── Primitive types ───────────────────────────────────────────────────────────

/// Structured primitives that can be extracted from free-form conversation text.
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    /// A named entity mentioned in conversation.
    Entity {
        /// The entity's name or label.
        name: String,
        /// The entity type (e.g. `"person"`, `"project"`, `"tool"`).
        kind: String,
    },
    /// A decision that was made or recorded.
    Decision {
        /// Short description of the decision.
        text: String,
        /// Surrounding sentence or paragraph providing context.
        context: String,
    },
    /// A stated preference (e.g. "I prefer tabs").
    Preference {
        /// The preference statement.
        text: String,
    },
    /// A simple subject–predicate–object fact.
    Fact {
        /// Entity the fact is about.
        subject: String,
        /// Relationship or attribute connecting subject and object.
        predicate: String,
        /// Value or entity the predicate points to.
        object: String,
    },
}

/// Find `needle` in `haystack` in a case-insensitive way.
///
/// Returns the byte index into `haystack` where the match starts.
/// The index is always on a UTF-8 boundary because it is derived from
/// `char_indices`.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }

    let needle_chars: Vec<char> = needle.chars().collect();
    let _needle_len = needle_chars.len();

    for (start, _) in haystack.char_indices() {
        let mut h_iter = haystack[start..].chars();
        let mut matched = true;

        for &n_ch in &needle_chars {
            match h_iter.next() {
                Some(h_ch) => {
                    // Prefixes are ASCII; eq_ignore_ascii_case is sufficient here
                    if !h_ch.eq_ignore_ascii_case(&n_ch) {
                        matched = false;
                        break;
                    }
                }
                None => {
                    return None;
                }
            }
        }

        if matched {
            return Some(start);
        }
    }

    None
}

/// Pattern-based extraction of primitives from free-form text.
///
/// Detects decisions, preferences, and simple subject-predicate-object facts
/// using keyword prefixes. LLM-based extraction is a future enhancement.
fn extract_primitives(text: &str) -> Vec<Primitive> {
    let lower = text.to_lowercase();
    let mut primitives = Vec::new();

    // Decisions — "we decided to X", "decided to X", "going with X", etc.
    for prefix in &[
        "we decided to ",
        "decided to ",
        "going with ",
        "we chose ",
        "chose ",
    ] {
        if let Some(pos) = find_case_insensitive(text, prefix) {
            let rest = &text[pos + prefix.len()..];
            let end = rest.find(['.', '!', '?', '\n']).unwrap_or(rest.len());
            let decision_text = rest[..end].trim().to_string();
            if !decision_text.is_empty() {
                primitives.push(Primitive::Decision {
                    text: decision_text,
                    context: text.to_string(),
                });
            }
        }
    }

    // Preferences — "I prefer X", "always use X", "never use X"
    for prefix in &["i prefer ", "always use ", "never use ", "prefer to use "] {
        if let Some(pos) = lower.find(prefix) {
            let rest = &text[pos + prefix.len()..];
            let end = rest.find(['.', '!', '?', '\n']).unwrap_or(rest.len());
            let pref_text = rest[..end].trim().to_string();
            if !pref_text.is_empty() {
                primitives.push(Primitive::Preference { text: pref_text });
            }
        }
    }

    // Facts — simple "X is Y" / "X are Y" (subject ≤ 4 words)
    let words: Vec<&str> = text.split_whitespace().collect();
    for i in 1..words.len().saturating_sub(1) {
        let word_lower = words[i].to_lowercase();
        if word_lower == "is" || word_lower == "are" {
            let subject = words[..i].join(" ");
            let object = words[i + 1..]
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            if !subject.is_empty() && !object.is_empty() && subject.split_whitespace().count() <= 4
            {
                primitives.push(Primitive::Fact {
                    subject,
                    predicate: words[i].to_string(),
                    object,
                });
            }
        }
    }

    primitives
}

/// Sanitize a string for use in an [`Event::StateChange`] key path.
///
/// Replaces any character that is not alphanumeric, a hyphen, or an underscore
/// with an underscore to prevent malformed key paths.
fn sanitize_key_segment(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Convert a [`Primitive`] to a `(key, value)` pair for [`Event::StateChange`].
fn primitive_to_state_change(primitive: &Primitive) -> (String, serde_json::Value) {
    match primitive {
        Primitive::Entity { name, kind } => (
            format!("primitive.entity.{}", sanitize_key_segment(name)),
            serde_json::json!({ "kind": kind }),
        ),
        Primitive::Decision { text, context } => (
            "primitive.decision".into(),
            serde_json::json!({ "text": text, "context": context }),
        ),
        Primitive::Preference { text } => (
            "primitive.preference".into(),
            serde_json::json!({ "text": text }),
        ),
        Primitive::Fact {
            subject,
            predicate,
            object,
        } => (
            format!("primitive.fact.{}", sanitize_key_segment(subject)),
            serde_json::json!({ "predicate": predicate, "object": object }),
        ),
    }
}

// ── cosine similarity helper ──────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Find groups of entries whose embeddings have cosine similarity ≥ `threshold`
/// and that belong to the same category.
///
/// Returns each group as a list of entry IDs (only groups with ≥ 2 members).
fn find_duplicate_groups(
    entries: &[crate::memory::entry::MemoryEntry],
    threshold: f32,
) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut grouped: HashSet<usize> = HashSet::new();

    for i in 0..entries.len() {
        if grouped.contains(&i) {
            continue;
        }
        let mut group = vec![entries[i].id.clone()];
        for j in (i + 1)..entries.len() {
            if grouped.contains(&j) {
                continue;
            }
            // Same category and high embedding similarity → duplicate
            if entries[i].category == entries[j].category {
                let sim = cosine_similarity(&entries[i].embedding, &entries[j].embedding);
                if sim >= threshold {
                    group.push(entries[j].id.clone());
                    grouped.insert(j);
                }
            }
        }
        if group.len() > 1 {
            grouped.insert(i);
            groups.push(group);
        }
    }

    groups
}

// ── autorecall ───────────────────────────────────────────────────────────────

/// Autorecall procedure — retrieves and compresses memories into learned
/// context before the conscious agent runs.
///
/// Pipeline:
/// 1. Extract key terms from the message content (tokenisation + stop-word removal).
/// 2. Run `Memory::recall()` with those terms.
/// 3. Apply the configured token budget via `inject_context`.
/// 4. Emit a `StateChange` event with the compressed context string as value.
pub struct Autorecall {
    memory: Arc<PluresLm>,
    top_k: usize,
    token_budget: usize,
}

impl Autorecall {
    /// Create an `Autorecall` procedure using the given `PluresLm` instance and
    /// tuning parameters from `config`.
    pub fn new(memory: Arc<PluresLm>, config: &super::CerebellumConfig) -> Self {
        Self {
            memory,
            top_k: config.recall_limit,
            token_budget: config.context_token_budget,
        }
    }
}

#[async_trait]
impl Procedure for Autorecall {
    fn name(&self) -> &str {
        "autorecall"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let content = match event {
            Event::Message { content, .. } => content.clone(),
            _ => return vec![],
        };

        let terms = extract_key_terms(&content);
        if terms.is_empty() {
            debug!("autorecall: no key terms extracted, skipping");
            return vec![];
        }

        let query = terms.join(" ");
        debug!(query, top_k = self.top_k, "autorecall: recalling memories");

        let memories = match self.memory.recall(&query, self.top_k, &[]).await {
            Ok(m) => m,
            Err(e) => {
                debug!(error = %e, "autorecall: recall failed");
                return vec![];
            }
        };

        if memories.is_empty() {
            debug!("autorecall: no memories found");
            return vec![];
        }

        let context = self
            .memory
            .inject_context(&memories, Some(self.token_budget));
        debug!(context_len = context.len(), "autorecall: context assembled");

        vec![Event::StateChange {
            key: "autorecall.context".into(),
            old_value: None,
            new_value: serde_json::json!(context),
        }]
    }
}

// ── primitive extraction ─────────────────────────────────────────────────────

/// Primitive extraction procedure — runs on message events to extract typed
/// primitives (decisions, facts, preferences) from conversation content.
///
/// Initial implementation uses regex/keyword patterns. LLM-based extraction
/// is a future enhancement.
pub struct PrimitiveExtract {
    /// Reserved for future LLM-based extraction; not used in the initial
    /// pattern-matching implementation.
    _memory: Arc<PluresLm>,
}

impl PrimitiveExtract {
    /// Create a `PrimitiveExtract` procedure backed by `memory`.
    pub fn new(memory: Arc<PluresLm>) -> Self {
        Self { _memory: memory }
    }
}

#[async_trait]
impl Procedure for PrimitiveExtract {
    fn name(&self) -> &str {
        "primitive-extract"
    }

    fn handles(&self) -> &str {
        "message"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        let content = match event {
            Event::Message { content, .. } => content.clone(),
            _ => return vec![],
        };

        debug!(
            content_len = content.len(),
            "primitive-extract: scanning for primitives"
        );

        let primitives = extract_primitives(&content);
        if primitives.is_empty() {
            debug!("primitive-extract: no primitives found");
            return vec![];
        }

        debug!(
            count = primitives.len(),
            "primitive-extract: found primitives"
        );

        // Emit one StateChange event per extracted primitive.
        primitives
            .iter()
            .map(|p| {
                let (key, value) = primitive_to_state_change(p);
                Event::StateChange {
                    key,
                    old_value: None,
                    new_value: value,
                }
            })
            .collect()
    }
}

// ── cerebellum sweep ─────────────────────────────────────────────────────────

/// Periodic maintenance sweep — runs on timer events.
///
/// Tasks:
/// - Detect stale memories (created more than `staleness_days` ago).
/// - Detect near-duplicate memories (same category + cosine similarity ≥
///   `similarity_threshold`).
///
/// Results are reported as `StateChange` events so that other procedures or
/// the application layer can act on them (e.g. prune, merge).
pub struct CerebellumSweep {
    memory: Arc<PluresLm>,
    staleness_days: u32,
    similarity_threshold: f32,
}

impl CerebellumSweep {
    /// Create a `CerebellumSweep` procedure using `config` for staleness and
    /// similarity thresholds.
    pub fn new(memory: Arc<PluresLm>, config: &super::CerebellumConfig) -> Self {
        Self {
            memory,
            staleness_days: config.staleness_days,
            similarity_threshold: config.similarity_threshold,
        }
    }
}

#[async_trait]
impl Procedure for CerebellumSweep {
    fn name(&self) -> &str {
        "cerebellum-sweep"
    }

    fn handles(&self) -> &str {
        "timer"
    }

    async fn execute(&self, event: &Event) -> Vec<Event> {
        match event {
            Event::Timer { .. } => {}
            _ => return vec![],
        }

        debug!(
            staleness_days = self.staleness_days,
            "cerebellum-sweep: starting"
        );

        let all_entries = match self.memory.scan_all().await {
            Ok(entries) => entries,
            Err(e) => {
                debug!(error = %e, "cerebellum-sweep: failed to scan memory");
                return vec![];
            }
        };

        if all_entries.is_empty() {
            debug!("cerebellum-sweep: no entries to sweep");
            return vec![];
        }

        let now = chrono::Utc::now();
        let staleness_cutoff = chrono::Duration::days(i64::from(self.staleness_days));
        let mut events = Vec::new();

        // Identify stale entries.
        let stale_ids: Vec<String> = all_entries
            .iter()
            .filter(
                |entry| match chrono::DateTime::parse_from_rfc3339(&entry.created_at) {
                    Ok(created) => {
                        now.signed_duration_since(created.with_timezone(&chrono::Utc))
                            > staleness_cutoff
                    }
                    Err(e) => {
                        debug!(
                            id = entry.id,
                            created_at = entry.created_at,
                            error = %e,
                            "cerebellum-sweep: skipping entry with unparseable timestamp"
                        );
                        false
                    }
                },
            )
            .map(|entry| entry.id.clone())
            .collect();

        if !stale_ids.is_empty() {
            debug!(
                count = stale_ids.len(),
                "cerebellum-sweep: found stale entries"
            );
            events.push(Event::StateChange {
                key: "cerebellum.sweep.stale".into(),
                old_value: None,
                new_value: serde_json::json!({ "ids": stale_ids }),
            });
        }

        // Detect duplicate groups.
        let duplicate_groups = find_duplicate_groups(&all_entries, self.similarity_threshold);
        if !duplicate_groups.is_empty() {
            debug!(
                groups = duplicate_groups.len(),
                "cerebellum-sweep: found duplicate groups"
            );
            events.push(Event::StateChange {
                key: "cerebellum.sweep.duplicates".into(),
                old_value: None,
                new_value: serde_json::json!({ "groups": duplicate_groups }),
            });
        }

        debug!(events = events.len(), "cerebellum-sweep: sweep complete");
        events
    }
}

/// Register all built-in cerebellum procedures into a registry.
#[cfg(test)]
pub(crate) fn register_builtins(
    registry: &mut pares_radix_core::procedure::ProcedureRegistry,
    memory: Arc<PluresLm>,
    config: &super::CerebellumConfig,
) {
    // Cerebellum itself handles messages first (lowest priority number = runs first)
    registry.register(Box::new(super::CerebellumProcedure::stub()));
    registry.set_priority("cerebellum", -200);

    // Autorecall runs next, injecting learned context
    registry.register(Box::new(Autorecall::new(memory.clone(), config)));
    registry.set_priority("autorecall", -100);

    // Primitive extraction runs on message events after autorecall
    registry.register(Box::new(PrimitiveExtract::new(memory.clone())));
    registry.set_priority("primitive-extract", 0);

    // Sweep runs on timers
    registry.register(Box::new(CerebellumSweep::new(memory, config)));
    registry.set_priority("cerebellum-sweep", 0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cerebellum::CerebellumConfig;
    use crate::memory::{
        embed::MockEmbedder,
        entry::{Exchange, MemoryCategory, MemoryEntry},
        store::{InMemoryStore, MemoryStore},
        PluresLm,
    };
    use pares_radix_core::procedure::ProcedureRegistry;
    use std::sync::Arc;

    fn test_memory() -> Arc<PluresLm> {
        Arc::new(PluresLm::new(
            Arc::new(InMemoryStore::new()),
            Box::new(MockEmbedder),
            128_000,
        ))
    }

    fn test_config() -> CerebellumConfig {
        CerebellumConfig::default()
    }

    // ── registration ─────────────────────────────────────────────────────────

    #[test]
    fn register_builtins_adds_four_procedures() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry, test_memory(), &test_config());
        assert_eq!(registry.len(), 4);
    }

    #[test]
    fn cerebellum_has_highest_priority_for_messages() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry, test_memory(), &test_config());

        let message_procs: Vec<&str> = registry.matching("message").map(|p| p.name()).collect();

        // cerebellum (-200) → autorecall (-100) → primitive-extract (0)
        assert_eq!(
            message_procs,
            vec!["cerebellum", "autorecall", "primitive-extract"]
        );
    }

    #[test]
    fn sweep_handles_timer_events() {
        let mut registry = ProcedureRegistry::new();
        register_builtins(&mut registry, test_memory(), &test_config());

        let timer_procs: Vec<&str> = registry.matching("timer").map(|p| p.name()).collect();

        assert_eq!(timer_procs, vec!["cerebellum-sweep"]);
    }

    // ── key-term extraction ───────────────────────────────────────────────────

    #[test]
    fn extract_key_terms_removes_stopwords() {
        let terms = extract_key_terms("How does the autorecall work");
        assert!(!terms.contains(&"how".to_string()));
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"does".to_string()));
        assert!(terms.contains(&"autorecall".to_string()));
        assert!(terms.contains(&"work".to_string()));
    }

    #[test]
    fn extract_key_terms_deduplicates() {
        let terms = extract_key_terms("rust rust rust memory memory");
        assert_eq!(terms.iter().filter(|t| *t == "rust").count(), 1);
        assert_eq!(terms.iter().filter(|t| *t == "memory").count(), 1);
    }

    #[test]
    fn extract_key_terms_short_tokens_excluded() {
        let terms = extract_key_terms("a to in the go");
        assert!(terms.is_empty());
    }

    // ── primitive extraction ──────────────────────────────────────────────────

    #[test]
    fn extract_primitives_finds_decisions() {
        let primitives = extract_primitives("We decided to use Rust for this project.");
        assert!(primitives.iter().any(|p| matches!(
            p,
            Primitive::Decision { text, .. } if text.contains("Rust")
        )));
    }

    #[test]
    fn extract_primitives_finds_preferences_i_prefer() {
        let primitives = extract_primitives("I prefer using snake_case for all identifiers.");
        assert!(primitives.iter().any(|p| matches!(
            p,
            Primitive::Preference { text } if text.contains("snake_case")
        )));
    }

    #[test]
    fn extract_primitives_finds_preferences_always_use() {
        let primitives = extract_primitives("Always use async/await for IO operations.");
        assert!(primitives
            .iter()
            .any(|p| matches!(p, Primitive::Preference { .. })));
    }

    #[test]
    fn extract_primitives_returns_no_decisions_or_prefs_for_plain_text() {
        let primitives = extract_primitives("The weather is nice today.");
        assert!(!primitives
            .iter()
            .any(|p| matches!(p, Primitive::Decision { .. })));
        assert!(!primitives
            .iter()
            .any(|p| matches!(p, Primitive::Preference { .. })));
    }

    // ── autorecall ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn autorecall_returns_empty_for_non_message_event() {
        let proc = Autorecall::new(test_memory(), &test_config());
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        assert!(proc.execute(&event).await.is_empty());
    }

    #[tokio::test]
    async fn autorecall_returns_empty_when_no_memories() {
        let proc = Autorecall::new(test_memory(), &test_config());
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "How does autorecall work?".into(),
        };
        assert!(
            proc.execute(&event).await.is_empty(),
            "empty store → no context to inject"
        );
    }

    #[tokio::test]
    async fn autorecall_returns_state_change_with_context() {
        let memory = test_memory();
        memory
            .capture(&Exchange {
                user: "What is autorecall?".into(),
                assistant:
                    "Autorecall retrieves relevant memories and compresses them into context."
                        .into(),
            })
            .await
            .unwrap();

        let proc = Autorecall::new(memory, &test_config());
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "Tell me about autorecall memory retrieval".into(),
        };
        let results = proc.execute(&event).await;
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0],
            Event::StateChange { key, .. } if key == "autorecall.context"
        ));
    }

    #[tokio::test]
    async fn autorecall_respects_token_budget() {
        let memory = test_memory();
        for i in 0..5usize {
            memory
                .capture(&Exchange {
                    user: format!(
                        "memory item {i}: longer content about topic {i} for testing budget enforcement"
                    ),
                    assistant: format!(
                        "detailed reply about topic {i} with explanation that fills token budget"
                    ),
                })
                .await
                .unwrap();
        }

        let mut config = test_config();
        config.context_token_budget = 50; // 50 tokens ≈ 200 chars

        let proc = Autorecall::new(memory, &config);
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "memory item topic content budget".into(),
        };
        let results = proc.execute(&event).await;
        if let Some(Event::StateChange { new_value, .. }) = results.first() {
            let ctx = new_value.as_str().unwrap_or("");
            // 50 tokens * 4 chars/token = 200 chars maximum
            assert!(
                ctx.len() <= 200,
                "context exceeded token budget: len={}",
                ctx.len()
            );
        }
    }

    // ── primitive-extract ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn primitive_extract_returns_empty_for_non_message() {
        let proc = PrimitiveExtract::new(test_memory());
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        assert!(proc.execute(&event).await.is_empty());
    }

    #[tokio::test]
    async fn primitive_extract_finds_decision_in_message() {
        let proc = PrimitiveExtract::new(test_memory());
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "We decided to use tokio for async runtime.".into(),
        };
        let results = proc.execute(&event).await;
        assert!(!results.is_empty());
        assert!(results.iter().any(|e| matches!(
            e,
            Event::StateChange { key, .. } if key == "primitive.decision"
        )));
    }

    #[tokio::test]
    async fn primitive_extract_finds_preference_in_message() {
        let proc = PrimitiveExtract::new(test_memory());
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "I prefer using rustfmt for all formatting.".into(),
        };
        let results = proc.execute(&event).await;
        assert!(results.iter().any(|e| matches!(
            e,
            Event::StateChange { key, .. } if key == "primitive.preference"
        )));
    }

    // ── cerebellum-sweep ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn sweep_returns_empty_for_non_timer() {
        let proc = CerebellumSweep::new(test_memory(), &test_config());
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "hello".into(),
        };
        assert!(proc.execute(&event).await.is_empty());
    }

    #[tokio::test]
    async fn sweep_returns_empty_for_empty_memory() {
        let proc = CerebellumSweep::new(test_memory(), &test_config());
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        assert!(proc.execute(&event).await.is_empty());
    }

    #[tokio::test]
    async fn sweep_detects_stale_entries() {
        let store = InMemoryStore::new();
        let stale_entry = MemoryEntry {
            id: "stale-1".into(),
            content: "old memory from 2020".into(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![0.1; 384],
            score: 0.0,
            created_at: "2020-01-01T00:00:00Z".into(),
        };
        store.insert(stale_entry).await.unwrap();

        let memory = Arc::new(PluresLm::new(
            Arc::new(store),
            Box::new(MockEmbedder),
            128_000,
        ));
        let mut config = test_config();
        config.staleness_days = 30;

        let proc = CerebellumSweep::new(memory, &config);
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        let results = proc.execute(&event).await;

        let stale_event = results.iter().find(|e| {
            matches!(
                e,
                Event::StateChange { key, .. } if key == "cerebellum.sweep.stale"
            )
        });
        assert!(stale_event.is_some(), "sweep should detect the stale entry");

        if let Some(Event::StateChange { new_value, .. }) = stale_event {
            let ids = new_value["ids"].as_array().unwrap();
            assert!(ids.iter().any(|id| id.as_str() == Some("stale-1")));
        }
    }

    #[tokio::test]
    async fn sweep_detects_duplicate_groups() {
        let store = InMemoryStore::new();
        let recent = chrono::Utc::now().to_rfc3339();
        // Unit vector — both entries have identical embeddings.
        let unit = vec![1.0f32 / (384.0_f32.sqrt()); 384];

        let entry1 = MemoryEntry {
            id: "dup-1".into(),
            content: "memory about rust async".into(),
            category: MemoryCategory::CodePattern,
            tags: vec![],
            embedding: unit.clone(),
            score: 0.0,
            created_at: recent.clone(),
        };
        let entry2 = MemoryEntry {
            id: "dup-2".into(),
            content: "memory about rust async (duplicate)".into(),
            category: MemoryCategory::CodePattern,
            tags: vec![],
            embedding: unit,
            score: 0.0,
            created_at: recent,
        };
        store.insert(entry1).await.unwrap();
        store.insert(entry2).await.unwrap();

        let memory = Arc::new(PluresLm::new(
            Arc::new(store),
            Box::new(MockEmbedder),
            128_000,
        ));
        let mut config = test_config();
        config.similarity_threshold = 0.85;

        let proc = CerebellumSweep::new(memory, &config);
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        let results = proc.execute(&event).await;

        assert!(
            results.iter().any(|e| matches!(
                e,
                Event::StateChange { key, .. } if key == "cerebellum.sweep.duplicates"
            )),
            "sweep should detect duplicate group"
        );
    }

    // ── utility function unit tests ───────────────────────────────────────────

    #[test]
    fn find_case_insensitive_basic_match() {
        assert_eq!(find_case_insensitive("Hello World", "hello"), Some(0));
        assert_eq!(find_case_insensitive("Hello World", "WORLD"), Some(6));
        assert_eq!(find_case_insensitive("Hello World", "xyz"), None);
    }

    #[test]
    fn find_case_insensitive_empty_needle() {
        assert_eq!(find_case_insensitive("anything", ""), Some(0));
    }

    #[test]
    fn find_case_insensitive_needle_longer_than_haystack() {
        assert_eq!(find_case_insensitive("hi", "hello world"), None);
    }

    #[test]
    fn find_case_insensitive_mid_string() {
        assert_eq!(find_case_insensitive("abc DEF ghi", "def"), Some(4));
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim + 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
        assert_eq!(cosine_similarity(&b, &a), 0.0);
    }

    #[test]
    fn sanitize_key_segment_keeps_alphanumeric() {
        assert_eq!(sanitize_key_segment("hello-world_123"), "hello-world_123");
    }

    #[test]
    fn sanitize_key_segment_replaces_special_chars() {
        assert_eq!(sanitize_key_segment("foo.bar/baz"), "foo_bar_baz");
        assert_eq!(sanitize_key_segment("a b c"), "a_b_c");
    }

    #[test]
    fn sanitize_key_segment_empty_input() {
        assert_eq!(sanitize_key_segment(""), "");
    }

    #[test]
    fn primitive_to_state_change_decision() {
        let p = Primitive::Decision {
            text: "use rust".into(),
            context: "we decided to use rust".into(),
        };
        let (key, value) = primitive_to_state_change(&p);
        assert_eq!(key, "primitive.decision");
        assert_eq!(value["text"], "use rust");
        assert_eq!(value["context"], "we decided to use rust");
    }

    #[test]
    fn primitive_to_state_change_preference() {
        let p = Primitive::Preference {
            text: "snake_case".into(),
        };
        let (key, value) = primitive_to_state_change(&p);
        assert_eq!(key, "primitive.preference");
        assert_eq!(value["text"], "snake_case");
    }

    #[test]
    fn primitive_to_state_change_entity() {
        let p = Primitive::Entity {
            name: "rust lang".into(),
            kind: "language".into(),
        };
        let (key, value) = primitive_to_state_change(&p);
        assert_eq!(key, "primitive.entity.rust_lang");
        assert_eq!(value["kind"], "language");
    }

    #[test]
    fn primitive_to_state_change_fact() {
        let p = Primitive::Fact {
            subject: "rust".into(),
            predicate: "is".into(),
            object: "fast".into(),
        };
        let (key, value) = primitive_to_state_change(&p);
        assert_eq!(key, "primitive.fact.rust");
        assert_eq!(value["predicate"], "is");
        assert_eq!(value["object"], "fast");
    }

    #[test]
    fn find_duplicate_groups_no_duplicates() {
        let entries = vec![
            MemoryEntry {
                id: "a".into(),
                content: "x".into(),
                category: MemoryCategory::Conversation,
                tags: vec![],
                embedding: vec![1.0, 0.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            MemoryEntry {
                id: "b".into(),
                content: "y".into(),
                category: MemoryCategory::Conversation,
                tags: vec![],
                embedding: vec![0.0, 1.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];
        let groups = find_duplicate_groups(&entries, 0.9);
        assert!(groups.is_empty());
    }

    #[test]
    fn find_duplicate_groups_identical_entries() {
        let entries = vec![
            MemoryEntry {
                id: "a".into(),
                content: "x".into(),
                category: MemoryCategory::CodePattern,
                tags: vec![],
                embedding: vec![1.0, 0.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            MemoryEntry {
                id: "b".into(),
                content: "y".into(),
                category: MemoryCategory::CodePattern,
                tags: vec![],
                embedding: vec![1.0, 0.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];
        let groups = find_duplicate_groups(&entries, 0.9);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
        assert!(groups[0].contains(&"a".to_string()));
        assert!(groups[0].contains(&"b".to_string()));
    }

    #[test]
    fn find_duplicate_groups_different_categories_not_grouped() {
        let entries = vec![
            MemoryEntry {
                id: "a".into(),
                content: "x".into(),
                category: MemoryCategory::Conversation,
                tags: vec![],
                embedding: vec![1.0, 0.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            MemoryEntry {
                id: "b".into(),
                content: "y".into(),
                category: MemoryCategory::CodePattern,
                tags: vec![],
                embedding: vec![1.0, 0.0, 0.0],
                score: 0.0,
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];
        let groups = find_duplicate_groups(&entries, 0.9);
        assert!(groups.is_empty(), "different categories should not group");
    }

    #[test]
    fn find_duplicate_groups_empty_entries() {
        let groups = find_duplicate_groups(&[], 0.9);
        assert!(groups.is_empty());
    }

    #[test]
    fn extract_primitives_finds_fact_is() {
        let prims = extract_primitives("Rust is a systems language.");
        assert!(prims.iter().any(|p| matches!(
            p,
            Primitive::Fact { subject, predicate, object }
            if subject == "Rust" && predicate == "is" && object.contains("systems")
        )));
    }

    #[test]
    fn extract_primitives_finds_fact_are() {
        let prims = extract_primitives("Cats are great companions.");
        assert!(prims.iter().any(|p| matches!(
            p,
            Primitive::Fact { predicate, .. } if predicate == "are"
        )));
    }

    #[test]
    fn extract_primitives_no_fact_for_long_subject() {
        // Subject > 4 words should not produce a fact
        let prims = extract_primitives("The very long subject phrase here is important.");
        let facts: Vec<_> = prims
            .iter()
            .filter(|p| matches!(p, Primitive::Fact { .. }))
            .collect();
        // Any fact extracted must have subject ≤ 4 words
        for f in &facts {
            if let Primitive::Fact { subject, .. } = f {
                assert!(subject.split_whitespace().count() <= 4);
            }
        }
    }

    #[test]
    fn extract_primitives_never_use_prefix() {
        let prims = extract_primitives("Never use global state in production.");
        assert!(prims.iter().any(|p| matches!(
            p,
            Primitive::Preference { text } if text.contains("global")
        )));
    }

    #[test]
    fn extract_primitives_going_with_decision() {
        let prims = extract_primitives("Going with tokio for the runtime.");
        assert!(prims.iter().any(|p| matches!(
            p,
            Primitive::Decision { text, .. } if text.contains("tokio")
        )));
    }

    #[test]
    fn extract_primitives_we_chose_decision() {
        let prims = extract_primitives("We chose PostgreSQL as our database.");
        assert!(prims.iter().any(|p| matches!(
            p,
            Primitive::Decision { text, .. } if text.contains("PostgreSQL")
        )));
    }

    // ── sweep boundary: entry exactly at staleness cutoff must NOT be swept ──

    #[tokio::test]
    async fn sweep_does_not_sweep_entry_exactly_at_boundary() {
        let store = InMemoryStore::new();
        // Create an entry whose age is exactly the staleness cutoff.
        // The sweep's `now` will be a few microseconds later than our `now`,
        // making the entry's actual age = 30d + epsilon. With `>` this is swept,
        // so we test with an entry that is 30d - 5 seconds old (clearly under).
        // Then we verify it's NOT swept.
        // A companion test below creates a 30d + 5s entry that IS swept.
        // Together they prove strict `>` (not `>=`) at the boundary.
        let now = chrono::Utc::now();
        let just_under_boundary = now - chrono::Duration::days(30) + chrono::Duration::seconds(5);
        let boundary_entry = MemoryEntry {
            id: "under-boundary".into(),
            content: "entry just under boundary".into(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![0.1; 384],
            score: 0.0,
            created_at: just_under_boundary.to_rfc3339(),
        };
        store.insert(boundary_entry).await.unwrap();

        let memory = Arc::new(PluresLm::new(
            Arc::new(store),
            Box::new(MockEmbedder),
            128_000,
        ));
        let mut config = test_config();
        config.staleness_days = 30;

        let proc = CerebellumSweep::new(memory, &config);
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        let results = proc.execute(&event).await;

        let stale_event = results.iter().find(|e| {
            matches!(
                e,
                Event::StateChange { key, .. } if key == "cerebellum.sweep.stale"
            )
        });
        assert!(
            stale_event.is_none(),
            "entry 5s under staleness boundary should NOT be swept"
        );
    }

    #[tokio::test]
    async fn sweep_does_sweep_entry_just_over_boundary() {
        let store = InMemoryStore::new();
        let now = chrono::Utc::now();
        // Entry is 30 days + 5 seconds old — clearly over the boundary
        let just_over_boundary = now - chrono::Duration::days(30) - chrono::Duration::seconds(5);
        let entry = MemoryEntry {
            id: "over-boundary".into(),
            content: "entry just over boundary".into(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![0.1; 384],
            score: 0.0,
            created_at: just_over_boundary.to_rfc3339(),
        };
        store.insert(entry).await.unwrap();

        let memory = Arc::new(PluresLm::new(
            Arc::new(store),
            Box::new(MockEmbedder),
            128_000,
        ));
        let mut config = test_config();
        config.staleness_days = 30;

        let proc = CerebellumSweep::new(memory, &config);
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        let results = proc.execute(&event).await;

        let stale_event = results.iter().find(|e| {
            matches!(
                e,
                Event::StateChange { key, .. } if key == "cerebellum.sweep.stale"
            )
        });
        assert!(
            stale_event.is_some(),
            "entry 5s over staleness boundary should be swept"
        );
        if let Some(Event::StateChange { new_value, .. }) = stale_event {
            let ids = new_value["ids"].as_array().unwrap();
            assert!(ids.iter().any(|id| id.as_str() == Some("over-boundary")));
        }
    }

    // ── mutation-gap tests: verify exact text extraction (not just contains) ──

    #[test]
    fn decision_text_excludes_prefix_decided_to() {
        let prims = extract_primitives("We decided to use Rust for this project.");
        let decision = prims.iter().find_map(|p| match p {
            Primitive::Decision { text, .. } => Some(text.clone()),
            _ => None,
        });
        let text = decision.unwrap();
        // Must NOT contain the trigger prefix
        assert!(!text.starts_with("decided to "));
        assert!(!text.starts_with("We decided to "));
        // Must start with the content AFTER the prefix
        assert!(text.starts_with("use Rust"));
    }

    #[test]
    fn preference_text_excludes_prefix_i_prefer() {
        let prims = extract_primitives("I prefer using snake_case for all identifiers.");
        let pref = prims.iter().find_map(|p| match p {
            Primitive::Preference { text } => Some(text.clone()),
            _ => None,
        });
        let text = pref.unwrap();
        // Must NOT contain the trigger prefix
        assert!(!text.to_lowercase().starts_with("i prefer "));
        assert!(!text.to_lowercase().starts_with("prefer "));
        // Must start with content after prefix
        assert!(
            text.starts_with("using snake_case") || text.starts_with("snake_case"),
            "got: {:?}",
            text
        );
    }

    #[test]
    fn preference_text_excludes_prefix_always_use() {
        let prims = extract_primitives("Always use async/await for IO operations.");
        let pref = prims.iter().find_map(|p| match p {
            Primitive::Preference { text } => Some(text.clone()),
            _ => None,
        });
        let text = pref.unwrap();
        assert!(!text.to_lowercase().starts_with("always use "));
        assert!(text.starts_with("async/await"), "got: {:?}", text);
    }

    #[test]
    fn fact_object_excludes_predicate_word() {
        // "Rust is a systems language." → subject="Rust", predicate="is", object starts with "a"
        let prims = extract_primitives("Rust is a systems language.");
        let fact = prims.iter().find_map(|p| match p {
            Primitive::Fact {
                subject,
                predicate,
                object,
            } if subject == "Rust" => Some((predicate.clone(), object.clone())),
            _ => None,
        });
        let (pred, obj) = fact.unwrap();
        assert_eq!(pred, "is");
        // Object must NOT start with the predicate itself (catches i+1 → i*1 mutation)
        assert!(
            !obj.starts_with("is "),
            "object should not include predicate: {:?}",
            obj
        );
        assert!(obj.starts_with("a systems"), "got: {:?}", obj);
    }

    #[test]
    fn fact_object_does_not_include_subject_tail() {
        // Catches i+1 → i-1 mutation which would include subject's last word in object
        let prims = extract_primitives("The dog is friendly and cute.");
        let fact = prims.iter().find_map(|p| match p {
            Primitive::Fact {
                subject, object, ..
            } if subject == "The dog" => Some(object.clone()),
            _ => None,
        });
        let obj = fact.unwrap();
        assert!(
            !obj.contains("dog"),
            "object must not contain subject words: {:?}",
            obj
        );
        assert!(obj.starts_with("friendly"), "got: {:?}", obj);
    }

    #[test]
    fn decision_going_with_excludes_prefix() {
        let prims = extract_primitives("Going with tokio for the runtime.");
        let text = prims
            .iter()
            .find_map(|p| match p {
                Primitive::Decision { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(!text.to_lowercase().starts_with("going with "));
        assert!(text.starts_with("tokio"), "got: {:?}", text);
    }

    #[test]
    fn decision_we_chose_excludes_prefix() {
        let prims = extract_primitives("We chose PostgreSQL as our database.");
        let text = prims
            .iter()
            .find_map(|p| match p {
                Primitive::Decision { text, .. } => Some(text.clone()),
                _ => None,
            })
            .unwrap();
        assert!(!text.to_lowercase().starts_with("we chose "));
        assert!(!text.to_lowercase().starts_with("chose "));
        assert!(text.starts_with("PostgreSQL"), "got: {:?}", text);
    }
}
