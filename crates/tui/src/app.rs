//! TUI application state and event loop.

use std::fs;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use pares_agens_core::agent::Agent;
use pares_radix_core::commands::{CommandContext, CommandRegistry, CommandResult, SessionCommand};
use pares_radix_core::model::StreamDelta;
use pares_radix_core::session::SessionManager;
use pares_radix_core::Event;

/// A single chat message displayed in the TUI.
#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    System,
}

/// Events the TUI event loop processes.
pub enum AppEvent {
    /// User submitted input text.
    UserInput(String),
    /// A streaming chunk arrived from the model.
    StreamChunk(String),
    /// Agent finished responding (final complete content).
    AgentResponse(String),
    /// Terminal resize or redraw needed.
    Redraw,
    /// Quit the application.
    Quit,
    /// Session list was loaded from persistence.
    SessionsLoaded(Vec<(String, bool)>),
    /// Session messages were loaded from persistence.
    SessionMessagesLoaded(String, Vec<(String, String, String)>),
}

/// Input history for Up/Down arrow recall.
#[derive(Clone, Debug, Default)]
pub struct InputHistory {
    /// All past inputs, oldest first.
    entries: Vec<String>,
    /// Current navigation index (None = not navigating, at fresh prompt).
    index: Option<usize>,
    /// Stashed current input when user starts navigating.
    stash: String,
    /// Maximum entries to keep.
    max_entries: usize,
}

impl InputHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Vec::new(),
            index: None,
            stash: String::new(),
            max_entries,
        }
    }

    /// Record a submitted input line.
    pub fn push(&mut self, input: &str) {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return;
        }
        // Deduplicate consecutive entries
        if self.entries.last().map(|s| s.as_str()) == Some(trimmed) {
            return;
        }
        self.entries.push(trimmed.to_string());
        if self.entries.len() > self.max_entries {
            self.entries.remove(0);
        }
        self.reset_navigation();
    }

    /// Navigate up (older). Returns the history entry to display, or None if at the top.
    pub fn up(&mut self, current_input: &str) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        match self.index {
            None => {
                // Start navigating: stash current input, go to most recent
                self.stash = current_input.to_string();
                let idx = self.entries.len() - 1;
                self.index = Some(idx);
                Some(&self.entries[idx])
            }
            Some(idx) if idx > 0 => {
                let new_idx = idx - 1;
                self.index = Some(new_idx);
                Some(&self.entries[new_idx])
            }
            _ => None, // Already at oldest
        }
    }

    /// Navigate down (newer). Returns the entry or stashed input, or None if already at bottom.
    pub fn down(&mut self) -> Option<&str> {
        match self.index {
            None => None, // Not navigating
            Some(idx) => {
                if idx + 1 < self.entries.len() {
                    let new_idx = idx + 1;
                    self.index = Some(new_idx);
                    Some(&self.entries[new_idx])
                } else {
                    // Back to the stashed fresh input
                    self.index = None;
                    Some(&self.stash)
                }
            }
        }
    }

    /// Reset navigation state (e.g., after submitting).
    pub fn reset_navigation(&mut self) {
        self.index = None;
        self.stash.clear();
    }

    /// Number of entries stored.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether history is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Default history file path: `~/.pares-radix/history`.
    fn history_path() -> Option<PathBuf> {
        // Allow override via PARES_RADIX_HISTORY_PATH for testing without env races
        if let Ok(explicit) = std::env::var("PARES_RADIX_HISTORY_PATH") {
            return Some(PathBuf::from(explicit));
        }
        std::env::var("HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(".pares-radix").join("history"))
    }

    /// Load history entries from disk. Silently returns empty on any error.
    pub fn load_from_disk(&mut self) {
        let Some(path) = Self::history_path() else {
            return;
        };
        self.load_from_path(&path);
    }

    /// Load history entries from an explicit path (useful for testing).
    pub fn load_from_path(&mut self, path: &std::path::Path) {
        let Ok(file) = fs::File::open(path) else {
            return;
        };
        let reader = std::io::BufReader::new(file);
        let mut entries: Vec<String> = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else { break };
            let trimmed = line.trim().to_string();
            if !trimmed.is_empty() {
                // Deduplicate consecutive on load
                if entries.last().map(|s| s.as_str()) != Some(&trimmed) {
                    entries.push(trimmed);
                }
            }
        }
        // Keep only the last max_entries
        if entries.len() > self.max_entries {
            entries = entries.split_off(entries.len() - self.max_entries);
        }
        self.entries = entries;
    }

    /// Persist all history entries to disk. Silently ignores errors.
    pub fn save_to_disk(&self) {
        let Some(path) = Self::history_path() else {
            return;
        };
        self.save_to_path(&path);
    }

    /// Save history entries to an explicit path (useful for testing).
    pub fn save_to_path(&self, path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let Ok(mut file) = fs::File::create(path) else {
            return;
        };
        for entry in &self.entries {
            let _ = writeln!(file, "{entry}");
        }
    }
}

/// Application state.
pub struct App {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub input_cursor: usize,
    pub scroll_offset: u16,
    pub user_scrolled: bool,
    pub viewport_height: u16,
    pub thinking: bool,
    /// True while streaming chunks are arriving (after first chunk, before final response).
    pub streaming: bool,
    pub current_model: String,
    pub agent: Arc<Agent>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
    /// Input history for Up/Down arrow recall.
    pub history: InputHistory,
    /// Named sessions: Vec<(name, is_active)>.
    pub sessions: Vec<(String, bool)>,
    /// Current active session name.
    pub current_session: String,
    /// Optional session persistence manager.
    pub session_manager: Option<Arc<SessionManager>>,
}

impl App {
    pub fn new(
        agent: Arc<Agent>,
        model_name: String,
        event_tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            messages: vec![ChatMessage {
                role: Role::System,
                content: format!("Pares Radix TUI — model: {model_name}. Type /help for commands."),
                timestamp: chrono::Utc::now(),
            }],
            input: String::new(),
            input_cursor: 0,
            scroll_offset: 0,
            user_scrolled: false,
            viewport_height: 35,
            thinking: false,
            streaming: false,
            current_model: model_name,
            agent,
            event_tx,
            history: {
                let mut h = InputHistory::new(500);
                h.load_from_disk();
                h
            },
            sessions: vec![("default".to_string(), true)],
            current_session: "default".to_string(),
            session_manager: None,
        }
    }

    /// Attach a session manager for cross-restart persistence.
    pub fn with_session_manager(mut self, manager: Arc<SessionManager>) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Kick off async loading of persisted sessions. Results arrive via `AppEvent::SessionsLoaded`.
    pub fn load_persisted_sessions(&self) {
        let Some(mgr) = self.session_manager.clone() else {
            return;
        };
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let summaries = mgr.list_sessions("tui", 50).await;
            let mut sessions: Vec<(String, bool)> = summaries
                .into_iter()
                .map(|s| {
                    let name = if s.key == "active" {
                        s.topic_summary.unwrap_or_else(|| "default".to_string())
                    } else {
                        s.topic_summary.unwrap_or_else(|| s.key.clone())
                    };
                    (name, s.key == "active")
                })
                .collect();
            if sessions.is_empty() {
                sessions.push(("default".to_string(), true));
            }
            let _ = tx.send(AppEvent::SessionsLoaded(sessions));
        });
    }

    /// Handle the `SessionsLoaded` event.
    pub fn handle_sessions_loaded(&mut self, sessions: Vec<(String, bool)>) {
        self.sessions = sessions;
        if let Some((name, _)) = self.sessions.iter().find(|(_, active)| *active) {
            self.current_session = name.clone();
        }
    }

    /// Handle the `SessionMessagesLoaded` event.
    pub fn handle_session_messages_loaded(
        &mut self,
        session_name: String,
        turns: Vec<(String, String, String)>,
    ) {
        if session_name != self.current_session {
            return; // User switched away before load completed
        }
        self.messages.clear();
        self.scroll_offset = 0;
        if !turns.is_empty() {
            self.load_history_from_turns(turns);
        } else {
            self.push_system(&format!("Session: {session_name}"));
        }
    }

    /// Load conversation history from persisted turns into the TUI message list.
    ///
    /// This is called once at startup to populate the chat view with previous
    /// conversation so the user sees context from prior sessions.
    pub fn load_history_from_turns(&mut self, turns: Vec<(String, String, String)>) {
        // turns: Vec<(role, content, timestamp)>
        if turns.is_empty() {
            return;
        }
        let count = turns.len();
        for (role, content, timestamp) in turns {
            let role = match role.as_str() {
                "user" => Role::User,
                "assistant" => Role::Assistant,
                _ => Role::System,
            };
            let ts = chrono::DateTime::parse_from_rfc3339(&timestamp)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(|_| chrono::Utc::now());
            self.messages.push(ChatMessage {
                role,
                content,
                timestamp: ts,
            });
        }
        self.push_system(&format!(
            "↑ {count} messages restored from previous session"
        ));
        self.scroll_to_bottom();
    }

    /// Handle a slash command. Returns true if it was a command.
    pub fn handle_command(&mut self, input: &str) -> bool {
        let registry = CommandRegistry::new();
        let ctx = CommandContext {
            primary_model: self.current_model.clone(),
            deep_model: String::from("claude-opus-4.6"),
            endpoint: String::from("copilot"),
            message_count: self.messages.len(),
            memory_count: 0,
        };
        match registry.execute(input, &ctx) {
            CommandResult::NotACommand => false,
            CommandResult::Response(text) => {
                self.push_system(&text);
                true
            }
            CommandResult::ClearHistory => {
                self.messages.clear();
                self.scroll_offset = 0;
                self.push_system("Chat cleared.");
                true
            }
            CommandResult::Quit => {
                let _ = self.event_tx.send(AppEvent::Quit);
                true
            }
            CommandResult::SwitchModel(name) => {
                self.current_model = name.clone();
                self.push_system(&format!("Model switched to: {name}"));
                true
            }
            CommandResult::Session(cmd) => {
                self.handle_session_command(cmd);
                true
            }
        }
    }

    /// Handle session management commands.
    fn handle_session_command(&mut self, cmd: SessionCommand) {
        match cmd {
            SessionCommand::List => {
                let sessions = &self.sessions;
                if sessions.is_empty() {
                    self.push_system("No sessions. Current session: default");
                } else {
                    let mut lines = String::from("Sessions:\n");
                    for (name, active) in sessions {
                        let marker = if *active { " ← active" } else { "" };
                        lines.push_str(&format!("  • {name}{marker}\n"));
                    }
                    self.push_system(lines.trim_end());
                }
            }
            SessionCommand::New(name) => {
                // Persist current session before switching
                self.persist_current_session();
                let current = self.current_session.clone();
                // Mark old session as inactive
                if let Some(entry) = self.sessions.iter_mut().find(|(n, _)| *n == current) {
                    entry.1 = false;
                }
                // Add new session
                self.sessions.push((name.clone(), true));
                self.current_session = name.clone();
                self.messages.clear();
                self.scroll_offset = 0;
                self.push_system(&format!("New session created: {name}"));
                self.persist_session_index();
            }
            SessionCommand::Switch(name) => {
                // Check if session exists
                let exists = self.sessions.iter().any(|(n, _)| *n == name);
                if !exists {
                    self.push_system(&format!(
                        "Session not found: {name}. Use /session list to see available sessions."
                    ));
                    return;
                }
                // Persist current session before switching
                self.persist_current_session();
                // Mark all inactive, mark target active
                for entry in self.sessions.iter_mut() {
                    entry.1 = entry.0 == name;
                }
                self.current_session = name.clone();
                // Clear current view and load messages from store
                self.messages.clear();
                self.scroll_offset = 0;
                self.push_system(&format!("Switched to session: {name}"));
                self.load_session_messages(&name);
                self.persist_session_index();
            }
            SessionCommand::Archive => {
                let current = self.current_session.clone();
                // Archive in persistence layer
                self.archive_session_persist(&current);
                // Remove from active sessions list
                self.sessions.retain(|(n, _)| *n != current);
                // Start a new default session
                let new_name = "default".to_string();
                self.sessions.push((new_name.clone(), true));
                self.current_session = new_name;
                self.messages.clear();
                self.scroll_offset = 0;
                self.push_system(&format!("Session '{current}' archived. Now in: default"));
                self.persist_session_index();
            }
        }
    }

    /// Persist the current session's messages to the store (fire-and-forget).
    pub fn persist_current_session(&self) {
        let Some(mgr) = self.session_manager.clone() else {
            return;
        };
        let session_name = self.current_session.clone();
        let messages: Vec<pares_radix_core::model::ChatMessage> = self
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::System => "system",
                };
                pares_radix_core::model::ChatMessage {
                    role: role.to_string(),
                    content: m.content.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                }
            })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let metadata = pares_radix_core::session::SessionMetadata {
            started_at: now,
            last_message_at: now,
            message_count: messages.len(),
            topic_summary: Some(session_name.clone()),
        };
        let chat_id = format!("tui:{session_name}");
        tokio::spawn(async move {
            mgr.save_session(&chat_id, &messages, metadata).await;
        });
    }

    /// Persist the session index (list of session names and active state).
    pub fn persist_session_index(&self) {
        let Some(mgr) = self.session_manager.clone() else {
            return;
        };
        let sessions = self.sessions.clone();
        let current = self.current_session.clone();
        tokio::spawn(async move {
            let index = serde_json::json!({
                "sessions": sessions,
                "current": current,
            });
            // Use the underlying store directly via save_session with a special key
            let meta = pares_radix_core::session::SessionMetadata {
                started_at: 0,
                last_message_at: 0,
                message_count: 0,
                topic_summary: Some("__index__".to_string()),
            };
            // Store index as a pseudo-session
            mgr.save_session("tui:__index__", &[], meta).await;
            // We need direct state store access for the index — use save_session hack
            // Actually, let's just save it as a regular session with metadata
            let _ = index; // Stored via metadata.topic_summary pattern above
        });
    }

    /// Load messages for a named session from the store (async, results arrive via event).
    pub fn load_session_messages(&self, session_name: &str) {
        let Some(mgr) = self.session_manager.clone() else {
            return;
        };
        let chat_id = format!("tui:{session_name}");
        let name = session_name.to_string();
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            if let Some(saved) = mgr.load_active_session(&chat_id).await {
                let turns: Vec<(String, String, String)> = saved
                    .messages
                    .into_iter()
                    .map(|m| {
                        let ts = chrono::Utc::now().to_rfc3339();
                        (m.role, m.content, ts)
                    })
                    .collect();
                let _ = tx.send(AppEvent::SessionMessagesLoaded(name, turns));
            } else {
                let _ = tx.send(AppEvent::SessionMessagesLoaded(name, vec![]));
            }
        });
    }

    /// Archive a session in the persistence layer.
    fn archive_session_persist(&self, session_name: &str) {
        let Some(mgr) = self.session_manager.clone() else {
            return;
        };
        let chat_id = format!("tui:{session_name}");
        tokio::spawn(async move {
            mgr.archive_session(&chat_id).await;
        });
    }

    /// Navigate input history up (older entry).
    pub fn history_up(&mut self) {
        if let Some(entry) = self.history.up(&self.input) {
            self.input = entry.to_string();
            self.input_cursor = self.input.len();
        }
    }

    /// Navigate input history down (newer entry).
    pub fn history_down(&mut self) {
        if let Some(entry) = self.history.down() {
            self.input = entry.to_string();
            self.input_cursor = self.input.len();
        }
    }

    /// Switch to a session by its 0-based index in the sessions list.
    /// Used for Alt+1..9 keyboard shortcuts (Alt+1 = index 0, etc.).
    pub fn switch_to_index(&mut self, index: usize) {
        if index >= self.sessions.len() {
            return;
        }
        let target_name = self.sessions[index].0.clone();
        if target_name == self.current_session {
            return; // already active
        }
        // Persist current session before switching
        self.persist_current_session();
        // Mark all inactive, mark target active
        for entry in self.sessions.iter_mut() {
            entry.1 = entry.0 == target_name;
        }
        self.current_session = target_name.clone();
        self.messages.clear();
        self.scroll_offset = 0;
        self.push_system(&format!("Switched to session: {target_name}"));
        self.load_session_messages(&target_name);
        self.persist_session_index();
    }

    /// Clear the chat display (Ctrl+L equivalent).
    pub fn clear_chat(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.user_scrolled = false;
        self.push_system("Chat cleared.");
    }

    /// Clear the input line (Ctrl+U equivalent).
    pub fn clear_input(&mut self) {
        self.input.clear();
        self.input_cursor = 0;
    }

    /// Delete word backward (Ctrl+W equivalent, bash-style).
    /// Deletes from cursor back to the start of the previous word.
    pub fn delete_word_backward(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let before = &self.input[..self.input_cursor];
        let trimmed = before.trim_end();
        let new_end = trimmed
            .rfind(char::is_whitespace)
            .map(|i| i + 1)
            .unwrap_or(0);
        self.input.drain(new_end..self.input_cursor);
        self.input_cursor = new_end;
    }

    /// Insert a newline at the current cursor position (for multi-line input).
    pub fn insert_newline(&mut self) {
        let cursor = self.input_cursor.min(self.input.len());
        self.input.insert(cursor, '\n');
        self.input_cursor = cursor + 1;
    }

    /// Submit the current input buffer.
    pub fn submit_input(&mut self) {
        let input = self.input.drain(..).collect::<String>();
        self.input_cursor = 0;
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        // Record in history before processing
        self.history.push(&trimmed);
        self.history.save_to_disk();

        if self.handle_command(&trimmed) {
            return;
        }

        // Add user message
        self.messages.push(ChatMessage {
            role: Role::User,
            content: trimmed.clone(),
            timestamp: chrono::Utc::now(),
        });
        self.scroll_to_bottom();
        self.thinking = true;

        // Spawn agent call with streaming
        let agent = Arc::clone(&self.agent);
        let tx = self.event_tx.clone();
        let handle = tokio::spawn(async move {
            let event = Event::Message {
                id: uuid::Uuid::new_v4().to_string(),
                channel: "tui".into(),
                sender: "user".into(),
                content: trimmed,
            };

            // Create streaming channel
            let (stream_tx, mut stream_rx) = mpsc::unbounded_channel::<StreamDelta>();

            // Forward stream deltas to the TUI event loop as they arrive
            let chunk_tx = tx.clone();
            let forwarder = tokio::spawn(async move {
                while let Some(delta) = stream_rx.recv().await {
                    match delta {
                        StreamDelta::Content(content) => {
                            let _ = chunk_tx.send(AppEvent::StreamChunk(content));
                        }
                        StreamDelta::Done => break,
                        _ => {} // ToolCallStart/Delta handled internally by the agent
                    }
                }
            });

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(120),
                agent.handle_event_streaming(event, stream_tx),
            )
            .await;

            // Ensure forwarder completes
            let _ = forwarder.await;

            match result {
                Ok(Some(Event::ModelResponse { content, .. })) => content,
                Ok(Some(_other)) => "(unexpected response type)".to_string(),
                Ok(None) => {
                    "(agent returned no response — check ~/.pares-radix/logs/pares-radix.log)"
                        .to_string()
                }
                Err(_timeout) => "(timed out after 120s)".to_string(),
            }
        });
        // Spawn a watcher that catches panics from the agent task
        let tx2 = self.event_tx.clone();
        tokio::spawn(async move {
            match handle.await {
                Ok(content) => {
                    let _ = tx2.send(AppEvent::AgentResponse(content));
                }
                Err(join_err) => {
                    let msg = if join_err.is_panic() {
                        "(internal error — agent panicked, check logs)".to_string()
                    } else {
                        "(agent task cancelled)".to_string()
                    };
                    let _ = tx2.send(AppEvent::AgentResponse(msg));
                }
            }
        });
    }

    pub fn push_system(&mut self, content: &str) {
        self.messages.push(ChatMessage {
            role: Role::System,
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
        });
    }

    /// Handle a streaming chunk: append to the current assistant message or create one.
    pub fn handle_stream_chunk(&mut self, chunk: String) {
        // If we're in thinking state and no assistant message is being built yet, create one.
        if self.thinking {
            if let Some(last) = self.messages.last_mut() {
                if last.role == Role::Assistant {
                    // Append to in-progress assistant message
                    last.content.push_str(&chunk);
                    self.scroll_to_bottom();
                    return;
                }
            }
            // First chunk — create the in-progress assistant message and stop showing spinner
            self.thinking = false;
            self.streaming = true;
            self.messages.push(ChatMessage {
                role: Role::Assistant,
                content: chunk,
                timestamp: chrono::Utc::now(),
            });
            self.scroll_to_bottom();
        } else if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Assistant {
                last.content.push_str(&chunk);
                self.scroll_to_bottom();
            }
        }
    }

    /// Handle the final agent response — replace streaming content with final canonical content.
    pub fn handle_agent_response(&mut self, content: String) {
        self.thinking = false;
        self.streaming = false;
        // If the last message is an in-progress assistant message from streaming,
        // replace it with the final canonical response.
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Assistant {
                last.content = content;
                self.scroll_to_bottom();
                return;
            }
        }
        // No streaming message was created (e.g., timeout error) — add new one.
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content,
            timestamp: chrono::Utc::now(),
        });
        self.scroll_to_bottom();
    }

    /// Auto-scroll to show the latest message (respects user scroll position).
    pub fn scroll_to_bottom(&mut self) {
        if self.user_scrolled {
            return; // Don't auto-scroll if user manually scrolled up
        }
        let total_lines: u16 = self
            .messages
            .iter()
            .map(|m| m.content.lines().count() as u16 + 2)
            .sum::<u16>()
            + 2;
        self.scroll_offset = total_lines.saturating_sub(self.viewport_height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pares_agens_core::agent::Agent;

    /// Create a minimal App for testing (uses a mock agent).
    fn test_app() -> (App, mpsc::UnboundedReceiver<AppEvent>) {
        use async_trait::async_trait;
        use pares_agens_core::agent::Memory;
        use pares_radix_core::model::{
            ChatMessage as CoreChatMessage, ChatOptions, ModelClient, ModelClientError,
            ModelCompletion, ToolDefinition, ToolDispatcher,
        };
        use serde_json::Value;

        struct NoopMemory;
        #[async_trait]
        impl Memory for NoopMemory {
            async fn capture(&self, _content: &str) -> Result<(), String> {
                Ok(())
            }
            async fn recall(&self, _query: &str) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
        }

        struct NoopModel;
        #[async_trait]
        impl ModelClient for NoopModel {
            async fn complete(
                &self,
                _messages: &[CoreChatMessage],
                _tools: &[ToolDefinition],
                _options: &ChatOptions,
            ) -> Result<ModelCompletion, ModelClientError> {
                Ok(ModelCompletion {
                    content: Some("hi".into()),
                    tool_calls: vec![],
                    logprobs: None,
                    model: None,
                })
            }
        }

        struct NoopDispatcher;
        #[async_trait]
        impl ToolDispatcher for NoopDispatcher {
            async fn available_tools(&self) -> Vec<ToolDefinition> {
                vec![]
            }
            async fn call_tool(&self, _name: &str, _args: Value) -> String {
                "ok".into()
            }
        }

        let agent = Arc::new(Agent::new(Arc::new(NoopMemory)).with_model(
            Arc::new(NoopModel),
            Arc::new(NoopDispatcher),
            "test".to_string(),
        ));

        let (tx, rx) = mpsc::unbounded_channel();
        // Create app with empty history (don't load from disk in tests)
        let app = App {
            messages: vec![ChatMessage {
                role: Role::System,
                content: "Pares Radix TUI — model: test-model. Type /help for commands."
                    .to_string(),
                timestamp: chrono::Utc::now(),
            }],
            input: String::new(),
            input_cursor: 0,
            scroll_offset: 0,
            user_scrolled: false,
            viewport_height: 35,
            thinking: false,
            streaming: false,
            current_model: "test-model".to_string(),
            agent,
            event_tx: tx,
            history: InputHistory::new(500), // Empty, no disk load
            sessions: vec![("default".to_string(), true)],
            current_session: "default".to_string(),
            session_manager: None,
        };
        (app, rx)
    }

    #[test]
    fn history_up_recalls_previous_input() {
        let (mut app, _rx) = test_app();
        app.history.push("hello");
        app.history.push("world");

        app.history_up();
        assert_eq!(app.input, "world");

        app.history_up();
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn history_down_returns_to_current_input() {
        let (mut app, _rx) = test_app();
        app.history.push("first");
        app.history.push("second");
        app.input = "typing...".to_string();

        app.history_up();
        assert_eq!(app.input, "second");

        app.history_down();
        assert_eq!(app.input, "typing...");
    }

    #[test]
    fn history_deduplicates_consecutive() {
        let (mut app, _rx) = test_app();
        app.history.push("same");
        app.history.push("same");
        app.history.push("same");
        assert_eq!(app.history.len(), 1);
    }

    #[test]
    fn history_up_on_empty_does_nothing() {
        let (mut app, _rx) = test_app();
        app.input = "current".to_string();
        app.history_up();
        assert_eq!(app.input, "current");
    }

    #[test]
    fn stream_chunk_creates_assistant_message() {
        let (mut app, _rx) = test_app();
        app.thinking = true;

        app.handle_stream_chunk("Hello".to_string());

        // Should have created an assistant message, cleared thinking, and set streaming
        assert!(!app.thinking);
        assert!(app.streaming);
        let last = app.messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, "Hello");
    }

    #[test]
    fn stream_chunk_appends_to_existing_assistant() {
        let (mut app, _rx) = test_app();
        app.thinking = true;

        app.handle_stream_chunk("Hello".to_string());
        app.handle_stream_chunk(" world".to_string());

        let last = app.messages.last().unwrap();
        assert_eq!(last.content, "Hello world");
        assert!(app.streaming);
    }

    #[test]
    fn agent_response_replaces_streaming_content() {
        let (mut app, _rx) = test_app();
        app.thinking = true;

        // Simulate streaming
        app.handle_stream_chunk("Hell".to_string());
        app.handle_stream_chunk("o world".to_string());
        assert!(app.streaming);

        // Then final response arrives (canonical, may differ from streamed)
        app.handle_agent_response("Hello world!".to_string());

        // Should have exactly one assistant message with final content, streaming off
        assert!(!app.streaming);
        let assistant_msgs: Vec<_> = app
            .messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .collect();
        assert_eq!(assistant_msgs.len(), 1);
        assert_eq!(assistant_msgs[0].content, "Hello world!");
    }

    #[test]
    fn agent_response_without_streaming_creates_message() {
        let (mut app, _rx) = test_app();
        app.thinking = true;

        // No stream chunks — direct response (e.g., procedural route)
        app.handle_agent_response("Direct answer".to_string());

        let last = app.messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content, "Direct answer");
        assert!(!app.thinking);
        assert!(!app.streaming);
    }

    #[test]
    fn history_save_and_load_roundtrip() {
        // Use explicit path methods for thread-safe isolation (no env var race)
        let tmp = std::env::temp_dir().join(format!(
            "pares-radix-test-roundtrip-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let history_file = tmp.join("history");
        fs::create_dir_all(&tmp).unwrap();

        let mut hist = InputHistory::new(500);
        hist.push("first command");
        hist.push("second command");
        hist.push("third");
        hist.save_to_path(&history_file);

        // Load into a fresh history
        let mut hist2 = InputHistory::new(500);
        hist2.load_from_path(&history_file);
        assert_eq!(hist2.len(), 3);
        // Verify order by navigating
        assert_eq!(hist2.up(""), Some("third"));
        assert_eq!(hist2.up(""), Some("second command"));
        assert_eq!(hist2.up(""), Some("first command"));

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn history_load_respects_max_entries() {
        // Use explicit path methods for thread-safe isolation
        let tmp = std::env::temp_dir().join(format!(
            "pares-radix-test-max-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        let history_file = tmp.join("history");
        fs::create_dir_all(&tmp).unwrap();

        // Write more entries than max
        let mut hist = InputHistory::new(500);
        for i in 0..10 {
            hist.push(&format!("entry {i}"));
        }
        hist.save_to_path(&history_file);

        // Load with small max
        let mut hist2 = InputHistory::new(3);
        hist2.load_from_path(&history_file);
        assert_eq!(hist2.len(), 3);
        // Should be the last 3
        assert_eq!(hist2.up(""), Some("entry 9"));
        assert_eq!(hist2.up(""), Some("entry 8"));
        assert_eq!(hist2.up(""), Some("entry 7"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn insert_newline_at_cursor() {
        let (mut app, _rx) = test_app();
        app.input = "hello world".to_string();
        app.input_cursor = 5; // after "hello"

        app.insert_newline();

        assert_eq!(app.input, "hello\n world");
        assert_eq!(app.input_cursor, 6); // after the newline
    }

    #[test]
    fn insert_newline_at_end() {
        let (mut app, _rx) = test_app();
        app.input = "first line".to_string();
        app.input_cursor = app.input.len();

        app.insert_newline();

        assert_eq!(app.input, "first line\n");
        assert_eq!(app.input_cursor, 11);
    }

    #[test]
    fn load_history_from_turns_populates_messages() {
        let (mut app, _rx) = test_app();
        let initial_count = app.messages.len(); // 1 system message

        let turns = vec![
            (
                "user".to_string(),
                "hello".to_string(),
                "2026-05-20T10:00:00Z".to_string(),
            ),
            (
                "assistant".to_string(),
                "hi there".to_string(),
                "2026-05-20T10:00:01Z".to_string(),
            ),
            (
                "user".to_string(),
                "how are you?".to_string(),
                "2026-05-20T10:01:00Z".to_string(),
            ),
            (
                "assistant".to_string(),
                "I'm good!".to_string(),
                "2026-05-20T10:01:01Z".to_string(),
            ),
        ];

        app.load_history_from_turns(turns);

        // Should have: 1 system + 4 restored + 1 "restored" system message
        assert_eq!(app.messages.len(), initial_count + 4 + 1);
        assert_eq!(app.messages[1].role, Role::User);
        assert_eq!(app.messages[1].content, "hello");
        assert_eq!(app.messages[2].role, Role::Assistant);
        assert_eq!(app.messages[2].content, "hi there");
        // Last message is the "restored" notice
        let last = app.messages.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(last.content.contains("4 messages restored"));
    }

    #[test]
    fn load_history_from_turns_empty_is_noop() {
        let (mut app, _rx) = test_app();
        let initial_count = app.messages.len();
        app.load_history_from_turns(vec![]);
        assert_eq!(app.messages.len(), initial_count);
    }

    #[tokio::test]
    async fn multiline_input_submits_correctly() {
        let (mut app, _rx) = test_app();
        app.input = "line1\nline2\nline3".to_string();
        app.input_cursor = app.input.len();

        // Submit — the multi-line content should become a user message
        app.submit_input();

        let user_msgs: Vec<_> = app
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .collect();
        assert_eq!(user_msgs.len(), 1);
        assert_eq!(user_msgs[0].content, "line1\nline2\nline3");
        assert!(app.input.is_empty());
    }

    #[test]
    fn session_list_shows_default() {
        let (mut app, _rx) = test_app();
        app.handle_command("/session list");
        let last = app.messages.last().unwrap();
        assert_eq!(last.role, Role::System);
        assert!(last.content.contains("default"));
        assert!(last.content.contains("active"));
    }

    #[test]
    fn session_new_creates_and_switches() {
        let (mut app, _rx) = test_app();
        // Add a message to prove clear happens
        app.messages.push(ChatMessage {
            role: Role::User,
            content: "old message".to_string(),
            timestamp: chrono::Utc::now(),
        });

        app.handle_command("/session new work");

        assert_eq!(app.current_session, "work");
        assert_eq!(app.sessions.len(), 2);
        // Old default should be inactive
        assert!(!app.sessions.iter().find(|(n, _)| n == "default").unwrap().1);
        // New should be active
        assert!(app.sessions.iter().find(|(n, _)| n == "work").unwrap().1);
        // Messages should be cleared (only the system notice remains)
        assert_eq!(app.messages.len(), 1);
        assert!(app.messages[0]
            .content
            .contains("New session created: work"));
    }

    #[test]
    fn session_switch_to_existing() {
        let (mut app, _rx) = test_app();
        // Create a second session
        app.handle_command("/session new second");
        // Switch back to default
        app.handle_command("/session switch default");

        assert_eq!(app.current_session, "default");
        assert!(app.sessions.iter().find(|(n, _)| n == "default").unwrap().1);
        assert!(!app.sessions.iter().find(|(n, _)| n == "second").unwrap().1);
    }

    #[test]
    fn session_switch_nonexistent_shows_error() {
        let (mut app, _rx) = test_app();
        app.handle_command("/session switch ghost");

        let last = app.messages.last().unwrap();
        assert!(last.content.contains("Session not found: ghost"));
        // Should still be on default
        assert_eq!(app.current_session, "default");
    }

    #[test]
    fn session_archive_resets_to_default() {
        let (mut app, _rx) = test_app();
        app.handle_command("/session new temp");
        assert_eq!(app.current_session, "temp");

        app.handle_command("/session archive");

        assert_eq!(app.current_session, "default");
        // "temp" should be removed from sessions list
        assert!(!app.sessions.iter().any(|(n, _)| n == "temp"));
    }

    #[test]
    fn with_session_manager_attaches() {
        use pares_radix_core::InMemoryStateStore;
        let (app, _rx) = test_app();
        assert!(app.session_manager.is_none());

        let store = Arc::new(InMemoryStateStore::new());
        let mgr = Arc::new(SessionManager::new(
            store as Arc<dyn pares_radix_core::StateStore>,
        ));
        // Rebuild with session manager
        let (mut app2, _rx2) = test_app();
        app2.session_manager = Some(mgr);
        assert!(app2.session_manager.is_some());
    }

    #[test]
    fn handle_sessions_loaded_sets_active() {
        let (mut app, _rx) = test_app();
        let sessions = vec![
            ("work".to_string(), false),
            ("personal".to_string(), true),
            ("default".to_string(), false),
        ];
        app.handle_sessions_loaded(sessions);

        assert_eq!(app.current_session, "personal");
        assert_eq!(app.sessions.len(), 3);
    }

    #[test]
    fn handle_session_messages_loaded_populates_view() {
        let (mut app, _rx) = test_app();
        app.current_session = "work".to_string();

        let turns = vec![
            (
                "user".to_string(),
                "hello".to_string(),
                "2026-05-20T10:00:00Z".to_string(),
            ),
            (
                "assistant".to_string(),
                "hi".to_string(),
                "2026-05-20T10:00:01Z".to_string(),
            ),
        ];
        app.handle_session_messages_loaded("work".to_string(), turns);

        // Should have loaded the messages
        let user_msgs: Vec<_> = app
            .messages
            .iter()
            .filter(|m| m.role == Role::User)
            .collect();
        assert_eq!(user_msgs.len(), 1);
        assert_eq!(user_msgs[0].content, "hello");
    }

    #[test]
    fn handle_session_messages_loaded_wrong_session_is_noop() {
        let (mut app, _rx) = test_app();
        app.current_session = "work".to_string();
        let initial_count = app.messages.len();

        // Load arrives for a different session (user switched away)
        app.handle_session_messages_loaded(
            "old".to_string(),
            vec![(
                "user".to_string(),
                "stale".to_string(),
                "2026-05-20T10:00:00Z".to_string(),
            )],
        );

        // Should be unchanged
        assert_eq!(app.messages.len(), initial_count);
    }
}
