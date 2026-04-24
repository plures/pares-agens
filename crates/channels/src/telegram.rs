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
use pares_agens_marketplace::{installer::Installer, SkillCategory, SkillMetadata};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use teloxide::{
    prelude::*,
    types::{
        CallbackQuery, ChatId, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageKind,
        ParseMode, ReactionType, ReplyParameters, User,
    },
};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::adapter::{ChannelAdapter, ChannelError};

const PARES_MODULUS_INDEX_URL: &str =
    "https://raw.githubusercontent.com/plures/pares-modulus/main/index.json";
const DEFAULT_MARKETPLACE_INSTALL_DIR: &str = "/skills";
const MAX_INDEX_LISTING_ITEMS: usize = 10;
const DEFAULT_NIX_FLAKE_DIR: &str = ".";
const DEFAULT_NIX_HOST: &str = "praxisbot";
const TELEGRAM_MAX_MESSAGE_CHARS: usize = 3900;
/// Internal prefix added by the Telegram adapter when `/verbose` is enabled.
///
/// The runtime strips this marker before model processing and uses it only to
/// decide whether to append tool execution details to the Telegram reply.
pub const TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER: &str = "__PARES_VERBOSE_TOOL_DETAILS__:";
const TELEGRAM_HELP_COMMANDS: [(&str, &str); 22] = [
    ("/start", "show this command list"),
    ("/help", "show this command list"),
    ("/status", "status + health snapshot"),
    ("/health", "alias for /status"),
    (
        "/verbose",
        "toggle inline tool execution details (or /verbose on|off)",
    ),
    (
        "/reasoning",
        "toggle deep model escalation (or /reasoning on|off)",
    ),
    ("/model", "show current primary + deep model"),
    ("/model <name>", "switch primary model at runtime"),
    ("/model deep <name>", "switch deep model at runtime"),
    (
        "/config",
        "show runtime config (model, endpoint, log level)",
    ),
    ("/config model <name>", "set runtime model"),
    ("/config endpoint <url>", "set runtime endpoint"),
    ("/config log-level <level>", "set runtime log level"),
    ("/reset", "full runtime reset (new session + config reload)"),
    ("/clear", "start a fresh conversation session"),
    ("/agents", "browse pares-modulus marketplace"),
    ("/browse", "alias for /agents"),
    ("/install <id>", "install an agent/plugin"),
    ("/logs [n]", "tail recent pares-agens service logs"),
    (
        "/update",
        "run NixOS self-update and rebuild if pares-agens changed",
    ),
    ("/approve <id>", "approve a pending elevated action by request ID"),
    ("/deny <id>", "deny a pending elevated action by request ID"),
];
const DEFAULT_LOG_TAIL_LINES: usize = 80;
const MAX_LOG_TAIL_LINES: usize = 400;

fn parse_modulus_index(payload: &str) -> Result<Vec<SkillMetadata>, String> {
    let value: Value =
        serde_json::from_str(payload).map_err(|e| format!("invalid index JSON: {e}"))?;
    let entries = match value {
        Value::Array(items) => items,
        Value::Object(map) => {
            for key in ["agents", "plugins", "items", "entries"] {
                if let Some(Value::Array(items)) = map.get(key) {
                    return Ok(items.iter().filter_map(metadata_from_index_entry).collect());
                }
            }
            return Err("index JSON must be an array or object containing agents/plugins".into());
        }
        _ => {
            return Err("index JSON must be an array or object containing agents/plugins".into());
        }
    };

    Ok(entries
        .iter()
        .filter_map(metadata_from_index_entry)
        .collect())
}

fn metadata_from_index_entry(entry: &Value) -> Option<SkillMetadata> {
    let obj = entry.as_object()?;
    let id = obj
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| obj.get("slug").and_then(Value::as_str))
        .or_else(|| obj.get("name").and_then(Value::as_str))?
        .trim()
        .to_string();
    if id.is_empty() {
        return None;
    }

    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    let version = obj
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("0.1.0")
        .to_string();
    let description = obj
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("No description provided.")
        .to_string();
    let author = obj
        .get("author")
        .and_then(Value::as_str)
        .or_else(|| obj.get("publisher").and_then(Value::as_str))
        .unwrap_or("pares-modulus")
        .to_string();
    let download_url = obj
        .get("download_url")
        .and_then(Value::as_str)
        .or_else(|| obj.get("url").and_then(Value::as_str))
        .unwrap_or("https://github.com/plures/pares-modulus")
        .to_string();
    if !download_url.starts_with("https://") {
        return None;
    }
    let checksum = obj
        .get("checksum")
        .and_then(Value::as_str)
        .or_else(|| obj.get("sha256").and_then(Value::as_str))
        .or_else(|| obj.get("digest").and_then(Value::as_str))
        .map(str::to_string)?;
    if !is_valid_sha256_hex(&checksum) {
        return None;
    }

    Some(SkillMetadata {
        id,
        name,
        version,
        description,
        author,
        categories: vec![SkillCategory::DomainSpecific("plugin".to_string())],
        checksum,
        download_url,
        signature: None,
    })
}

fn is_valid_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

async fn fetch_marketplace_index(index_url: &str) -> Result<Vec<SkillMetadata>, String> {
    let response = reqwest::get(index_url)
        .await
        .map_err(|e| format!("failed to fetch marketplace index: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "marketplace index returned HTTP {}",
            response.status()
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|e| format!("failed to read marketplace index response: {e}"))?;
    parse_modulus_index(&body)
}

fn format_index_listing(skills: &[SkillMetadata]) -> String {
    if skills.is_empty() {
        return "No agents/plugins found in pares-modulus index.".to_string();
    }

    let mut lines = vec![format!(
        "Found {} agent/plugin entries in pares-modulus:",
        skills.len()
    )];
    for skill in skills.iter().take(MAX_INDEX_LISTING_ITEMS) {
        lines.push(format!(
            "• {} ({}) — {}",
            skill.id, skill.version, skill.description
        ));
    }
    if skills.len() > MAX_INDEX_LISTING_ITEMS {
        lines.push(format!(
            "…and {} more entries.",
            skills.len() - MAX_INDEX_LISTING_ITEMS
        ));
    }
    lines.push("Install with: /install <id>".to_string());
    lines.join("\n")
}

fn find_skill_by_id(skills: &[SkillMetadata], id: &str) -> Option<SkillMetadata> {
    skills
        .iter()
        .find(|skill| skill.id.eq_ignore_ascii_case(id))
        .cloned()
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn build_nixos_update_command(flake_dir: &str, host: &str) -> String {
    let flake_dir = shell_single_quote(flake_dir);
    let host = shell_single_quote(host);
    format!(
        "set -eu; cd {flake_dir}; lock_before=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); sudo nix flake update pares-agens; lock_after=$(sha256sum flake.lock 2>/dev/null | cut -d' ' -f1 || true); if [ \"$lock_before\" != \"$lock_after\" ]; then sudo nixos-rebuild switch --flake .#{host}; echo \"Self-update applied\"; else echo \"No new pares-agens commits on main\"; fi"
    )
}

fn truncate_telegram_message(content: String) -> String {
    let mut chars = content.chars();
    let truncated: String = chars.by_ref().take(TELEGRAM_MAX_MESSAGE_CHARS).collect();
    if chars.next().is_some() {
        format!("{truncated}\n…(truncated)")
    } else {
        truncated
    }
}

/// Parse `/logs [n]` tail argument and clamp it to the allowed range.
///
/// Returns [`DEFAULT_LOG_TAIL_LINES`] when no argument is provided, or a
/// positive integer up to [`MAX_LOG_TAIL_LINES`]. Invalid values return a
/// usage string suitable for Telegram replies.
fn parse_logs_tail_lines(args: Vec<&str>) -> Result<usize, &'static str> {
    match args.as_slice() {
        [] => Ok(DEFAULT_LOG_TAIL_LINES),
        [raw] => {
            let value = raw
                .trim()
                .parse::<usize>()
                .map_err(|_| "Usage: /logs [n] (n must be a positive integer)")?;
            if value == 0 {
                return Err("Usage: /logs [n] (n must be a positive integer)");
            }
            Ok(value.min(MAX_LOG_TAIL_LINES))
        }
        _ => Err("Usage: /logs [n]"),
    }
}

/// Format `journalctl` output for Telegram delivery.
///
/// Successful output returns stdout (or a fallback message when empty). Failed
/// commands include status and stderr when available.
fn format_service_logs_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        if stdout.is_empty() {
            "No recent service logs found.".to_string()
        } else {
            stdout
        }
    } else if stderr.is_empty() {
        format!(
            "Failed to read service logs ({status}).",
            status = output.status
        )
    } else {
        format!(
            "Failed to read service logs ({status}).\n{stderr}",
            status = output.status
        )
    }
}

fn format_update_command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        if stdout.is_empty() {
            "Self-update completed.".to_string()
        } else {
            stdout
        }
    } else {
        format!(
            "Self-update failed ({status}).\n{stdout}\n{stderr}",
            status = output.status
        )
    }
}

fn telegram_help_text() -> String {
    let mut lines = vec!["Pares Agens commands:".to_string()];
    lines.extend(
        TELEGRAM_HELP_COMMANDS
            .iter()
            .map(|(command, description)| format!("{command} - {description}")),
    );
    lines.push(String::new());
    lines.push("Or just send a message.".to_string());
    lines.join("\n")
}

fn current_process_rss_kib() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        status.lines().find_map(|line| {
            let line = line.trim();
            if !line.starts_with("VmRSS:") {
                return None;
            }
            line.split_whitespace().nth(1)?.parse::<u64>().ok()
        })
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn is_user_authorized(from: &User) -> bool {
    let allowlist = std::env::var("PARES_TELEGRAM_UPDATE_ALLOWED_USERS")
        .ok()
        .unwrap_or_default();
    if allowlist.trim().is_empty() {
        return false;
    }
    let username = from.username.as_deref().unwrap_or_default();
    let user_id = from.id.0.to_string();

    allowlist
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .any(|entry| {
            let normalized = entry.trim_start_matches('@');
            normalized.eq_ignore_ascii_case(username) || normalized == user_id
        })
}

fn is_update_authorized(msg: &Message) -> bool {
    msg.from.as_ref().map(is_user_authorized).unwrap_or(false)
}

/// Configuration for the Telegram adapter.
///
/// The bot token should be stored in PluresDB state and passed here at
/// runtime — never hard-coded or read from environment variables.
#[derive(Clone)]
pub struct TelegramConfig {
    /// Telegram bot token (from BotFather).
    pub token: String,
    /// Marketplace index URL used by `/agents` and `/install`.
    pub marketplace_index_url: String,
    /// Local install directory used by marketplace installer state.
    pub marketplace_install_dir: String,
    /// Optional runtime model control for `/model`.
    pub model_control: Option<Arc<dyn TelegramModelControl>>,
    /// Optional runtime reset control for `/reset`.
    pub runtime_control: Option<Arc<dyn TelegramRuntimeControl>>,
    /// Optional runtime config control for `/config`.
    pub config_control: Option<Arc<dyn TelegramConfigControl>>,
}

impl TelegramConfig {
    /// Create a new [`TelegramConfig`] with the given bot token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            marketplace_index_url: PARES_MODULUS_INDEX_URL.to_string(),
            marketplace_install_dir: DEFAULT_MARKETPLACE_INSTALL_DIR.to_string(),
            model_control: None,
            runtime_control: None,
            config_control: None,
        }
    }

    /// Override marketplace index URL.
    #[must_use]
    pub fn with_marketplace_index_url(mut self, url: impl Into<String>) -> Self {
        self.marketplace_index_url = url.into();
        self
    }

    /// Override marketplace install directory.
    #[must_use]
    pub fn with_marketplace_install_dir(mut self, dir: impl Into<String>) -> Self {
        self.marketplace_install_dir = dir.into();
        self
    }

    /// Enable `/model` runtime model control support.
    #[must_use]
    pub fn with_model_control(mut self, model_control: Arc<dyn TelegramModelControl>) -> Self {
        self.model_control = Some(model_control);
        self
    }

    /// Enable `/reset` runtime reset support.
    #[must_use]
    pub fn with_runtime_control(
        mut self,
        runtime_control: Arc<dyn TelegramRuntimeControl>,
    ) -> Self {
        self.runtime_control = Some(runtime_control);
        self
    }

    /// Enable `/config` runtime config control support.
    #[must_use]
    pub fn with_config_control(mut self, config_control: Arc<dyn TelegramConfigControl>) -> Self {
        self.config_control = Some(config_control);
        self
    }
}

/// Runtime model control hooks used by the `/model` Telegram command.
#[async_trait]
pub trait TelegramModelControl: Send + Sync {
    /// Return the current `(primary_model, deep_model)` pair.
    async fn current_models(&self) -> (String, String);
    /// Update the primary model.
    async fn set_primary_model(&self, model: &str) -> Result<(), String>;
    /// Update the deep model.
    async fn set_deep_model(&self, model: &str) -> Result<(), String>;
    /// Return whether deep model escalation is enabled.
    async fn deep_escalation_enabled(&self) -> bool;
    /// Enable or disable deep model escalation.
    async fn set_deep_escalation_enabled(&self, enabled: bool) -> Result<(), String>;
}

/// Runtime reset hooks used by the `/reset` Telegram command.
#[async_trait]
pub trait TelegramRuntimeControl: Send + Sync {
    /// Reset runtime state: clear active context, reload config, and re-init memory runtime.
    async fn reset_runtime(&self) -> Result<(), String>;
}

/// Runtime configuration hooks used by the `/config` Telegram command.
#[async_trait]
pub trait TelegramConfigControl: Send + Sync {
    /// Return the current runtime configuration snapshot.
    async fn current_config(&self) -> TelegramRuntimeConfig;
    /// Update the primary runtime model.
    async fn set_model(&self, model: &str) -> Result<(), String>;
    /// Update the runtime endpoint URL.
    async fn set_endpoint(&self, endpoint: &str) -> Result<(), String>;
    /// Update the runtime log level.
    async fn set_log_level(&self, log_level: &str) -> Result<(), String>;
}

/// Runtime configuration snapshot shown by `/config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TelegramRuntimeConfig {
    /// Primary model identifier.
    pub model: String,
    /// OpenAI-compatible endpoint URL.
    pub endpoint: String,
    /// Active runtime log level.
    pub log_level: String,
}

/// A Telegram channel adapter that bridges Telegram messages to the agent event loop.
///
/// Receives messages from Telegram via long-polling and emits [`Event::Message`]
/// events. Sends [`Event::ModelResponse`] content back as MarkdownV2-formatted
/// Telegram messages.
pub struct TelegramAdapter {
    config: TelegramConfig,
}

#[derive(Debug)]
enum ModelCommand {
    Show,
    SetPrimary(String),
    SetDeep(String),
}

#[derive(Debug, Clone)]
enum ConfigCommand {
    Show,
    SetModel(String),
    SetEndpoint(String),
    SetLogLevel(String),
}

/// Tracks an in-flight approval request that was emitted by the agent.
///
/// Stored in the shared `pending_approvals` map keyed by `request_id`.
/// Removed when the user responds via `/approve`/`/deny` or the inline keyboard.
#[derive(Debug, Clone)]
struct PendingApproval {
    /// Chat that the approval prompt was sent in, used to route the follow-up reply.
    chat_id: ChatId,
}

impl TelegramAdapter {
    /// Create a new [`TelegramAdapter`] with the given configuration.
    pub fn new(config: TelegramConfig) -> Self {
        Self { config }
    }

    fn parse_model_command(args: Vec<&str>) -> Result<ModelCommand, &'static str> {
        match args.as_slice() {
            [] => Ok(ModelCommand::Show),
            ["deep"] => Err("Usage: /model deep <name>"),
            ["deep", model] if !model.trim().is_empty() => {
                Ok(ModelCommand::SetDeep(model.trim().to_string()))
            }
            [model] if !model.trim().is_empty() => {
                Ok(ModelCommand::SetPrimary(model.trim().to_string()))
            }
            _ => Err("Usage: /model | /model <name> | /model deep <name>"),
        }
    }

    fn parse_verbose_command(args: &[&str], current: bool) -> Result<bool, &'static str> {
        match args {
            [] => Ok(!current),
            [flag] => match flag.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => Ok(true),
                "off" | "false" | "0" => Ok(false),
                _ => Err("Usage: /verbose [on|off]"),
            },
            _ => Err("Usage: /verbose [on|off]"),
        }
    }

    fn parse_reasoning_command(args: &[&str], current: bool) -> Result<bool, &'static str> {
        match args {
            [] => Ok(!current),
            [flag] => match flag.trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "1" => Ok(true),
                "off" | "false" | "0" => Ok(false),
                _ => Err("Usage: /reasoning [on|off]"),
            },
            _ => Err("Usage: /reasoning [on|off]"),
        }
    }

    fn parse_config_command(args: Vec<&str>) -> Result<ConfigCommand, &'static str> {
        match args.as_slice() {
            [] => Ok(ConfigCommand::Show),
            ["model", model] if !model.trim().is_empty() => {
                Ok(ConfigCommand::SetModel(model.trim().to_string()))
            }
            ["endpoint", endpoint] if !endpoint.trim().is_empty() => {
                Ok(ConfigCommand::SetEndpoint(endpoint.trim().to_string()))
            }
            ["log-level", level] | ["loglevel", level] | ["log_level", level]
                if !level.trim().is_empty() =>
            {
                Ok(ConfigCommand::SetLogLevel(level.trim().to_string()))
            }
            _ => Err(
                "Usage: /config | /config model <name> | /config endpoint <url> | /config log-level <level>",
            ),
        }
    }

    /// Parse the `<request_id>` argument for `/approve` and `/deny`.
    ///
    /// Returns `Ok(request_id)` when exactly one non-empty argument is provided,
    /// or `Err(usage_string)` otherwise.
    fn parse_approve_or_deny_command(args: &[&str]) -> Result<String, &'static str> {
        match args {
            [id] if !id.trim().is_empty() => Ok(id.trim().to_string()),
            _ => Err("Usage: /approve <request_id> | /deny <request_id>"),
        }
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
            .map(|u| u.username.as_deref().unwrap_or(&u.first_name).to_string())
            .unwrap_or_else(|| format!("chat:{}", msg.chat.id));

        match &msg.kind {
            MessageKind::Common(common) => {
                use teloxide::types::MediaKind;
                match &common.media_kind {
                    MediaKind::Text(t) => Some(Event::Message {
                        id: Uuid::new_v4().to_string().to_string(),
                        content: t.text.clone(),
                        channel: "telegram".to_string(),
                        sender: from,
                    }),
                    MediaKind::Photo(p) => {
                        // Use the highest-resolution photo
                        let file_id = p
                            .photo
                            .last()
                            .map(|ps| ps.file.id.clone())
                            .unwrap_or_default();
                        let caption = p.caption.as_deref().unwrap_or("").trim().to_string();
                        Some(Event::Message {
                            id: Uuid::new_v4().to_string().to_string(),
                            content: if caption.is_empty() {
                                format!("[photo file_id={file_id}]")
                            } else {
                                format!("[photo file_id={file_id}] {caption}")
                            },
                            channel: "telegram".to_string(),
                            sender: from,
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
                        let caption = d.caption.as_deref().unwrap_or("").trim().to_string();
                        Some(Event::Message {
                            id: Uuid::new_v4().to_string().to_string(),
                            content: if caption.is_empty() {
                                format!("[document file_id={file_id} name={file_name}]")
                            } else {
                                format!("[document file_id={file_id} name={file_name}] {caption}")
                            },
                            channel: "telegram".to_string(),
                            sender: from,
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
            '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.',
            '!',
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

    fn is_approval_prompt(content: &str) -> bool {
        let normalized = content.to_ascii_lowercase();
        normalized.contains("approval required")
            || normalized.contains("requires explicit human approval")
            || normalized.contains("requires approval")
    }

    fn approval_keyboard(request_id: &str) -> InlineKeyboardMarkup {
        InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback("✅ Yes", format!("approval:yes:{request_id}")),
            InlineKeyboardButton::callback("❌ No", format!("approval:no:{request_id}")),
        ]])
    }

    async fn send_markdown_reply(
        bot: &Bot,
        msg: &Message,
        content: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), teloxide::RequestError> {
        let mut req = bot
            .send_message(msg.chat.id, Self::escape_markdown_v2(content))
            .parse_mode(ParseMode::MarkdownV2)
            .reply_parameters(ReplyParameters::new(msg.id));

        if let Some(markup) = reply_markup {
            req = req.reply_markup(markup);
        }

        req.await.map(|_| ())
    }

    async fn acknowledge_message(bot: &Bot, msg: &Message) {
        if let Err(e) = bot
            .set_message_reaction(msg.chat.id, msg.id)
            .reaction(vec![ReactionType::Emoji {
                emoji: "👍".to_string(),
            }])
            .await
        {
            debug!("failed to add Telegram reaction acknowledgement: {e}");
        }
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
        let index_url = self.config.marketplace_index_url.clone();
        let model_control = self.config.model_control.clone();
        let runtime_control = self.config.runtime_control.clone();
        let config_control = self.config.config_control.clone();
        let verbose_by_chat = Arc::new(TokioMutex::new(HashMap::<i64, bool>::new()));
        let installer = std::sync::Arc::new(TokioMutex::new(
            Installer::new(&self.config.marketplace_install_dir)
                .map_err(|e| ChannelError::Telegram(e.to_string()))?,
        ));

        let on_event = std::sync::Arc::new(on_event);
        let on_event_cb = on_event.clone();
        let pending_approvals =
            Arc::new(TokioMutex::new(HashMap::<String, PendingApproval>::new()));
        let pending_approvals_cb = pending_approvals.clone();
        let msg_handler = Update::filter_message().endpoint(move |bot: Bot, msg: Message| {
            let event = Self::message_to_event(&msg);
            let on_event = on_event.clone();
            let installer = installer.clone();
            let index_url = index_url.clone();
            let model_control = model_control.clone();
            let runtime_control = runtime_control.clone();
            let config_control = config_control.clone();
            let verbose_by_chat = verbose_by_chat.clone();
            let pending_approvals = pending_approvals.clone();
            let update_flake_dir =
                std::env::var("PARES_NIX_FLAKE_DIR").unwrap_or_else(|_| DEFAULT_NIX_FLAKE_DIR.into());
            let update_host =
                std::env::var("PARES_NIX_HOST").unwrap_or_else(|_| DEFAULT_NIX_HOST.into());
            let update_command = build_nixos_update_command(&update_flake_dir, &update_host);
            async move {
                // Check for slash commands before sending to agent
                if let Some(text) = msg.text() {
                    if text.starts_with('/') {
                        let mut cmd_parts = text.split_whitespace();
                        let raw_cmd = cmd_parts.next().unwrap_or("").to_lowercase();
                        let cmd = raw_cmd.trim_start_matches('/');
                        let cmd = cmd.split('@').next().unwrap_or(cmd);
                        match cmd {
                            "start" | "help" => {
                                let help = telegram_help_text();
                                let _ = Self::send_markdown_reply(
                                    &bot,
                                    &msg,
                                    &help,
                                    None,
                                )
                                .await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "status" | "health" => {
                                let memory = current_process_rss_kib()
                                    .map(|rss| format!("{rss} KiB"))
                                    .unwrap_or_else(|| "n/a".to_string());
                                let model_line = if let Some(control) = &model_control {
                                    let (primary, deep) = control.current_models().await;
                                    format!("{primary} + {deep}")
                                } else {
                                    "GPT-4.1 + Opus 4.6".to_string()
                                };
                                let status = format!(
                                    "Pares Agens status snapshot\nPID: {}\nMemory RSS: {}\nModel: {}\nPluresDB: ~/.pares-agens/memory/",
                                    std::process::id(), memory, model_line,
                                );
                                let _ = Self::send_markdown_reply(&bot, &msg, &status, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "verbose" => {
                                let args: Vec<&str> = cmd_parts.collect();
                                let chat_key = msg.chat.id.0;
                                let current = {
                                    let lock = verbose_by_chat.lock().await;
                                    *lock.get(&chat_key).unwrap_or(&false)
                                };
                                let reply = match Self::parse_verbose_command(&args, current) {
                                    Ok(new_state) => {
                                        let mut lock = verbose_by_chat.lock().await;
                                        lock.insert(chat_key, new_state);
                                        if new_state {
                                            "Verbose tool details enabled.".to_string()
                                        } else {
                                            "Verbose tool details disabled.".to_string()
                                        }
                                    }
                                    Err(usage) => usage.to_string(),
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "reasoning" => {
                                let Some(control) = &model_control else {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Runtime reasoning controls are unavailable for this deployment.",
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };
                                let args: Vec<&str> = cmd_parts.collect();
                                let current = control.deep_escalation_enabled().await;
                                let reply = match Self::parse_reasoning_command(&args, current) {
                                    Ok(enabled) => match control
                                        .set_deep_escalation_enabled(enabled)
                                        .await
                                    {
                                        Ok(()) => {
                                            if enabled {
                                                "Deep model escalation enabled.".to_string()
                                            } else {
                                                "Deep model escalation disabled.".to_string()
                                            }
                                        }
                                        Err(e) => {
                                            format!("Failed to update deep model escalation: {e}")
                                        }
                                    },
                                    Err(usage) => usage.to_string(),
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "model" => {
                                let Some(control) = &model_control else {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Runtime model switching is unavailable for this deployment.",
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };

                                let reply = match Self::parse_model_command(cmd_parts.collect()) {
                                    Ok(ModelCommand::Show) => {
                                        let (primary, deep) = control.current_models().await;
                                        format!("Current models\nPrimary: {primary}\nDeep: {deep}")
                                    }
                                    Ok(ModelCommand::SetPrimary(model)) => {
                                        match control.set_primary_model(&model).await {
                                            Ok(()) => {
                                                let (_, deep) = control.current_models().await;
                                                format!("Updated primary model to {model}\nDeep: {deep}")
                                            }
                                            Err(e) => format!("Failed to update primary model: {e}"),
                                        }
                                    }
                                    Ok(ModelCommand::SetDeep(model)) => {
                                        match control.set_deep_model(&model).await {
                                            Ok(()) => {
                                                let (primary, _) = control.current_models().await;
                                                format!("Updated deep model to {model}\nPrimary: {primary}")
                                            }
                                            Err(e) => format!("Failed to update deep model: {e}"),
                                        }
                                    }
                                    Err(e) => e.to_string(),
                                };

                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "reset" => {
                                let reply = if let Some(control) = &runtime_control {
                                    match control.reset_runtime().await {
                                        Ok(()) => {
                                            "Reset complete. Runtime state and configuration reloaded.".to_string()
                                        }
                                        Err(e) => {
                                            warn!(error = %e, "telegram /reset failed");
                                            format!("Reset failed: {e}")
                                        }
                                    }
                                } else {
                                    "Runtime reset is unavailable for this deployment.".to_string()
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "config" => {
                                let Some(control) = &config_control else {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Runtime config editing is unavailable for this deployment.",
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };

                                let reply = match Self::parse_config_command(cmd_parts.collect()) {
                                    Ok(ConfigCommand::Show) => {
                                        let config = control.current_config().await;
                                        format!(
                                            "Runtime config\nModel: {}\nEndpoint: {}\nLog level: {}",
                                            config.model, config.endpoint, config.log_level
                                        )
                                    }
                                    Ok(ConfigCommand::SetModel(model)) => {
                                        match control.set_model(&model).await {
                                            Ok(()) => format!("Updated runtime model to {model}"),
                                            Err(e) => format!("Failed to update model: {e}"),
                                        }
                                    }
                                    Ok(ConfigCommand::SetEndpoint(endpoint)) => {
                                        match control.set_endpoint(&endpoint).await {
                                            Ok(()) => {
                                                format!("Updated runtime endpoint to {endpoint}")
                                            }
                                            Err(e) => format!("Failed to update endpoint: {e}"),
                                        }
                                    }
                                    Ok(ConfigCommand::SetLogLevel(log_level)) => {
                                        match control.set_log_level(&log_level).await {
                                            Ok(()) => {
                                                format!("Updated runtime log level to {log_level}")
                                            }
                                            Err(e) => format!("Failed to update log level: {e}"),
                                        }
                                    }
                                    Err(e) => e.to_string(),
                                };

                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "agents" | "browse" => {
                                let message = match fetch_marketplace_index(&index_url).await {
                                    Ok(skills) => format_index_listing(&skills),
                                    Err(e) => format!("Marketplace lookup failed: {e}"),
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &message, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "install" => {
                                let Some(id) = cmd_parts.next() else {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Usage: /install <id>",
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };

                                let reply = match fetch_marketplace_index(&index_url).await {
                                    Ok(skills) => {
                                        if let Some(metadata) = find_skill_by_id(&skills, id) {
                                            let mut lock = installer.lock().await;
                                            if lock.is_installed(&metadata.id) {
                                                format!("'{}' is already installed.", metadata.id)
                                            } else {
                                                match lock.install(metadata) {
                                                    Ok(installed) => format!(
                                                        "✓ Installed '{}' {}.",
                                                        installed.metadata.id,
                                                        installed.metadata.version
                                                    ),
                                                    Err(e) => format!("Install failed: {e}"),
                                                }
                                            }
                                        } else {
                                            format!(
                                                "Agent/plugin '{id}' was not found in pares-modulus index."
                                            )
                                        }
                                    }
                                    Err(e) => format!("Marketplace lookup failed: {e}"),
                                };

                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "logs" => {
                                if !is_update_authorized(&msg) {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Logs denied. Configure PARES_TELEGRAM_UPDATE_ALLOWED_USERS with approved Telegram usernames or numeric IDs.",
                                        None,
                                    )
                                    .await;
                                    return respond(());
                                }
                                let tail_lines = match parse_logs_tail_lines(cmd_parts.collect()) {
                                    Ok(lines) => lines,
                                    Err(usage) => {
                                        let _ =
                                            Self::send_markdown_reply(&bot, &msg, usage, None).await;
                                        Self::acknowledge_message(&bot, &msg).await;
                                        return respond(());
                                    }
                                };

                                info!(
                                    tail_lines,
                                    "telegram /logs requested for pares-agens service"
                                );
                                let reply = match tokio::process::Command::new("journalctl")
                                    .arg("-u")
                                    .arg("pares-agens")
                                    .arg("-n")
                                    .arg(tail_lines.to_string())
                                    .arg("--no-pager")
                                    .output()
                                    .await
                                {
                                    Ok(output) => {
                                        info!(
                                            tail_lines,
                                            status = %output.status,
                                            stdout_bytes = output.stdout.len(),
                                            stderr_bytes = output.stderr.len(),
                                            "telegram /logs command completed"
                                        );
                                        truncate_telegram_message(format!(
                                            "Recent pares-agens logs (last {tail_lines} lines):\n{}",
                                            format_service_logs_output(&output)
                                        ))
                                    }
                                    Err(e) => format!("Failed to start log tail command: {e}"),
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "update" => {
                                if !is_update_authorized(&msg) {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Update denied. Configure PARES_TELEGRAM_UPDATE_ALLOWED_USERS with approved Telegram usernames or numeric IDs.",
                                        None,
                                    )
                                    .await;
                                    return respond(());
                                }
                                let _ = Self::send_markdown_reply(
                                    &bot,
                                    &msg,
                                    &format!(
                                        "Running self-update in `{}` for host `{}`.",
                                        update_flake_dir, update_host
                                    ),
                                    None,
                                )
                                .await;
                                let reply = match tokio::process::Command::new("sh")
                                    .arg("-c")
                                    .arg(&update_command)
                                    .output()
                                    .await
                                {
                                    Ok(output) => {
                                        truncate_telegram_message(format_update_command_output(&output))
                                    }
                                    Err(e) => format!("Failed to start self-update command: {e}"),
                                };
                                let _ = Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "approve" | "deny" => {
                                if !is_update_authorized(&msg) {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        "Not authorized. Configure PARES_TELEGRAM_UPDATE_ALLOWED_USERS with approved Telegram usernames or numeric IDs.",
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                }
                                let args: Vec<&str> = cmd_parts.collect();
                                let request_id =
                                    match Self::parse_approve_or_deny_command(&args) {
                                        Ok(id) => id,
                                        Err(usage) => {
                                            let _ = Self::send_markdown_reply(
                                                &bot, &msg, usage, None,
                                            )
                                            .await;
                                            Self::acknowledge_message(&bot, &msg).await;
                                            return respond(());
                                        }
                                    };

                                let removed = {
                                    let mut map = pending_approvals.lock().await;
                                    map.remove(&request_id)
                                };

                                if removed.is_none() {
                                    let _ = Self::send_markdown_reply(
                                        &bot,
                                        &msg,
                                        &format!(
                                            "No pending approval found with ID: {request_id}"
                                        ),
                                        None,
                                    )
                                    .await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                }

                                let approved = cmd == "approve";
                                let sender = msg
                                    .from
                                    .as_ref()
                                    .map(|u| {
                                        u.username
                                            .as_deref()
                                            .unwrap_or(&u.first_name)
                                            .to_string()
                                    })
                                    .unwrap_or_else(|| format!("chat:{}", msg.chat.id));

                                info!(
                                    request_id = %request_id,
                                    approved,
                                    sender = %sender,
                                    "telegram /approve|/deny: elevated action decision received"
                                );

                                let content = if approved {
                                    format!("Approved. [request_id: {request_id}]")
                                } else {
                                    format!("Denied. [request_id: {request_id}]")
                                };

                                let approval_event = Event::Message {
                                    id: Uuid::new_v4().to_string(),
                                    content,
                                    channel: "telegram".to_string(),
                                    sender,
                                };

                                let reply =
                                    if let Some(Event::ModelResponse { content, .. }) =
                                        on_event(approval_event).await
                                    {
                                        content
                                    } else if approved {
                                        format!("Approved request {request_id}.")
                                    } else {
                                        format!("Denied request {request_id}.")
                                    };

                                let _ =
                                    Self::send_markdown_reply(&bot, &msg, &reply, None).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            _ => {} // fall through to agent
                        }
                    }
                }

                // Normal message — send to agent
                if let Some(mut event) = event {
                    let verbose_enabled = {
                        let lock = verbose_by_chat.lock().await;
                        *lock.get(&msg.chat.id.0).unwrap_or(&false)
                    };
                    if verbose_enabled {
                        if let Event::Message { content, .. } = &mut event {
                            *content = format!("{TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER}{content}");
                        }
                    }
                    if let Some(Event::ModelResponse {
                        request_id, content, ..
                    }) = on_event(event).await
                    {
                        let reply_markup = if Self::is_approval_prompt(&content) {
                            {
                                let mut map = pending_approvals.lock().await;
                                map.insert(
                                    request_id.clone(),
                                    PendingApproval {
                                        chat_id: msg.chat.id,
                                    },
                                );
                            }
                            info!(
                                request_id = %request_id,
                                "telegram: queued pending approval"
                            );
                            Some(Self::approval_keyboard(&request_id))
                        } else {
                            None
                        };

                        if let Err(e) = Self::send_markdown_reply(&bot, &msg, &content, reply_markup).await
                        {
                            error!("Failed to send Telegram reply: {e}");
                        } else {
                            Self::acknowledge_message(&bot, &msg).await;
                        }
                    }
                }
                respond(())
            }
        });

        let callback_handler =
            Update::filter_callback_query().endpoint(move |bot: Bot, q: CallbackQuery| {
                let on_event = on_event_cb.clone();
                let pending_approvals = pending_approvals_cb.clone();
                async move {
                    let data = match q.data.as_deref() {
                        Some(d) if d.starts_with("approval:") => d.to_string(),
                        _ => {
                            let _ = bot.answer_callback_query(q.id).await;
                            return respond(());
                        }
                    };

                    let parts: Vec<&str> = data.splitn(3, ':').collect();
                    if parts.len() != 3 {
                        let _ = bot.answer_callback_query(q.id).await;
                        return respond(());
                    }
                    let decision = parts[1]; // "yes" or "no"
                    let request_id = parts[2].to_string();

                    // Authorization check — same allowlist as `/approve`/`/deny`
                    if !is_user_authorized(&q.from) {
                        let _ = bot
                            .answer_callback_query(q.id)
                            .text("Not authorized to approve or deny actions.")
                            .await;
                        return respond(());
                    }

                    let removed = {
                        let mut map = pending_approvals.lock().await;
                        map.remove(&request_id)
                    };

                    let pending = match removed {
                        Some(p) => p,
                        None => {
                            let _ = bot
                                .answer_callback_query(q.id)
                                .text("This approval request has already been resolved or expired.")
                                .await;
                            return respond(());
                        }
                    };

                    let approved = decision == "yes";
                    let sender = q
                        .from
                        .username
                        .as_deref()
                        .unwrap_or(&q.from.first_name)
                        .to_string();

                    info!(
                        request_id = %request_id,
                        approved,
                        sender = %sender,
                        "telegram approval callback: decision received"
                    );

                    let status_text = if approved { "Approved" } else { "Denied" };
                    let _ = bot
                        .answer_callback_query(q.id)
                        .text(status_text)
                        .await;

                    let content = if approved {
                        format!("Approved. [request_id: {request_id}]")
                    } else {
                        format!("Denied. [request_id: {request_id}]")
                    };

                    let approval_event = Event::Message {
                        id: Uuid::new_v4().to_string(),
                        content,
                        channel: "telegram".to_string(),
                        sender,
                    };

                    let reply = if let Some(Event::ModelResponse { content, .. }) =
                        on_event(approval_event).await
                    {
                        content
                    } else if approved {
                        format!("Approved request {request_id}.")
                    } else {
                        format!("Denied request {request_id}.")
                    };

                    let _ = bot
                        .send_message(
                            pending.chat_id,
                            TelegramAdapter::escape_markdown_v2(&reply),
                        )
                        .parse_mode(ParseMode::MarkdownV2)
                        .await;

                    respond(())
                }
            });

        Dispatcher::builder(
            bot,
            teloxide::dptree::entry()
                .branch(msg_handler)
                .branch(callback_handler),
        )
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
    use teloxide::types::InlineKeyboardButtonKind;

    // ── escape_markdown_v2 ────────────────────────────────────────────────

    #[test]
    fn escape_plain_text_unchanged() {
        assert_eq!(
            TelegramAdapter::escape_markdown_v2("hello world"),
            "hello world"
        );
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

    #[test]
    fn approval_prompt_detection_matches_expected_phrases() {
        assert!(TelegramAdapter::is_approval_prompt(
            "This action requires explicit human approval before dispatch."
        ));
        assert!(TelegramAdapter::is_approval_prompt(
            "approval required: potentially destructive operation"
        ));
        assert!(!TelegramAdapter::is_approval_prompt("All checks passed."));
    }

    #[test]
    fn approval_keyboard_contains_yes_no_buttons() {
        let kb = TelegramAdapter::approval_keyboard("req-42");
        let rows = kb.inline_keyboard;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].len(), 2);
        assert_eq!(rows[0][0].text, "✅ Yes");
        assert_eq!(rows[0][1].text, "❌ No");
        assert_eq!(
            rows[0][0].kind,
            InlineKeyboardButtonKind::CallbackData("approval:yes:req-42".to_string())
        );
        assert_eq!(
            rows[0][1].kind,
            InlineKeyboardButtonKind::CallbackData("approval:no:req-42".to_string())
        );
    }

    // ── parse_approve_or_deny_command ─────────────────────────────────────

    #[test]
    fn parse_approve_or_deny_command_returns_request_id() {
        assert_eq!(
            TelegramAdapter::parse_approve_or_deny_command(&["req-42"]),
            Ok("req-42".to_string())
        );
    }

    #[test]
    fn parse_approve_or_deny_command_trims_whitespace() {
        assert_eq!(
            TelegramAdapter::parse_approve_or_deny_command(&["  req-7  "]),
            Ok("req-7".to_string())
        );
    }

    #[test]
    fn parse_approve_or_deny_command_rejects_empty_args() {
        assert_eq!(
            TelegramAdapter::parse_approve_or_deny_command(&[]),
            Err("Usage: /approve <request_id> | /deny <request_id>")
        );
    }

    #[test]
    fn parse_approve_or_deny_command_rejects_multiple_args() {
        assert_eq!(
            TelegramAdapter::parse_approve_or_deny_command(&["req-1", "req-2"]),
            Err("Usage: /approve <request_id> | /deny <request_id>")
        );
    }

    #[test]
    fn parse_approve_or_deny_command_rejects_blank_id() {
        assert_eq!(
            TelegramAdapter::parse_approve_or_deny_command(&[""]),
            Err("Usage: /approve <request_id> | /deny <request_id>")
        );
    }

    #[test]
    fn parse_model_command_show() {
        assert!(matches!(
            TelegramAdapter::parse_model_command(vec![]),
            Ok(ModelCommand::Show)
        ));
    }

    #[test]
    fn help_text_lists_all_registered_slash_commands() {
        let help = telegram_help_text();
        for (command, description) in TELEGRAM_HELP_COMMANDS {
            assert!(
                help.contains(&format!("{command} - {description}")),
                "expected help output to include {command} with description"
            );
        }
    }

    #[test]
    fn parse_model_command_set_primary() {
        assert!(matches!(
            TelegramAdapter::parse_model_command(vec!["gpt-4o"]),
            Ok(ModelCommand::SetPrimary(model)) if model == "gpt-4o"
        ));
    }

    #[test]
    fn parse_model_command_set_deep() {
        assert!(matches!(
            TelegramAdapter::parse_model_command(vec!["deep", "claude-opus-4.6"]),
            Ok(ModelCommand::SetDeep(model)) if model == "claude-opus-4.6"
        ));
    }

    #[test]
    fn parse_model_command_invalid_usage() {
        assert_eq!(
            TelegramAdapter::parse_model_command(vec!["deep"]).unwrap_err(),
            "Usage: /model deep <name>"
        );
    }

    #[test]
    fn parse_config_command_show() {
        assert!(matches!(
            TelegramAdapter::parse_config_command(vec![]),
            Ok(ConfigCommand::Show)
        ));
    }

    #[test]
    fn parse_config_command_set_model() {
        assert!(matches!(
            TelegramAdapter::parse_config_command(vec!["model", "gpt-4.1"]),
            Ok(ConfigCommand::SetModel(model)) if model == "gpt-4.1"
        ));
    }

    #[test]
    fn parse_config_command_set_endpoint() {
        assert!(matches!(
            TelegramAdapter::parse_config_command(vec!["endpoint", "http://localhost:11434/v1"]),
            Ok(ConfigCommand::SetEndpoint(endpoint)) if endpoint == "http://localhost:11434/v1"
        ));
    }

    #[test]
    fn parse_config_command_set_log_level() {
        assert!(matches!(
            TelegramAdapter::parse_config_command(vec!["log-level", "debug"]),
            Ok(ConfigCommand::SetLogLevel(level)) if level == "debug"
        ));
    }

    #[test]
    fn parse_config_command_invalid_usage() {
        assert_eq!(
            TelegramAdapter::parse_config_command(vec!["endpoint"]).unwrap_err(),
            "Usage: /config | /config model <name> | /config endpoint <url> | /config log-level <level>"
        );
    }

    #[test]
    fn parse_verbose_command_toggles_when_no_args() {
        assert!(TelegramAdapter::parse_verbose_command(&[], false).unwrap());
        assert!(!TelegramAdapter::parse_verbose_command(&[], true).unwrap());
    }

    #[test]
    fn parse_verbose_command_supports_explicit_values() {
        assert!(TelegramAdapter::parse_verbose_command(&["on"], false).unwrap());
        assert!(!TelegramAdapter::parse_verbose_command(&["off"], true).unwrap());
    }

    #[test]
    fn parse_verbose_command_rejects_invalid_args() {
        assert_eq!(
            TelegramAdapter::parse_verbose_command(&["maybe"], false).unwrap_err(),
            "Usage: /verbose [on|off]"
        );
    }

    #[test]
    fn parse_reasoning_command_toggles_when_no_args() {
        assert!(TelegramAdapter::parse_reasoning_command(&[], false).unwrap());
        assert!(!TelegramAdapter::parse_reasoning_command(&[], true).unwrap());
    }

    #[test]
    fn parse_reasoning_command_supports_explicit_values() {
        assert!(TelegramAdapter::parse_reasoning_command(&["on"], false).unwrap());
        assert!(!TelegramAdapter::parse_reasoning_command(&["off"], true).unwrap());
    }

    #[test]
    fn parse_reasoning_command_rejects_invalid_args() {
        assert_eq!(
            TelegramAdapter::parse_reasoning_command(&["maybe"], false).unwrap_err(),
            "Usage: /reasoning [on|off]"
        );
    }

    #[test]
    fn parse_logs_tail_lines_defaults_and_clamps() {
        assert_eq!(
            parse_logs_tail_lines(vec![]).unwrap(),
            DEFAULT_LOG_TAIL_LINES
        );
        assert_eq!(
            parse_logs_tail_lines(vec!["9999"]).unwrap(),
            MAX_LOG_TAIL_LINES
        );
    }

    #[test]
    fn parse_logs_tail_lines_rejects_invalid_values() {
        assert_eq!(
            parse_logs_tail_lines(vec!["0"]).unwrap_err(),
            "Usage: /logs [n] (n must be a positive integer)"
        );
        assert_eq!(
            parse_logs_tail_lines(vec!["not-a-number"]).unwrap_err(),
            "Usage: /logs [n] (n must be a positive integer)"
        );
        assert_eq!(
            parse_logs_tail_lines(vec!["10", "20"]).unwrap_err(),
            "Usage: /logs [n]"
        );
    }

    // ── TelegramAdapter basics ────────────────────────────────────────────

    #[test]
    fn adapter_name_is_telegram() {
        let adapter = TelegramAdapter::new(TelegramConfig::new("test-token"));
        assert_eq!(adapter.name(), "telegram");
    }

    #[test]
    fn parse_modulus_index_accepts_array_root() {
        let json = r#"
        [
          {"id":"pares/rust-helper","name":"Rust Helper","version":"1.2.3","description":"Rust coding helper","author":"pares","checksum":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","download_url":"https://example.com/rust-helper.tar.gz"}
        ]
        "#;
        let skills = parse_modulus_index(json).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "pares/rust-helper");
        assert_eq!(skills[0].version, "1.2.3");
    }

    #[test]
    fn parse_modulus_index_accepts_object_agents_root() {
        let json = r#"
        {
          "agents": [
            {"id":"pares/ops","description":"Ops assistant","checksum":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","download_url":"https://example.com/ops.tar.gz"}
          ]
        }
        "#;
        let skills = parse_modulus_index(json).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "pares/ops");
    }

    #[test]
    fn find_skill_by_id_is_case_insensitive() {
        let skills = vec![SkillMetadata {
            id: "pares/rust-helper".to_string(),
            name: "Rust Helper".to_string(),
            version: "1.0.0".to_string(),
            description: "desc".to_string(),
            author: "pares".to_string(),
            categories: vec![SkillCategory::Coding("rust".to_string())],
            checksum: "0".repeat(64),
            download_url: "https://example.com".to_string(),
            signature: None,
        }];

        let found = find_skill_by_id(&skills, "PARES/RUST-HELPER").unwrap();
        assert_eq!(found.id, "pares/rust-helper");
    }

    #[test]
    fn parse_modulus_index_skips_entries_without_checksum() {
        let json = r#"
        [
          {"id":"pares/invalid","description":"missing checksum","download_url":"https://example.com/invalid.tar.gz"}
        ]
        "#;
        let skills = parse_modulus_index(json).unwrap();
        assert!(skills.is_empty());
    }

    #[test]
    fn build_nixos_update_command_contains_required_steps() {
        let command = build_nixos_update_command("/etc/nixos", "praxisbot");
        assert!(command.contains("sudo nix flake update pares-agens"));
        assert!(command.contains("sudo nixos-rebuild switch --flake .#'praxisbot'"));
        assert!(command.contains("No new pares-agens commits on main"));
    }

    #[test]
    fn shell_single_quote_escapes_single_quotes() {
        assert_eq!(shell_single_quote("/etc/ni'xos"), "'/etc/ni'\"'\"'xos'");
    }

    #[test]
    fn truncate_telegram_message_marks_truncation() {
        let input = "a".repeat(TELEGRAM_MAX_MESSAGE_CHARS + 10);
        let truncated = truncate_telegram_message(input);
        assert!(truncated.ends_with("…(truncated)"));
    }

    #[test]
    fn format_update_command_output_success_without_stdout() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("true")
            .output()
            .unwrap();
        assert_eq!(
            format_update_command_output(&output),
            "Self-update completed.".to_string()
        );
    }

    #[test]
    fn format_update_command_output_success_with_stdout() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("printf 'updated'")
            .output()
            .unwrap();
        assert_eq!(format_update_command_output(&output), "updated".to_string());
    }

    #[test]
    fn format_update_command_output_failure_includes_stderr() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo boom >&2; exit 7")
            .output()
            .unwrap();
        let formatted = format_update_command_output(&output);
        assert!(formatted.contains("Self-update failed"));
        assert!(formatted.contains("boom"));
    }

    #[test]
    fn format_service_logs_output_handles_success_and_failure() {
        let success = std::process::Command::new("sh")
            .arg("-c")
            .arg("printf 'line1\\nline2'")
            .output()
            .unwrap();
        assert_eq!(
            format_service_logs_output(&success),
            "line1\nline2".to_string()
        );

        let failure = std::process::Command::new("sh")
            .arg("-c")
            .arg("echo denied >&2; exit 1")
            .output()
            .unwrap();
        let formatted_failure = format_service_logs_output(&failure);
        assert!(formatted_failure.contains("Failed to read service logs"));
        assert!(formatted_failure.contains("denied"));
    }
}
