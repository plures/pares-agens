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
    /// Factual knowledge extracted from responses.
    Fact,
    /// Procedure candidates inferred from conversations.
    Procedure,
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
    /// User correction — a persistent behavioral adjustment.
    Correction,
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
            Self::Fact => "fact",
            Self::Procedure => "procedure",
            Self::UiInteraction => "ui-interaction",
            Self::AppState => "app-state",
            Self::ScreenCapture => "screen-capture",
            Self::AutomationTrace => "automation-trace",
            Self::BuildResult => "build-result",
            Self::DemoCheckpoint => "demo-checkpoint",
            Self::Correction => "correction",
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

/// A persisted conversation turn stored in PluresDB.
///
/// Each turn captures a single user→assistant exchange along with tool
/// interactions, keyed by channel so multi-channel history stays separate.
/// Unlike `MemoryEntry` (which stores distilled knowledge), `ChatTurn`
/// stores the raw conversation for context-window hydration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatTurn {
    /// Unique turn identifier (UUID v4).
    pub id: String,
    /// Channel identifier (e.g. `"telegram"`, `"cli"`).
    pub channel: String,
    /// Session identifier within a channel (e.g. `"main"`, `"alt"`).
    ///
    /// Older persisted turns may not include this field; they default to
    /// `"main"` during deserialization for backward compatibility.
    #[serde(default = "default_chat_turn_session_id")]
    pub session_id: String,
    /// ISO 8601 timestamp of this turn.
    pub timestamp: String,
    /// The ordered messages that make up this turn (user, assistant, tool
    /// calls/results — everything the model loop produced).
    pub messages: Vec<pares_radix_core::model::ChatMessage>,
}

fn default_chat_turn_session_id() -> String {
    "main".to_string()
}
