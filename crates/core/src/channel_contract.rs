//! Channel contracts — declarative capability descriptions for output channels.
//!
//! Each channel (Telegram, Discord, etc.) registers a [`ChannelContract`] that
//! describes its rendering constraints (max message length, parse modes, rate
//! limits, feature support).  The event spine's delivery procedures read these
//! contracts to decide how to format and chunk outbound messages.

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Declarative description of a channel's rendering capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelContract {
    /// Channel identifier, e.g. `"telegram"`, `"discord"`.
    pub channel: String,
    /// Maximum message length in characters.
    pub max_message_len: usize,
    /// Supported parse/format modes (e.g. `["HTML", "MarkdownV2", "plain"]`).
    pub parse_modes: Vec<String>,
    /// Preferred output format.
    pub preferred_format: String,
    /// Fallback format if the preferred one fails.
    pub fallback_format: String,
    /// Maximum sends per second before rate-limiting kicks in.
    pub rate_limit_per_sec: f64,
    /// Whether the channel supports inline keyboard buttons.
    pub supports_inline_buttons: bool,
    /// Whether the channel supports message reactions.
    pub supports_reactions: bool,
    /// Whether the channel supports threaded replies.
    pub supports_threads: bool,
    /// Whether the channel supports editing sent messages for progressive updates.
    pub supports_message_edit: bool,
    /// Maximum time (ms) before the channel must show *something* to the user.
    pub initial_response_timeout_ms: u64,
    /// Minimum interval (ms) between message edits to avoid rate limits.
    pub edit_throttle_ms: u64,
    /// Whether to show a typing indicator while processing.
    pub typing_indicator: bool,
}

impl ChannelContract {
    /// Return the default contract for the Telegram channel.
    pub fn telegram() -> Self {
        Self {
            channel: "telegram".to_string(),
            max_message_len: 4096,
            parse_modes: vec![
                "HTML".to_string(),
                "MarkdownV2".to_string(),
                "plain".to_string(),
            ],
            preferred_format: "HTML".to_string(),
            fallback_format: "plain".to_string(),
            rate_limit_per_sec: 30.0,
            supports_inline_buttons: true,
            supports_reactions: true,
            supports_threads: true,
            supports_message_edit: true,
            initial_response_timeout_ms: 30_000,
            edit_throttle_ms: 500,
            typing_indicator: true,
        }
    }

    /// Serialize this contract into a JSON value suitable for PluresDB storage.
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "_type": "agens:channel_contract",
            "channel": self.channel,
            "max_message_len": self.max_message_len,
            "parse_modes": self.parse_modes,
            "preferred_format": self.preferred_format,
            "fallback_format": self.fallback_format,
            "rate_limit_per_sec": self.rate_limit_per_sec,
            "supports_inline_buttons": self.supports_inline_buttons,
            "supports_reactions": self.supports_reactions,
            "supports_threads": self.supports_threads,
            "supports_message_edit": self.supports_message_edit,
            "initial_response_timeout_ms": self.initial_response_timeout_ms,
            "edit_throttle_ms": self.edit_throttle_ms,
            "typing_indicator": self.typing_indicator,
        })
    }

    /// Attempt to deserialize a contract from a PluresDB node's JSON data.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        if value.get("_type")?.as_str()? != "agens:channel_contract" {
            return None;
        }
        serde_json::from_value(value.clone()).ok()
    }
}

/// Policy governing bot participation in group chats.
///
/// Controls when the bot responds vs. passively observes in multi-user
/// conversations. In 1:1 chats, the bot always responds regardless of policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupChatPolicy {
    /// Respond when the bot is @-mentioned.
    pub respond_on_mention: bool,
    /// Respond when a user replies to a previous bot message.
    pub respond_on_reply: bool,
    /// Respond when the message starts with this prefix (e.g. `"!"`).
    pub respond_on_prefix: Option<String>,
    /// Store messages the bot doesn't respond to for conversational context.
    pub passive_observe: bool,
    /// Number of recent messages to keep per chat for context injection.
    pub context_window: usize,
    /// Maximum proactive (unprompted) messages per hour in group chats.
    pub max_unprompted_per_hour: u8,
}

impl Default for GroupChatPolicy {
    fn default() -> Self {
        Self {
            respond_on_mention: true,
            respond_on_reply: true,
            respond_on_prefix: None,
            passive_observe: true,
            context_window: 50,
            max_unprompted_per_hour: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_contract_roundtrip() {
        let contract = ChannelContract::telegram();
        let json = contract.to_json();
        let back = ChannelContract::from_json(&json).expect("should deserialize");
        assert_eq!(back.channel, "telegram");
        assert_eq!(back.max_message_len, 4096);
        assert_eq!(back.preferred_format, "HTML");
    }
}
