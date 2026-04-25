//! TUI application state and event loop.

use std::sync::Arc;
use tokio::sync::mpsc;

use pares_agens_core::agent::Agent;
use pares_agens_core::Event;

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
    /// Agent finished responding.
    AgentResponse(String),
    /// Terminal resize or redraw needed.
    Redraw,
    /// Quit the application.
    Quit,
}

/// Application state.
pub struct App {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub input_cursor: usize,
    pub scroll_offset: u16,
    pub thinking: bool,
    pub current_model: String,
    pub agent: Arc<Agent>,
    pub event_tx: mpsc::UnboundedSender<AppEvent>,
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
                content: format!("Pares Agens TUI — model: {model_name}. Type /help for commands."),
                timestamp: chrono::Utc::now(),
            }],
            input: String::new(),
            input_cursor: 0,
            scroll_offset: 0,
            thinking: false,
            current_model: model_name,
            agent,
            event_tx,
        }
    }

    /// Handle a slash command. Returns true if it was a command.
    pub fn handle_command(&mut self, input: &str) -> bool {
        let trimmed = input.trim();
        match trimmed {
            "/quit" | "/exit" | "/q" => {
                let _ = self.event_tx.send(AppEvent::Quit);
                return true;
            }
            "/clear" => {
                self.messages.clear();
                self.push_system("Chat cleared.");
                self.scroll_offset = 0;
                return true;
            }
            "/status" => {
                let status = format!(
                    "Model: {}\nMessages: {}\nThinking: {}",
                    self.current_model,
                    self.messages.len(),
                    self.thinking
                );
                self.push_system(&status);
                return true;
            }
            "/help" => {
                self.push_system(
                    "Commands:\n  /model <name> — switch model\n  /status — show status\n  /clear — clear chat\n  /quit — exit\n  /help — this message",
                );
                return true;
            }
            _ => {}
        }

        if let Some(model) = trimmed.strip_prefix("/model ") {
            let model = model.trim();
            if model.is_empty() {
                self.push_system(&format!("Current model: {}", self.current_model));
            } else {
                self.current_model = model.to_string();
                self.push_system(&format!("Model switched to: {model}"));
            }
            return true;
        }

        if trimmed == "/model" {
            self.push_system(&format!("Current model: {}", self.current_model));
            return true;
        }

        false
    }

    /// Submit the current input buffer.
    pub fn submit_input(&mut self) {
        let input = self.input.drain(..).collect::<String>();
        self.input_cursor = 0;
        let trimmed = input.trim().to_string();
        if trimmed.is_empty() {
            return;
        }

        if self.handle_command(&trimmed) {
            return;
        }

        // Add user message
        self.messages.push(ChatMessage {
            role: Role::User,
            content: trimmed.clone(),
            timestamp: chrono::Utc::now(),
        });
        self.thinking = true;

        // Spawn agent call
        let agent = Arc::clone(&self.agent);
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let event = Event::Message {
                id: uuid::Uuid::new_v4().to_string(),
                channel: "tui".into(),
                sender: "user".into(),
                content: trimmed,
            };
            match agent.handle_event(event).await {
                Some(Event::ModelResponse { content, .. }) => {
                    let _ = tx.send(AppEvent::AgentResponse(content));
                }
                _ => {
                    let _ = tx.send(AppEvent::AgentResponse(
                        "(no response from agent)".to_string(),
                    ));
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

    pub fn handle_agent_response(&mut self, content: String) {
        self.thinking = false;
        self.messages.push(ChatMessage {
            role: Role::Assistant,
            content,
            timestamp: chrono::Utc::now(),
        });
    }
}
