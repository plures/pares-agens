use serde::{Deserialize, Serialize};

/// All supported memory categories.
///
/// These coexist in the same vector space — see the pluresLM desktop memory design doc.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryCategory {
    /// General conversation exchanges.
    Conversation,
    /// Reusable code snippets and patterns.
    CodePattern,
    /// Records of errors encountered and their fixes.
    ErrorFix,
    /// Stated user preferences and settings.
    Preference,
    /// Recorded decisions and rationale.
    Decision,
    /// UI click/type/navigate events with before/after state.
    UiInteraction,
    /// Application window snapshots.
    AppState,
    /// Tagged screenshots with semantic region annotations.
    ScreenCapture,
    /// Full trace of a multi-step automated sequence.
    AutomationTrace,
    /// Build/compile/test outcomes with environment context.
    BuildResult,
    /// Named state during an executable presentation.
    DemoCheckpoint,
}

impl MemoryCategory {
    /// Return a human-readable label used in injected context blocks.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Conversation => "conversation",
            Self::CodePattern => "code-pattern",
            Self::ErrorFix => "error-fix",
            Self::Preference => "preference",
            Self::Decision => "decision",
            Self::UiInteraction => "ui-interaction",
            Self::AppState => "app-state",
            Self::ScreenCapture => "screen-capture",
            Self::AutomationTrace => "automation-trace",
            Self::BuildResult => "build-result",
            Self::DemoCheckpoint => "demo-checkpoint",
        }
    }
}

/// A single stored memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique memory identifier (UUID v4).
    pub id: String,
    /// The raw text content of the memory.
    pub content: String,
    /// Semantic category used for filtering and display.
    pub category: MemoryCategory,
    /// Arbitrary tags (e.g. `["app:vscode", "action:build"]`).
    pub tags: Vec<String>,
    /// Embedding vector produced by `EmbeddingProvider`.
    ///
    /// For BAAI/bge-small-en-v1.5 this is 384 floats.
    pub embedding: Vec<f32>,
    /// Relevance score populated by [`super::PluresLm::recall`]; 0.0 when stored.
    pub score: f32,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
}

/// A conversation exchange used as input to [`super::PluresLm::capture`].
#[derive(Debug, Clone)]
pub struct Exchange {
    /// The user's message.
    pub user: String,
    /// The assistant's reply.
    pub assistant: String,
}
