//! Channel-level threading adapters.
//!
//! Each channel adapter has a corresponding [`ChannelThreading`] implementation
//! that declares its threading capabilities and handles thread-related formatting
//! and metadata resolution.
//!
//! The trait is defined locally for now. Once `pares_agens_core::threading` lands,
//! this module will re-export from core instead.

pub mod http;
pub mod keyboard;
pub mod stdio;
pub mod tauri;
pub mod telegram;
pub mod tui;

use async_trait::async_trait;
use serde_json::Value;

// Re-export adapter structs at module level for convenience.
pub use http::HttpThreading;
pub use keyboard::ThreadKeyboard;
pub use stdio::StdioThreading;
pub use tauri::TauriThreading;
pub use telegram::TelegramThreading;
pub use tui::TuiThreading;

/// Describes what threading features a channel natively supports.
#[derive(Debug, Clone)]
pub struct ThreadCapabilities {
    /// Can show visual indicators (icons, badges) for active threads.
    pub indicators: bool,
    /// Supports reply-chain style threading (e.g. Telegram reply_to_message).
    pub reply_chains: bool,
    /// Has native thread isolation (forum topics, tabs, separate endpoints).
    pub native_threads: bool,
    /// Can present a thread switcher UI (inline keyboard, tab bar, sidebar).
    pub thread_switcher: bool,
    /// Can display multiple threads concurrently (split pane, multi-window).
    pub concurrent_display: bool,
}

/// Metadata about a conversation thread.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ThreadInfo {
    /// Unique thread identifier.
    pub id: String,
    /// Human-readable topic name.
    pub topic: String,
    /// Number of messages in this thread.
    pub message_count: usize,
    /// Whether this thread is currently active.
    pub is_active: bool,
}

/// Opaque anchor value returned when a thread is created in a channel.
/// May contain message IDs, topic IDs, or other channel-specific references.
pub type ChannelAnchor = Value;

/// Errors arising from channel threading operations.
#[derive(Debug, thiserror::Error)]
pub enum ThreadError {
    /// The requested thread was not found.
    #[error("thread not found: {0}")]
    NotFound(String),
    /// A storage/persistence error occurred.
    #[error("storage error: {0}")]
    Storage(String),
    /// A channel-specific communication error.
    #[error("channel error: {0}")]
    Channel(String),
}

/// Trait for channel-specific threading behavior.
///
/// Each channel adapter implements this to declare capabilities and provide
/// formatting/resolution logic. Heavy state (thread storage, lifecycle) lives
/// in the core `ThreadStore`; this trait handles channel-facing concerns only.
///
/// # TODO
/// Import from `pares_agens_core::threading::channel` once that module lands.
#[async_trait]
pub trait ChannelThreading: Send + Sync {
    /// Report what threading features this channel supports.
    fn capabilities(&self) -> ThreadCapabilities;

    /// Called when a new thread is created. Returns a channel-specific anchor
    /// (e.g. a message ID for reply chains, a topic ID for forums).
    async fn on_thread_created(
        &self,
        thread_id: &str,
        topic: &str,
        chat_id: &str,
    ) -> Result<ChannelAnchor, ThreadError>;

    /// Called when the active thread switches. Allows the channel to update
    /// visual indicators or send transition messages.
    async fn on_thread_switched(
        &self,
        from_topic: &str,
        to_topic: &str,
        chat_id: &str,
    ) -> Result<(), ThreadError>;

    /// Format a message for display within a specific thread context.
    async fn format_message_in_thread(&self, topic: &str, content: &str) -> String;

    /// Attempt to resolve a thread ID from incoming message metadata.
    async fn resolve_thread_from_metadata(&self, metadata: &Value) -> Option<String>;

    /// Format a list of threads for display in this channel.
    async fn format_thread_list(&self, threads: &[ThreadInfo]) -> String;
}
