//! PluresLM — native memory operations for Pares Radix.
//!
//! Provides three high-level operations:
//!
//! - [`PluresLm::recall`] — vector search with category filtering
//! - [`PluresLm::capture`] — quality-gated extraction and storage from a conversation exchange
//! - [`PluresLm::inject_context`] — format memories for model prompt with budget enforcement

/// Correction detection and learning engine.
pub mod correction;
/// Embedding provider trait and mock implementation.
pub mod embed;
/// Memory entry data structures and category taxonomy.
pub mod entry;
/// Controlled forgetting — retention policies, purge engine, and simulation drills.
pub mod forgetting;
/// Quality gate helpers for filtering low-signal content.
pub mod quality;
/// Memory store trait and backend implementations.
pub mod store;

use std::{path::Path, sync::Arc};

use tracing::{debug, info, warn};
use uuid::Uuid;

use self::{
    embed::EmbeddingProvider,
    entry::{Exchange, MemoryCategory, MemoryEntry},
    store::MemoryStore,
};

/// Error type for memory operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The embedding provider failed to produce a vector for the input text.
    #[error("embedding failed: {0}")]
    Embed(String),
    /// The backing memory store returned an error.
    #[error("store operation failed: {0}")]
    Store(String),
    /// Filesystem operation failed while ingesting documents.
    #[error("io operation failed: {0}")]
    Io(String),
}

/// PluresLM memory system — native (non-MCP) memory operations for Pares Radix.
///
/// Wraps a [`MemoryStore`] and [`EmbeddingProvider`] to provide recall, capture,
/// and context-injection without going through an MCP server hop.
///
/// # Example
///
/// ```rust,no_run
/// # use std::sync::Arc;
/// # use pares_agens_core::memory::{PluresLm, embed::MockEmbedder, entry::Exchange, store::InMemoryStore};
/// # #[tokio::main] async fn main() {
/// let lm = PluresLm::new(Arc::new(InMemoryStore::new()), Box::new(MockEmbedder), 128_000);
/// let ids = lm.capture(&Exchange { user: "What is Rust?".into(), assistant: "A systems language.".into() }).await.unwrap();
/// let mems = lm.recall("Rust language systems", 5, &[]).await.unwrap();
/// let ctx  = lm.inject_context(&mems, None);
/// # }
/// ```
pub struct PluresLm {
    store: Arc<dyn MemoryStore>,
    embedder: Box<dyn EmbeddingProvider>,
    /// Model context window in tokens (e.g. 128 000 for Qwen3-235B).
    context_window: usize,
}

impl PluresLm {
    /// Create a new `PluresLm` instance.
    ///
    /// `context_window` is the model's maximum context length in **tokens**.
    /// [`inject_context`][Self::inject_context] enforces a 25 % budget of this value.
    ///
    /// Accepts any [`Arc<dyn MemoryStore>`] so the same backing store can be
    /// shared between the agent and the application state (e.g. `AppState`).
    pub fn new(
        store: Arc<dyn MemoryStore>,
        embedder: Box<dyn EmbeddingProvider>,
        context_window: usize,
    ) -> Self {
        Self {
            store,
            embedder,
            context_window,
        }
    }

    /// Embed arbitrary text using the configured embedding provider.
    ///
    /// Useful for higher-level orchestration logic (e.g. topic-shift detection)
    /// that needs vector similarity without performing a full recall operation.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>, Error> {
        self.embedder
            .embed(text)
            .await
            .map_err(|e| Error::Embed(e.to_string()))
    }

    /// Recall the most relevant memories for `query`.
    ///
    /// Returns up to `limit` entries sorted by **descending cosine similarity**,
    /// skipping any entry whose category appears in `exclude_categories`.
    ///
    /// # Errors
    /// Propagates embedding and store errors.
    pub async fn recall(
        &self,
        query: &str,
        limit: usize,
        exclude_categories: &[MemoryCategory],
    ) -> Result<Vec<MemoryEntry>, Error> {
        let query_emb = self
            .embedder
            .embed(query)
            .await
            .map_err(|e| Error::Embed(e.to_string()))?;

        let all = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        debug!(total = all.len(), query, "scoring memories");

        let mut scored: Vec<(f32, MemoryEntry)> = all
            .into_iter()
            .filter(|m| !exclude_categories.contains(&m.category))
            .map(|m| {
                let sim = cosine_similarity(&query_emb, &m.embedding);
                (sim, m)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        Ok(scored
            .into_iter()
            .map(|(score, mut m)| {
                m.score = score;
                m
            })
            .collect())
    }

    /// Extract and store a memory from a conversation exchange.
    ///
    /// The quality gate rejects:
    /// - Content shorter than [`quality::MIN_CONTENT_LEN`] characters
    /// - Pure noise phrases (acknowledgements, greetings)
    /// - Near-duplicate echoes of already-stored memories (cosine ≥ [`quality::ECHO_THRESHOLD`])
    ///
    /// Returns the IDs of newly stored memories (empty if rejected by the gate).
    ///
    /// # Errors
    /// Propagates embedding and store errors.
    pub async fn capture(&self, exchange: &Exchange) -> Result<Vec<String>, Error> {
        // Check raw exchange text for noise (before prepending labels).
        let raw = format!("{} {}", exchange.user, exchange.assistant);
        if quality::is_noise(&raw) {
            debug!("capture rejected: noise");
            return Ok(vec![]);
        }

        let content = format_exchange(exchange);

        let embedding = self
            .embedder
            .embed(&content)
            .await
            .map_err(|e| Error::Embed(e.to_string()))?;

        let all = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        let refs: Vec<&MemoryEntry> = all.iter().collect();
        if quality::is_echo(&embedding, &refs) {
            debug!("capture rejected: echo");
            return Ok(vec![]);
        }

        let category = detect_category(&content);
        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        info!(id, ?category, "capturing memory");

        let entry = MemoryEntry {
            id: id.clone(),
            content,
            category,
            tags: extract_tags(exchange),
            embedding,
            score: 0.0,
            created_at,
        };

        self.store
            .insert(entry)
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        Ok(vec![id])
    }

    /// Store a single factual statement as a memory entry.
    pub async fn capture_fact(
        &self,
        fact: &str,
        tags: Vec<String>,
    ) -> Result<Option<String>, Error> {
        if !passes_quality_gate(fact) {
            debug!("capture_fact rejected: quality gate");
            return Ok(None);
        }

        let embedding = self
            .embedder
            .embed(fact)
            .await
            .map_err(|e| Error::Embed(e.to_string()))?;

        let all = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        let refs: Vec<&MemoryEntry> = all.iter().collect();
        if quality::is_echo(&embedding, &refs) {
            debug!("capture_fact rejected: echo");
            return Ok(None);
        }

        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        let entry = MemoryEntry {
            id: id.clone(),
            content: fact.to_string(),
            category: MemoryCategory::Fact,
            tags,
            embedding,
            score: 0.0,
            created_at,
        };

        self.store
            .insert(entry)
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        Ok(Some(id))
    }

    /// Store a procedure candidate derived from a conversation.
    pub async fn capture_procedure_candidate(
        &self,
        description: &str,
        tags: Vec<String>,
    ) -> Result<Option<String>, Error> {
        if !passes_quality_gate(description) {
            debug!("capture_procedure_candidate rejected: quality gate");
            return Ok(None);
        }

        let embedding = self
            .embedder
            .embed(description)
            .await
            .map_err(|e| Error::Embed(e.to_string()))?;

        let all = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        let refs: Vec<&MemoryEntry> = all.iter().collect();
        if quality::is_echo(&embedding, &refs) {
            debug!("capture_procedure_candidate rejected: echo");
            return Ok(None);
        }

        let id = Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339();

        let mut tagged = tags;
        tagged.push("procedure:candidate".into());

        let entry = MemoryEntry {
            id: id.clone(),
            content: description.to_string(),
            category: MemoryCategory::Procedure,
            tags: tagged,
            embedding,
            score: 0.0,
            created_at,
        };

        self.store
            .insert(entry)
            .await
            .map_err(|e| Error::Store(e.to_string()))?;

        Ok(Some(id))
    }

    /// Ingest a file or directory of supported documents into memory.
    ///
    /// Supported file types:
    /// - Markdown (`.md`, `.markdown`)
    /// - Text (`.txt`, `.text`)
    /// - Source code files (common language/config extensions)
    ///
    /// Returns the number of chunks indexed.
    pub async fn ingest_documents_path(&self, path: impl AsRef<Path>) -> Result<usize, Error> {
        let path = path.as_ref();
        let metadata = tokio::fs::metadata(path).await.map_err(|e| {
            Error::Io(format!(
                "failed to read metadata for {}: {e}",
                path.display()
            ))
        })?;

        if metadata.is_dir() {
            self.ingest_documents_dir(path).await
        } else if metadata.is_file() {
            self.ingest_document_file(path).await
        } else {
            Ok(0)
        }
    }

    async fn ingest_documents_dir(&self, root: &Path) -> Result<usize, Error> {
        let mut indexed = 0usize;
        let mut stack = vec![root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&dir).await.map_err(|e| {
                Error::Io(format!("failed to read directory {}: {e}", dir.display()))
            })?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| Error::Io(format!("failed to read directory entry: {e}")))?
            {
                let file_type = entry
                    .file_type()
                    .await
                    .map_err(|e| Error::Io(format!("failed to get file type: {e}")))?;
                let entry_path = entry.path();

                if file_type.is_dir() {
                    stack.push(entry_path);
                } else if file_type.is_file() {
                    indexed += self.ingest_document_file(&entry_path).await?;
                }
            }
        }

        Ok(indexed)
    }

    async fn ingest_document_file(&self, path: &Path) -> Result<usize, Error> {
        let Some(kind) = classify_document_kind(path) else {
            return Ok(0);
        };

        let canonical_path = match tokio::fs::canonicalize(path).await {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to canonicalize document path; using provided path"
                );
                path.to_path_buf()
            }
        };
        let source = canonical_path.to_string_lossy().to_string();
        self.remove_existing_document_chunks(&source).await?;

        let raw = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| Error::Io(format!("failed to read file {}: {e}", path.display())))?;
        let chunks = split_document_chunks(
            &raw,
            DOCUMENT_CHUNK_SIZE_CHARS,
            DOCUMENT_CHUNK_OVERLAP_CHARS,
        );
        if chunks.is_empty() {
            return Ok(0);
        }

        let total_chunks = chunks.len();
        for (idx, chunk) in chunks.into_iter().enumerate() {
            let content = format_document_chunk_content(&source, idx + 1, total_chunks, &chunk);
            let embedding = self
                .embedder
                .embed(&content)
                .await
                .map_err(|e| Error::Embed(e.to_string()))?;

            let entry = MemoryEntry {
                id: Uuid::new_v4().to_string(),
                content,
                category: kind.category(),
                tags: vec![
                    format!("source:{source}"),
                    format!("source-kind:{}", kind.as_str()),
                    format!("chunk:{}/{}", idx + 1, total_chunks),
                ],
                embedding,
                score: 0.0,
                created_at: chrono::Utc::now().to_rfc3339(),
            };

            self.store
                .insert(entry)
                .await
                .map_err(|e| Error::Store(e.to_string()))?;
        }

        Ok(total_chunks)
    }

    async fn remove_existing_document_chunks(&self, source: &str) -> Result<(), Error> {
        let source_tag = format!("source:{source}");
        let all = self
            .store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))?;
        for entry in all {
            if entry.tags.iter().any(|tag| tag == &source_tag) {
                self.store
                    .remove(&entry.id)
                    .await
                    .map_err(|e| Error::Store(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Return all stored memory entries (unordered).
    ///
    /// Used by maintenance procedures such as `cerebellum-sweep` that need to
    /// inspect the full memory store without a specific query.
    ///
    /// # Errors
    /// Propagates store errors.
    pub async fn scan_all(&self) -> Result<Vec<MemoryEntry>, Error> {
        self.store
            .all()
            .await
            .map_err(|e| Error::Store(e.to_string()))
    }

    /// Format `memories` as a Markdown block for injection into the model prompt.
    ///
    /// `budget` overrides the default token budget (25 % of `context_window`).
    /// Approximately 4 characters per token is used for the conversion.
    ///
    /// Memories are included in order; truncation stops when the budget is exhausted.
    pub fn inject_context(&self, memories: &[MemoryEntry], budget: Option<usize>) -> String {
        let max_tokens = budget.unwrap_or(self.context_window / 4);
        let max_chars = max_tokens.saturating_mul(4);

        let header = "# Relevant memories\n\n";
        let mut out = String::with_capacity(header.len() + memories.len() * 80);
        out.push_str(header);

        for (i, m) in memories.iter().enumerate() {
            let block = format!("{}. [{}] {}\n", i + 1, m.category.as_str(), m.content);
            if out.len() + block.len() > max_chars {
                break;
            }
            out.push_str(&block);
        }

        out
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

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

/// Combine user and assistant turns into a single storable string.
fn format_exchange(exchange: &Exchange) -> String {
    format!("User: {}\nAssistant: {}", exchange.user, exchange.assistant)
}

/// Heuristic category detection based on content keywords.
pub fn detect_category(text: &str) -> MemoryCategory {
    let lower = text.to_lowercase();

    // Derive a user-only segment for correction detection so assistant phrasing
    // can't falsely trigger a Correction category.
    let user_segment = if let Some(user_idx) = lower.find("user:") {
        let after_user = &lower[user_idx + "user:".len()..];
        if let Some(assistant_idx) = after_user.find("\nassistant:") {
            after_user[..assistant_idx].trim()
        } else {
            after_user.trim()
        }
    } else {
        lower.as_str()
    };

    // Check for correction signals first (highest priority), using only the user text.
    if correction::is_correction(user_segment) {
        return MemoryCategory::Correction;
    }

    if lower.contains("error")
        || lower.contains("fix")
        || lower.contains("bug")
        || lower.contains("panic")
    {
        MemoryCategory::ErrorFix
    } else if lower.contains("fn ")
        || lower.contains("impl ")
        || lower.contains("struct ")
        || lower.contains("cargo")
        || lower.contains("crate")
        || lower.contains("trait ")
    {
        MemoryCategory::CodePattern
    } else if lower.contains("prefer")
        || lower.contains("always use")
        || lower.contains("never use")
        || lower.contains("convention")
    {
        MemoryCategory::Preference
    } else if lower.contains("decided")
        || lower.contains("decision")
        || lower.contains("chose")
        || lower.contains("because")
    {
        MemoryCategory::Decision
    } else {
        MemoryCategory::Conversation
    }
}

/// Return true if `content` passes the basic quality gate.
///
/// Rejects noise, heartbeat pings, and obvious git output.
pub fn passes_quality_gate(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.eq_ignore_ascii_case("HEARTBEAT_OK") {
        return false;
    }
    if quality::is_noise(trimmed) {
        return false;
    }
    if is_git_noise(trimmed) {
        return false;
    }
    true
}

fn is_git_noise(text: &str) -> bool {
    let lower = text.to_lowercase();
    (lower.contains("commit ") && lower.contains("author:") && lower.contains("date:"))
        || lower.contains("diff --git")
        || lower.contains("index ") && lower.contains("+++")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentKind {
    Markdown,
    Text,
    SourceCode,
}

impl DocumentKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Markdown => "markdown",
            Self::Text => "text",
            Self::SourceCode => "source-code",
        }
    }

    fn category(self) -> MemoryCategory {
        match self {
            Self::SourceCode => MemoryCategory::CodePattern,
            Self::Markdown | Self::Text => MemoryCategory::Fact,
        }
    }
}

const DOCUMENT_CHUNK_SIZE_CHARS: usize = 1_200;
const DOCUMENT_CHUNK_OVERLAP_CHARS: usize = 200;

fn classify_document_kind(path: &Path) -> Option<DocumentKind> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => Some(DocumentKind::Markdown),
        "txt" | "text" => Some(DocumentKind::Text),
        "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go" | "java" | "c" | "cc" | "cpp" | "h"
        | "hpp" | "cs" | "swift" | "kt" | "kts" | "rb" | "php" | "scala" | "sh" | "bash"
        | "zsh" | "fish" | "sql" | "toml" | "json" | "yaml" | "yml" => {
            Some(DocumentKind::SourceCode)
        }
        _ => None,
    }
}

fn floor_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn split_document_chunks(text: &str, max_chars: usize, overlap_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = text.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return out;
    }

    let mut start = 0usize;
    let len = trimmed.len();
    while start < len {
        let mut end = (start + max_chars).min(len);
        end = floor_char_boundary(trimmed, end);
        if end <= start {
            break;
        }

        let chunk = trimmed[start..end].trim();
        if !chunk.is_empty() {
            out.push(chunk.to_string());
        }

        if end == len {
            break;
        }

        let mut next_start = end.saturating_sub(overlap_chars);
        next_start = floor_char_boundary(trimmed, next_start);
        if next_start <= start {
            next_start = end;
        }
        start = next_start;
    }

    out
}

fn format_document_chunk_content(
    source: &str,
    chunk_index: usize,
    total_chunks: usize,
    chunk: &str,
) -> String {
    format!("Source: {source}\nChunk: {chunk_index}/{total_chunks}\n\n{chunk}")
}

/// Extract simple keyword tags from the exchange.
fn extract_tags(exchange: &Exchange) -> Vec<String> {
    let combined = format!("{} {}", exchange.user, exchange.assistant).to_lowercase();
    let mut tags = Vec::new();

    // Programming language hints
    for lang in &["rust", "python", "typescript", "javascript", "go"] {
        if combined.contains(lang) {
            tags.push(format!("lang:{lang}"));
        }
    }
    // Tool hints
    for tool in &["cargo", "tokio", "serde", "git", "docker"] {
        if combined.contains(tool) {
            tags.push(format!("tool:{tool}"));
        }
    }

    tags
}

// Compatibility re-exports (from original memory.rs)

// Handler-facing memory interface types (`Memory`, `MemoryCapture`,
// `MemoryClient`) moved to the platform crate in Stage S2a so the built-in
// `OnMessage` handler can depend on them without a cognition edge. Re-exported
// here so existing `pares_agens_core::memory::{...}` paths keep resolving.
pub use pares_radix_core::memory_client::{Memory, MemoryCapture, MemoryClient};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{embed::MockEmbedder, store::InMemoryStore};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn lm() -> PluresLm {
        PluresLm::new(
            Arc::new(InMemoryStore::new()),
            Box::new(MockEmbedder),
            128_000,
        )
    }

    #[tokio::test]
    async fn capture_rejects_noise() {
        let lm = lm();
        let ids = lm
            .capture(&Exchange {
                user: "ok".into(),
                assistant: "Sure.".into(),
            })
            .await
            .unwrap();
        assert!(ids.is_empty(), "noise should be rejected");
    }

    #[tokio::test]
    async fn capture_stores_quality_exchange() {
        let lm = lm();
        let ids = lm
            .capture(&Exchange {
                user: "How do I write async Rust?".into(),
                assistant: "Use `async fn` and `.await` on futures. Add tokio to Cargo.toml."
                    .into(),
            })
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
    }

    #[tokio::test]
    async fn capture_rejects_echo() {
        let lm = lm();
        let exchange = Exchange {
            user: "Explain ownership in Rust with examples and borrowing rules.".into(),
            assistant: "Ownership ensures memory safety without a GC. Each value has one owner."
                .into(),
        };
        // First capture succeeds
        let first = lm.capture(&exchange).await.unwrap();
        assert_eq!(first.len(), 1);
        // Identical exchange is an echo → rejected
        let second = lm.capture(&exchange).await.unwrap();
        assert!(second.is_empty(), "duplicate should be rejected as echo");
    }

    #[tokio::test]
    async fn recall_returns_empty_for_empty_store() {
        let lm = lm();
        let results = lm.recall("anything", 5, &[]).await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn recall_excludes_categories() {
        let lm = lm();
        lm.capture(&Exchange {
            user: "I prefer using snake_case convention for all variable names always.".into(),
            assistant: "Noted — snake_case is the Rust convention for variables and functions."
                .into(),
        })
        .await
        .unwrap();

        let all = lm
            .recall("snake_case naming convention", 5, &[])
            .await
            .unwrap();
        assert!(!all.is_empty());

        let excluded = lm
            .recall(
                "snake_case naming convention",
                5,
                &[
                    MemoryCategory::Preference,
                    MemoryCategory::Conversation,
                    MemoryCategory::Correction,
                ],
            )
            .await
            .unwrap();
        assert!(excluded.is_empty(), "excluded categories must not appear");
    }

    #[test]
    fn inject_context_respects_budget() {
        let lm = lm();
        let mem = MemoryEntry {
            id: "1".into(),
            content: "A".repeat(200),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: vec![],
            score: 1.0,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        // Budget of 10 tokens = 40 chars; header alone is ~22 chars, so only short content fits.
        let ctx = lm.inject_context(&[mem], Some(10));
        assert!(ctx.len() <= 40, "context must not exceed budget");
    }

    #[test]
    fn inject_context_includes_category_label() {
        let lm = lm();
        let mem = MemoryEntry {
            id: "1".into(),
            content: "Use cargo test to run tests.".into(),
            category: MemoryCategory::CodePattern,
            tags: vec![],
            embedding: vec![],
            score: 0.9,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let ctx = lm.inject_context(&[mem], None);
        assert!(ctx.contains("[code-pattern]"));
        assert!(ctx.contains("Use cargo test to run tests."));
    }

    #[test]
    fn detect_category_classifies_code() {
        assert_eq!(detect_category("fn main() {}"), MemoryCategory::CodePattern);
        assert_eq!(
            detect_category("cargo build failed with error"),
            MemoryCategory::ErrorFix
        );
        assert_eq!(
            detect_category("my convention is tabs over spaces, a preference"),
            MemoryCategory::Preference
        );
        assert_eq!(
            detect_category("we decided to use tokio because it is async"),
            MemoryCategory::Decision
        );
        assert_eq!(
            detect_category("what is the weather today"),
            MemoryCategory::Conversation
        );
    }

    #[test]
    fn detect_category_classifies_corrections() {
        assert_eq!(
            detect_category("don't use unwrap in production"),
            MemoryCategory::Correction
        );
        assert_eq!(
            detect_category("I prefer spaces over tabs from now on"),
            MemoryCategory::Correction
        );
        assert_eq!(
            detect_category("Actually, that's wrong — use Vec instead"),
            MemoryCategory::Correction
        );
    }

    #[tokio::test]
    async fn ingest_documents_path_indexes_markdown_text_and_source() {
        let lm = lm();
        let dir = tempdir().unwrap();
        let root = dir.path();
        let nested = root.join("src");
        tokio::fs::create_dir_all(&nested).await.unwrap();

        let md = root.join("guide.md");
        let txt = root.join("notes.txt");
        let rs = nested.join("lib.rs");
        let bin = root.join("image.png");

        tokio::fs::write(&md, "# Deployment runbook\nUse staging first.\n")
            .await
            .unwrap();
        tokio::fs::write(&txt, "Remember to rotate secrets monthly.\n")
            .await
            .unwrap();
        tokio::fs::write(
            &rs,
            "pub async fn start_server() { tokio::spawn(async move {}); }\n",
        )
        .await
        .unwrap();
        tokio::fs::write(&bin, [0_u8, 1_u8, 2_u8, 3_u8])
            .await
            .unwrap();

        let indexed = lm.ingest_documents_path(root).await.unwrap();
        assert!(indexed >= 3, "expected supported files to be indexed");

        let deployment = lm
            .recall("deployment runbook staging", 5, &[])
            .await
            .unwrap();
        assert!(deployment.iter().any(|m| m.content.contains("guide.md")));

        let secret_notes = lm.recall("rotate secrets monthly", 5, &[]).await.unwrap();
        assert!(secret_notes.iter().any(|m| m.content.contains("notes.txt")));

        let rust_code = lm
            .recall("tokio spawn async fn server", 5, &[])
            .await
            .unwrap();
        assert!(rust_code.iter().any(|m| {
            m.content.contains("lib.rs") && m.category == MemoryCategory::CodePattern
        }));
    }

    #[tokio::test]
    async fn ingest_documents_path_replaces_existing_chunks_for_same_source() {
        let lm = lm();
        let dir = tempdir().unwrap();
        let file = dir.path().join("kb.txt");
        tokio::fs::write(&file, "alpha release notes and rollout checklist")
            .await
            .unwrap();

        let first = lm.ingest_documents_path(&file).await.unwrap();
        assert!(first > 0);

        tokio::fs::write(&file, "beta release notes and rollback checklist")
            .await
            .unwrap();
        let second = lm.ingest_documents_path(&file).await.unwrap();
        assert!(second > 0);

        let canonical: PathBuf = tokio::fs::canonicalize(&file).await.unwrap();
        let source_tag = format!("source:{}", canonical.to_string_lossy());
        let entries = lm.scan_all().await.unwrap();
        let source_entries: Vec<_> = entries
            .iter()
            .filter(|m| m.tags.iter().any(|t| t == &source_tag))
            .collect();

        assert_eq!(source_entries.len(), second);
        assert!(source_entries.iter().all(|m| m.content.contains("beta")));
        assert!(source_entries.iter().all(|m| !m.content.contains("alpha")));
    }

    // ── Mutation-gap coverage ──────────────────────────────────────────────

    // cosine_similarity
    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_mismatched_lengths() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let a: Vec<f32> = vec![];
        let b: Vec<f32> = vec![];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn cosine_similarity_zero_norm() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
        assert_eq!(cosine_similarity(&b, &a), 0.0);
    }

    // floor_char_boundary
    #[test]
    fn floor_char_boundary_at_end() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 10), 5); // past end → len
    }

    #[test]
    fn floor_char_boundary_at_valid_boundary() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 3), 3);
    }

    #[test]
    fn floor_char_boundary_multibyte() {
        let s = "aé"; // a=1 byte, é=2 bytes → len=3
                      // idx=2 is in the middle of é, should walk back to 1
        assert_eq!(floor_char_boundary(s, 2), 1);
        assert_eq!(floor_char_boundary(s, 1), 1); // valid boundary
        assert_eq!(floor_char_boundary(s, 3), 3); // end of é
    }

    #[test]
    fn floor_char_boundary_zero() {
        let s = "hello";
        assert_eq!(floor_char_boundary(s, 0), 0);
    }

    // split_document_chunks
    #[test]
    fn split_document_chunks_empty_text() {
        assert!(split_document_chunks("", 100, 10).is_empty());
        assert!(split_document_chunks("   ", 100, 10).is_empty());
    }

    #[test]
    fn split_document_chunks_zero_max_chars() {
        assert!(split_document_chunks("hello world", 0, 0).is_empty());
    }

    #[test]
    fn split_document_chunks_single_small_chunk() {
        let chunks = split_document_chunks("short text", 1000, 100);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "short text");
    }

    #[test]
    fn split_document_chunks_splits_with_overlap() {
        // 20 chars of content, max 10, overlap 3
        let text = "abcdefghijklmnopqrst";
        let chunks = split_document_chunks(text, 10, 3);
        assert!(chunks.len() >= 2);
        // First chunk is first 10 chars
        assert_eq!(chunks[0], "abcdefghij");
        // Overlap means next chunk starts at 10-3=7
        assert!(chunks[1].starts_with('h'));
    }

    #[test]
    fn split_document_chunks_no_infinite_loop() {
        // Overlap larger than chunk size shouldn't infinite loop
        let text = "abcdefghij";
        let chunks = split_document_chunks(text, 5, 10);
        assert!(!chunks.is_empty());
    }

    // format_document_chunk_content
    #[test]
    fn format_document_chunk_content_includes_metadata() {
        let result = format_document_chunk_content("/path/to/file.rs", 2, 5, "fn main() {}");
        assert!(result.contains("/path/to/file.rs"));
        assert!(result.contains("2/5"));
        assert!(result.contains("fn main() {}"));
    }

    // extract_tags
    #[test]
    fn extract_tags_detects_languages() {
        let exchange = Exchange {
            user: "How do I use Rust and Python together?".into(),
            assistant: "Use pyo3 for Rust-Python interop.".into(),
        };
        let tags = extract_tags(&exchange);
        assert!(tags.contains(&"lang:rust".to_string()));
        assert!(tags.contains(&"lang:python".to_string()));
        assert!(!tags.contains(&"lang:go".to_string()));
    }

    #[test]
    fn extract_tags_detects_tools() {
        let exchange = Exchange {
            user: "I ran cargo build and got a git error in docker.".into(),
            assistant: "Check your Dockerfile.".into(),
        };
        let tags = extract_tags(&exchange);
        assert!(tags.contains(&"tool:cargo".to_string()));
        assert!(tags.contains(&"tool:git".to_string()));
        assert!(tags.contains(&"tool:docker".to_string()));
    }

    #[test]
    fn extract_tags_empty_for_unrelated() {
        let exchange = Exchange {
            user: "What is the meaning of life?".into(),
            assistant: "42, according to Douglas Adams.".into(),
        };
        let tags = extract_tags(&exchange);
        assert!(tags.is_empty());
    }

    // passes_quality_gate
    #[test]
    fn passes_quality_gate_rejects_empty() {
        assert!(!passes_quality_gate(""));
        assert!(!passes_quality_gate("   "));
    }

    #[test]
    fn passes_quality_gate_rejects_heartbeat() {
        assert!(!passes_quality_gate("HEARTBEAT_OK"));
        assert!(!passes_quality_gate("heartbeat_ok"));
    }

    #[test]
    fn passes_quality_gate_rejects_noise() {
        assert!(!passes_quality_gate("ok"));
        assert!(!passes_quality_gate("Thanks."));
    }

    #[test]
    fn passes_quality_gate_rejects_git_noise() {
        assert!(!passes_quality_gate(
            "commit abc123\nAuthor: dev\nDate: 2026-01-01\n\nmessage"
        ));
        assert!(!passes_quality_gate("diff --git a/file.rs b/file.rs"));
    }

    #[test]
    fn passes_quality_gate_accepts_real_content() {
        assert!(passes_quality_gate(
            "The tokio runtime provides async task scheduling."
        ));
    }

    // is_git_noise
    #[test]
    fn is_git_noise_commit_format() {
        assert!(is_git_noise(
            "commit abc\nAuthor: dev\nDate: today\nsomething"
        ));
    }

    #[test]
    fn is_git_noise_diff_format() {
        assert!(is_git_noise("diff --git a/foo b/foo\nindex abc..def"));
    }

    #[test]
    fn is_git_noise_index_plus_format() {
        assert!(is_git_noise("index abc..def\n+++ b/foo"));
    }

    #[test]
    fn is_git_noise_not_normal_text() {
        assert!(!is_git_noise(
            "This is a normal discussion about git branching strategies."
        ));
    }

    // classify_document_kind
    #[test]
    fn classify_markdown() {
        assert_eq!(
            classify_document_kind(Path::new("readme.md")),
            Some(DocumentKind::Markdown)
        );
        assert_eq!(
            classify_document_kind(Path::new("guide.markdown")),
            Some(DocumentKind::Markdown)
        );
    }

    #[test]
    fn classify_text() {
        assert_eq!(
            classify_document_kind(Path::new("notes.txt")),
            Some(DocumentKind::Text)
        );
        assert_eq!(
            classify_document_kind(Path::new("log.text")),
            Some(DocumentKind::Text)
        );
    }

    #[test]
    fn classify_source_code() {
        for ext in &["rs", "ts", "py", "go", "java", "toml", "json", "yaml"] {
            let path = format!("file.{ext}");
            assert_eq!(
                classify_document_kind(Path::new(&path)),
                Some(DocumentKind::SourceCode),
                "expected SourceCode for .{ext}"
            );
        }
    }

    #[test]
    fn classify_unknown_extension() {
        assert_eq!(classify_document_kind(Path::new("image.png")), None);
        assert_eq!(classify_document_kind(Path::new("archive.tar.gz")), None);
    }

    #[test]
    fn classify_no_extension() {
        assert_eq!(classify_document_kind(Path::new("Makefile")), None);
    }

    // DocumentKind
    #[test]
    fn document_kind_as_str() {
        assert_eq!(DocumentKind::Markdown.as_str(), "markdown");
        assert_eq!(DocumentKind::Text.as_str(), "text");
        assert_eq!(DocumentKind::SourceCode.as_str(), "source-code");
    }

    #[test]
    fn document_kind_category() {
        assert_eq!(DocumentKind::Markdown.category(), MemoryCategory::Fact);
        assert_eq!(DocumentKind::Text.category(), MemoryCategory::Fact);
        assert_eq!(
            DocumentKind::SourceCode.category(),
            MemoryCategory::CodePattern
        );
    }

    // detect_category — additional coverage for each branch
    #[test]
    fn detect_category_error_keywords() {
        assert_eq!(
            detect_category("there was a panic in the code"),
            MemoryCategory::ErrorFix
        );
        assert_eq!(
            detect_category("how to fix this bug"),
            MemoryCategory::ErrorFix
        );
    }

    #[test]
    fn detect_category_code_keywords() {
        assert_eq!(
            detect_category("impl Display for MyType"),
            MemoryCategory::CodePattern
        );
        assert_eq!(
            detect_category("struct Config { port: u16 }"),
            MemoryCategory::CodePattern
        );
        assert_eq!(
            detect_category("add this crate to dependencies"),
            MemoryCategory::CodePattern
        );
        assert_eq!(
            detect_category("use trait bounds for generics"),
            MemoryCategory::CodePattern
        );
    }

    #[test]
    fn detect_category_preference_keywords() {
        // "convention" keyword (avoid words containing 'fix', 'error', etc.)
        assert_eq!(
            detect_category("Our team convention is tab indentation."),
            MemoryCategory::Preference
        );
        // "prefer" without "i prefer" (which is a correction signal)
        assert_eq!(
            detect_category("Most developers prefer explicit return types in Rust."),
            MemoryCategory::Preference
        );
    }

    #[test]
    fn detect_category_decision_keywords() {
        assert_eq!(
            detect_category("we decided to go with tokio"),
            MemoryCategory::Decision
        );
        assert_eq!(
            detect_category("chose actix-web because of performance"),
            MemoryCategory::Decision
        );
    }

    #[test]
    fn detect_category_correction_from_user_segment() {
        // "don't" in user text triggers correction
        assert_eq!(
            detect_category("User: don't do that anymore\nAssistant: Understood."),
            MemoryCategory::Correction
        );
    }

    // capture_fact
    #[tokio::test]
    async fn capture_fact_stores_valid_fact() {
        let lm = lm();
        let id = lm
            .capture_fact(
                "The Rust borrow checker enforces memory safety at compile time.",
                vec!["lang:rust".into()],
            )
            .await
            .unwrap();
        assert!(id.is_some());

        let all = lm.scan_all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, MemoryCategory::Fact);
        assert!(all[0].tags.contains(&"lang:rust".to_string()));
    }

    #[tokio::test]
    async fn capture_fact_rejects_short() {
        let lm = lm();
        let id = lm.capture_fact("short", vec![]).await.unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn capture_fact_rejects_echo() {
        let lm = lm();
        let fact = "The standard library provides Vec, HashMap, and other collections.";
        let first = lm.capture_fact(fact, vec![]).await.unwrap();
        assert!(first.is_some());
        let second = lm.capture_fact(fact, vec![]).await.unwrap();
        assert!(second.is_none(), "duplicate fact should be rejected");
    }

    // capture_procedure_candidate
    #[tokio::test]
    async fn capture_procedure_candidate_stores_valid() {
        let lm = lm();
        let id = lm
            .capture_procedure_candidate(
                "When the user asks about deployment, suggest Docker with compose.",
                vec!["tool:docker".into()],
            )
            .await
            .unwrap();
        assert!(id.is_some());

        let all = lm.scan_all().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].category, MemoryCategory::Procedure);
        assert!(all[0].tags.contains(&"procedure:candidate".to_string()));
        assert!(all[0].tags.contains(&"tool:docker".to_string()));
    }

    #[tokio::test]
    async fn capture_procedure_candidate_rejects_noise() {
        let lm = lm();
        let id = lm
            .capture_procedure_candidate("ok sure", vec![])
            .await
            .unwrap();
        assert!(id.is_none());
    }

    #[tokio::test]
    async fn capture_procedure_candidate_rejects_echo() {
        let lm = lm();
        let desc = "When tests fail, run cargo test with --nocapture for output.";
        let first = lm.capture_procedure_candidate(desc, vec![]).await.unwrap();
        assert!(first.is_some());
        let second = lm.capture_procedure_candidate(desc, vec![]).await.unwrap();
        assert!(second.is_none());
    }

    // embed_text
    #[tokio::test]
    async fn embed_text_returns_vector() {
        let lm = lm();
        let emb = lm.embed_text("test input").await.unwrap();
        assert!(!emb.is_empty());
    }

    // scan_all
    #[tokio::test]
    async fn scan_all_returns_all_entries() {
        let lm = lm();
        lm.capture_fact(
            "First fact with enough content to pass the quality gate.",
            vec![],
        )
        .await
        .unwrap();
        lm.capture_fact("Second fact with different content for the store.", vec![])
            .await
            .unwrap();
        let all = lm.scan_all().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    // recall scoring and ordering
    #[tokio::test]
    async fn recall_sets_score_field() {
        let lm = lm();
        lm.capture(&Exchange {
            user: "Explain the async runtime in tokio with spawn and select.".into(),
            assistant: "Tokio uses a multi-threaded work-stealing scheduler for tasks.".into(),
        })
        .await
        .unwrap();
        let results = lm.recall("tokio async runtime", 5, &[]).await.unwrap();
        assert!(!results.is_empty());
        // Score should be set (MockEmbedder produces consistent vectors)
        assert!(results[0].score > 0.0);
    }

    // inject_context with empty memories
    #[test]
    fn inject_context_empty_memories() {
        let lm = lm();
        let ctx = lm.inject_context(&[], None);
        assert!(ctx.contains("# Relevant memories"));
        // Should just be the header, no numbered entries
        assert!(!ctx.contains("1."));
    }

    // inject_context truncates at budget
    #[test]
    fn inject_context_truncates_multiple_entries() {
        let lm = lm();
        let mems: Vec<MemoryEntry> = (0..100)
            .map(|i| MemoryEntry {
                id: format!("{i}"),
                content: format!("Memory entry number {i} with some padding text."),
                category: MemoryCategory::Fact,
                tags: vec![],
                embedding: vec![],
                score: 0.9,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .collect();
        // Very tight budget of 50 tokens = 200 chars
        let ctx = lm.inject_context(&mems, Some(50));
        assert!(ctx.len() <= 200);
    }

    // --- Mutation coverage: embed_text returns correct dimensions ---
    #[tokio::test]
    async fn embed_text_returns_correct_dimensions() {
        let lm = lm();
        let vec = lm.embed_text("hello world").await.unwrap();
        // MockEmbedder returns EMBEDDING_DIM (384) dimensions.
        // Mutant replaces body with Ok(vec![0.0]) which is length 1.
        assert_eq!(vec.len(), crate::memory::embed::EMBEDDING_DIM);
    }

    #[tokio::test]
    async fn embed_text_varies_with_input() {
        let lm = lm();
        let v1 = lm.embed_text("alpha").await.unwrap();
        let v2 = lm.embed_text("beta").await.unwrap();
        // Different inputs must produce different embeddings.
        assert_ne!(v1, v2);
    }

    // --- Mutation coverage: ingest_document_file chunk numbering starts at 1 ---
    #[tokio::test]
    async fn ingest_document_file_chunk_numbering_starts_at_one() {
        let lm = lm();
        let dir = tempdir().unwrap();
        let file = dir.path().join("test.md");
        std::fs::write(&file, "Hello world, this is a test document.").unwrap();

        let count = lm.ingest_document_file(&file).await.unwrap();
        assert!(count >= 1);

        // Retrieve stored entries and verify chunk tag format starts at "1"
        let entries = lm.store.all().await.unwrap();
        let chunk_tags: Vec<&String> = entries
            .iter()
            .flat_map(|e| e.tags.iter())
            .filter(|t| t.starts_with("chunk:"))
            .collect();
        assert!(!chunk_tags.is_empty());
        // First chunk must be "chunk:1/N" not "chunk:0/N"
        let first_chunk = chunk_tags.iter().find(|t| t.starts_with("chunk:1/"));
        assert!(
            first_chunk.is_some(),
            "chunk numbering must start at 1, got: {:?}",
            chunk_tags
        );
        // Verify no chunk:0/ exists
        let zero_chunk = chunk_tags.iter().find(|t| t.starts_with("chunk:0/"));
        assert!(
            zero_chunk.is_none(),
            "chunk numbering must not start at 0, got: {:?}",
            chunk_tags
        );
    }

    // --- Mutation coverage: inject_context default budget is context_window / 4, not * 4 ---
    #[test]
    fn inject_context_default_budget_limits_output() {
        // Create LM with small context window so default budget is tight
        let lm = PluresLm::new(
            Arc::new(InMemoryStore::new()),
            Box::new(MockEmbedder),
            100, // context_window = 100 tokens → default budget = 25 tokens = 100 chars
        );
        let mems: Vec<MemoryEntry> = (0..50)
            .map(|i| MemoryEntry {
                id: format!("{i}"),
                content: format!("Memory entry number {i} with lots of padding text that goes on."),
                category: MemoryCategory::Fact,
                tags: vec![],
                embedding: vec![],
                score: 0.9,
                created_at: "2026-01-01T00:00:00Z".into(),
            })
            .collect();
        let ctx = lm.inject_context(&mems, None);
        // Default budget = 100/4 = 25 tokens = 100 chars.
        // If mutant changes / to *, budget would be 400 tokens = 1600 chars (no truncation).
        // With correct /, output must be ≤ 100 chars.
        assert!(
            ctx.len() <= 100,
            "default budget should limit output to ~100 chars, got {} chars",
            ctx.len()
        );
    }

    // --- Mutation coverage: inject_context > vs >= boundary ---
    #[test]
    fn inject_context_boundary_exact_fit_is_included() {
        // The condition is: if out.len() + block.len() > max_chars { break; }
        // With >=, an entry that fits exactly would be excluded.
        // We need to find a case where out.len() + block.len() == max_chars and verify inclusion.
        let lm = lm(); // context_window = 128_000

        // header = "# Relevant memories\n\n" = 22 chars
        // block = "1. [fact] " + content + "\n" = 11 + content_len
        // budget_tokens = 13 → max_chars = 52. header(22) + block(30) = 52 = max_chars.
        // condition: 52 > 52 is FALSE → entry IS included.
        // With >=: 52 >= 52 is TRUE → entry excluded (mutant detected).

        let content2 = "Y".repeat(19); // block = 11 + 19 = 30
        let mem = MemoryEntry {
            id: "1".into(),
            content: content2.clone(),
            category: MemoryCategory::Fact,
            tags: vec![],
            embedding: vec![],
            score: 0.9,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let ctx = lm.inject_context(&[mem], Some(13)); // 13 tokens = 52 chars
                                                       // The entry should be included (exactly fits)
        assert!(
            ctx.contains(&content2),
            "entry that exactly fits budget must be included, got: {:?}",
            ctx
        );
    }
}
