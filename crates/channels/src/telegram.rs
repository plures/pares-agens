//! Telegram channel adapter using [teloxide](https://github.com/teloxide/teloxide).
//!
//! # Features
//! - Receive text messages → emit [`Event::Message`] events
//! - Send responses with Telegram MarkdownV2 formatting
//! - Support inline keyboard buttons for Praxis decision gates
//! - Handle photos and documents (passed as attachment metadata in event content)
//! - Bot token supplied via [`TelegramConfig`] (not env vars)
//! - Graceful reconnection handled by teloxide's built-in polling retry
//!
//! # Example
//! ```no_run
//! use pares_agens_channels::telegram::{TelegramAdapter, TelegramConfig};
//!
//! let config = TelegramConfig::new("123456:ABC-token");
//! let adapter = TelegramAdapter::new(config);
//! ```

use async_trait::async_trait;
use pares_agens_core::Event;
use teloxide::{
    payloads::SendMessageSetters,
    prelude::*,
    types::{
        InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageKind, ParseMode,
    },
};
use tracing::{debug, error, info};
use uuid::Uuid;

use crate::adapter::{ChannelAdapter, ChannelError};

/// Configuration for the Telegram adapter.
///
/// The bot token should be stored in PluresDB state and passed here at
/// runtime — never hard-coded or read from environment variables.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    /// Telegram bot token (from BotFather).
    pub token: String,
}

impl TelegramConfig {
    /// Create a new [`TelegramConfig`] with the given bot token.
    pub fn new(token: impl Into<String>) -> Self {
        Self { token: token.into() }
    }
}

/// A Telegram channel adapter that bridges Telegram messages to the agent event loop.
///
/// Receives messages from Telegram via long-polling and emits [`Event::Message`]
/// events. Sends [`Event::ModelResponse`] content back as MarkdownV2-formatted
/// Telegram messages.
pub struct TelegramAdapter {
    config: TelegramConfig,
}

impl TelegramAdapter {
    /// Create a new [`TelegramAdapter`] with the given configuration.
    pub fn new(config: TelegramConfig) -> Self {
        Self { config }
    }

    /// Convert a Telegram [`Message`] into an agent [`Event`].
    ///
    /// Text messages become `Event::Message`. Photos and documents include
    /// their file IDs in the content so the agent can reference them.
    /// Returns `None` for unsupported message types.
    pub fn message_to_event(msg: &Message) -> Option<Event> {
        let from = msg
            .from
            .as_ref()
            .map(|u| {
                u.username
                    .as_deref()
                    .unwrap_or(&u.first_name)
                    .to_string()
            })
            .unwrap_or_else(|| format!("chat:{}", msg.chat.id));

        match &msg.kind {
            MessageKind::Common(common) => {
                use teloxide::types::MediaKind;
                match &common.media_kind {
                    MediaKind::Text(t) => Some(Event::Message {
                        id: Uuid::new_v4().to_string().to_string(),
                        content: t.text.clone(),
                        channel: "telegram".to_string(), sender: from,
                    }),
                    MediaKind::Photo(p) => {
                        // Use the highest-resolution photo
                        let file_id = p
                            .photo
                            .last()
                            .map(|ps| ps.file.id.clone())
                            .unwrap_or_default();
                        let caption = p
                            .caption
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        Some(Event::Message {
                            id: Uuid::new_v4().to_string().to_string(),
                            content: if caption.is_empty() {
                                format!("[photo file_id={file_id}]")
                            } else {
                                format!("[photo file_id={file_id}] {caption}")
                            },
                            channel: "telegram".to_string(), sender: from,
                        })
                    }
                    MediaKind::Document(d) => {
                        let file_id = d.document.file.id.clone();
                        let file_name = d
                            .document
                            .file_name
                            .as_deref()
                            .unwrap_or("unknown")
                            .to_string();
                        let caption = d
                            .caption
                            .as_deref()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        Some(Event::Message {
                            id: Uuid::new_v4().to_string().to_string(),
                            content: if caption.is_empty() {
                                format!("[document file_id={file_id} name={file_name}]")
                            } else {
                                format!("[document file_id={file_id} name={file_name}] {caption}")
                            },
                            channel: "telegram".to_string(), sender: from,
                        })
                    }
                    _ => {
                        debug!("unsupported media kind, ignoring");
                        None
                    }
                }
            }
            _ => {
                debug!("unsupported message kind, ignoring");
                None
            }
        }
    }

    /// Escape text for Telegram MarkdownV2 format.
    ///
    /// MarkdownV2 requires escaping of: `_ * [ ] ( ) ~ ` > # + - = | { } . !`
    pub fn escape_markdown_v2(text: &str) -> String {
        let special: &[char] = &[
            '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{',
            '}', '.', '!',
        ];
        let mut out = String::with_capacity(text.len() * 2);
        for ch in text.chars() {
            if special.contains(&ch) {
                out.push('\\');
            }
            out.push(ch);
        }
        out
    }

    /// Build an [`InlineKeyboardMarkup`] from a list of `(label, callback_data)` pairs.
    ///
    /// Used by Praxis decision gates to present approval/rejection buttons to the user.
    pub fn build_inline_keyboard(buttons: &[(&str, &str)]) -> InlineKeyboardMarkup {
        let row: Vec<InlineKeyboardButton> = buttons
            .iter()
            .map(|(label, data)| InlineKeyboardButton::callback(*label, *data))
            .collect();
        InlineKeyboardMarkup::new(vec![row])
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn run(
        &self,
        on_event: impl Fn(Event) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Event>> + Send>>
            + Send
            + Sync
            + 'static,
        ) -> Result<(), ChannelError> {
        info!("Starting Telegram adapter");
        let bot = Bot::new(self.config.token.clone());

        let handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: Message| {
                let event = Self::message_to_event(&msg);
                let response = event.map(&on_event);
                async move {
                    if let Some(fut) = response {
                        if let Some(Event::ModelResponse { content, .. }) = fut.await {
                            let escaped = TelegramAdapter::escape_markdown_v2(&content);
                            if let Err(e) = bot
                                .send_message(msg.chat.id, escaped)
                                .parse_mode(ParseMode::MarkdownV2)
                                .await
                            {
                                error!("Failed to send Telegram reply: {e}");
                            }
                        }
                    }
                    respond(())
                }
            },
        );

        Dispatcher::builder(bot, handler)
            .enable_ctrlc_handler()
            .build()
            .dispatch()
            .await;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── escape_markdown_v2 ────────────────────────────────────────────────

    #[test]
    fn escape_plain_text_unchanged() {
        assert_eq!(TelegramAdapter::escape_markdown_v2("hello world"), "hello world");
    }

    #[test]
    fn escape_special_characters() {
        let input = "Hello! Price: $5.00 — (discount 10%)";
        let escaped = TelegramAdapter::escape_markdown_v2(input);
        // '!' and '.' and '(' and ')' must be escaped
        assert!(escaped.contains("\\!"));
        assert!(escaped.contains("\\."));
        assert!(escaped.contains("\\("));
        assert!(escaped.contains("\\)"));
        // Non-special chars preserved
        assert!(escaped.contains("Hello"));
        assert!(escaped.contains("Price"));
    }

    #[test]
    fn escape_all_special_chars() {
        let specials = "_*[]()~`>#+-=|{}.!";
        let escaped = TelegramAdapter::escape_markdown_v2(specials);
        for ch in specials.chars() {
            let expected = format!("\\{ch}");
            assert!(
                escaped.contains(&expected),
                "expected '{expected}' in escaped string '{escaped}'"
            );
        }
    }

    // ── build_inline_keyboard ─────────────────────────────────────────────

    #[test]
    fn inline_keyboard_single_button() {
        let kb = TelegramAdapter::build_inline_keyboard(&[("Approve", "gate:approve:123")]);
        let rows = kb.inline_keyboard;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 1);
        assert_eq!(rows[0][0].text, "Approve");
    }

    #[test]
    fn inline_keyboard_multiple_buttons() {
        let kb = TelegramAdapter::build_inline_keyboard(&[
            ("✅ Approve", "gate:approve:42"),
            ("❌ Reject", "gate:reject:42"),
        ]);
        let rows = kb.inline_keyboard;
        assert_eq!(rows.len(), 1, "both buttons should be in one row");
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].text, "✅ Approve");
        assert_eq!(rows[0][1].text, "❌ Reject");
    }

    #[test]
    fn inline_keyboard_empty() {
        let kb = TelegramAdapter::build_inline_keyboard(&[]);
        assert!(kb.inline_keyboard[0].is_empty());
    }

    // ── TelegramAdapter basics ────────────────────────────────────────────

    #[test]
    fn adapter_name_is_telegram() {
        let adapter = TelegramAdapter::new(TelegramConfig::new("test-token"));
        assert_eq!(adapter.name(), "telegram");
    }
}
