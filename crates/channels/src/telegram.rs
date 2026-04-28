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
        ChatAction, InlineKeyboardButton, InlineKeyboardMarkup, Message, MessageKind, ParseMode,
        ReactionType, ReplyParameters,
    },
};
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::adapter::{ChannelAdapter, ChannelError};
use crate::group_context::{GroupContextBuffer, GroupMessage};
use pares_agens_core::channel_contract::{ChannelContract, GroupChatPolicy};
use pares_agens_core::event_spine::EventSpineHandle;
use pares_agens_core::renderers::telegram as html_renderer;
use pares_agens_agenda::scheduler::Scheduler;

const PARES_MODULUS_INDEX_URL: &str =
    "https://raw.githubusercontent.com/plures/pares-modulus/main/index.json";
const DEFAULT_MARKETPLACE_INSTALL_DIR: &str = "/skills";
const MAX_INDEX_LISTING_ITEMS: usize = 10;
const DEFAULT_NIX_FLAKE_DIR: &str = "nixos-config";
const DEFAULT_NIX_HOST: &str = "praxisbot";
/// Directory containing the pares-agens source for rebuilding the binary.
const DEFAULT_PARES_AGENS_DIR: &str = "pares-agens";
/// Subdirectory under $HOME/projects for defaults, or override with env vars.
const PROJECTS_SUBDIR: &str = "projects";
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
    (
        "/version",
        "show version and build info",
    ),
    ("/logs [n]", "tail recent pares-agens service logs"),
    ("/tools", "show tool governance policies"),
    (
        "/update",
        "run NixOS self-update and rebuild if pares-agens changed",
    ),
    (
        "/personality",
        "show or modify personality (set tone <t>, rule add <r>, rule remove <id>)",
    ),
    (
        "/cron",
        "manage scheduled tasks (list, add, remove, pause, resume)",
    ),
    (
        "/plugin",
        "manage plugins (list, install <path>, uninstall <name>, schema <name>)",
    ),
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

fn build_nixos_update_command(_flake_dir: &str, _host: &str) -> String {
    // Resolve agens source dir: env var → $HOME/projects/pares-agens
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/kbristol".into());
    let agens_dir = std::env::var("PARES_AGENS_DIR")
        .unwrap_or_else(|_| format!("{home}/{PROJECTS_SUBDIR}/{DEFAULT_PARES_AGENS_DIR}"));
    let agens_dir = shell_single_quote(&agens_dir);
    let bin_dir = format!("{home}/.local/bin");
    format!(
        "set -eu; \
         echo 'Step 1: Pulling latest pares-agens source...'; \
         cd {agens_dir} && git pull --ff-only; \
         echo 'Step 2: Building pares-agens binary...'; \
         nix develop --option substituters 'https://cache.nixos.org' -c cargo build --release -p pares-agens; \
         echo 'Step 3: Installing binary...'; \
         mkdir -p {bin_dir}; \
         cp target/release/pares-agens {bin_dir}/pares-agens; \
         echo 'Step 4: Restarting service...'; \
         sudo systemctl restart pares-agens; \
         echo 'Self-update complete. New binary installed and service restarted.'"
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

fn is_update_authorized(msg: &Message) -> bool {
    let allowlist = std::env::var("PARES_TELEGRAM_UPDATE_ALLOWED_USERS")
        .ok()
        .unwrap_or_default();
    if allowlist.trim().is_empty() {
        return false;
    }
    let Some(from) = msg.from.as_ref() else {
        return false;
    };

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
    /// Optional personality control for `/personality`.
    pub personality_control: Option<Arc<dyn TelegramPersonalityControl>>,
    /// Optional scheduler for `/cron` commands.
    pub scheduler: Option<Arc<Scheduler>>,
    /// Policy for group chat participation.
    pub group_chat_policy: GroupChatPolicy,
    /// Optional plugin runtime for `/plugin` commands.
    pub plugin_runtime: Option<Arc<pares_agens_core::plugins::PluginRuntime>>,
    /// Optional plugin CRUD executor for entity counts.
    pub plugin_executor: Option<Arc<pares_agens_core::plugins::PluginCrudExecutor>>,
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
            personality_control: None,
            scheduler: None,
            group_chat_policy: GroupChatPolicy::default(),
            plugin_runtime: None,
            plugin_executor: None,
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

    /// Enable `/personality` runtime personality control.
    #[must_use]
    pub fn with_personality_control(mut self, control: Arc<dyn TelegramPersonalityControl>) -> Self {
        self.personality_control = Some(control);
        self
    }

    /// Enable `/cron` scheduling commands.
    #[must_use]
    pub fn with_scheduler(mut self, scheduler: Arc<Scheduler>) -> Self {
        self.scheduler = Some(scheduler);
        self
    }

    /// Override the default group chat participation policy.
    #[must_use]
    pub fn with_group_chat_policy(mut self, policy: GroupChatPolicy) -> Self {
        self.group_chat_policy = policy;
        self
    }

    /// Attach the plugin runtime and executor for `/plugin` commands.
    #[must_use]
    pub fn with_plugin_runtime(
        mut self,
        runtime: Arc<pares_agens_core::plugins::PluginRuntime>,
        executor: Arc<pares_agens_core::plugins::PluginCrudExecutor>,
    ) -> Self {
        self.plugin_runtime = Some(runtime);
        self.plugin_executor = Some(executor);
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

/// Runtime personality control for `/personality`.
#[async_trait::async_trait]
pub trait TelegramPersonalityControl: Send + Sync {
    /// Return the current personality summary.
    async fn show(&self, channel: Option<&str>) -> String;
    /// Set the tone.
    async fn set_tone(&self, tone: &str) -> Result<(), String>;
    /// Add or update a behavioral rule.
    async fn add_rule(&self, rule_text: &str) -> Result<String, String>;
    /// Remove a behavioral rule by ID.
    async fn remove_rule(&self, id: &str) -> Result<(), String>;
    /// List all personality documents with sizes.
    async fn list_documents(&self) -> String;
    /// Get a specific personality document's content.
    async fn get_document(&self, doc_type: &str) -> String;
    /// Set a personality document's content.
    async fn set_document(&self, doc_type: &str, content: &str) -> Result<(), String>;
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
    event_spine: Option<EventSpineHandle>,
}

#[derive(Debug)]
enum ModelCommand {
    Show,
    SetPrimary(String),
    SetDeep(String),
}

#[derive(Debug)]
enum ConfigCommand {
    Show,
    SetModel(String),
    SetEndpoint(String),
    SetLogLevel(String),
}

impl TelegramAdapter {
    /// Create a new [`TelegramAdapter`] with the given configuration.
    pub fn new(config: TelegramConfig) -> Self {
        Self { config, event_spine: None }
    }

    /// Create a new [`TelegramAdapter`] with an event spine handle.
    pub fn with_event_spine(config: TelegramConfig, spine: EventSpineHandle) -> Self {
        Self { config, event_spine: Some(spine) }
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

    /// Telegram enforces a 4096-character limit per message.
    /// Split long replies into chunks on paragraph boundaries and send sequentially.
    #[allow(dead_code)]
    const TELEGRAM_MAX_LEN: usize = 4096;

    #[allow(dead_code)]
    fn chunk_message(text: &str, max_len: usize) -> Vec<String> {
        if text.len() <= max_len {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if remaining.len() <= max_len {
                chunks.push(remaining.to_string());
                break;
            }

            // Find a split point: prefer double-newline (paragraph), then single newline, then space
            let search_range = &remaining[..max_len];
            let split_at = search_range.rfind("\n\n")
                .map(|i| i + 2)
                .or_else(|| search_range.rfind('\n').map(|i| i + 1))
                .or_else(|| search_range.rfind(' ').map(|i| i + 1))
                .unwrap_or(max_len);

            chunks.push(remaining[..split_at].to_string());
            remaining = &remaining[split_at..];
        }

        chunks
    }

    #[deprecated(note = "Use send_html_reply instead — HTML is the preferred Telegram format")]
    #[allow(dead_code)]
    async fn send_markdown_reply(
        bot: &Bot,
        msg: &Message,
        content: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
    ) -> Result<(), teloxide::RequestError> {
        let escaped = Self::escape_markdown_v2(content);
        let chunks = Self::chunk_message(&escaped, Self::TELEGRAM_MAX_LEN);

        for (i, chunk) in chunks.iter().enumerate() {
            let mut req = bot
                .send_message(msg.chat.id, chunk.clone())
                .parse_mode(ParseMode::MarkdownV2);

            // Only reply to the original message on the first chunk
            if i == 0 {
                req = req.reply_parameters(ReplyParameters::new(msg.id));
            }

            // Only attach reply markup to the last chunk
            if i == chunks.len() - 1 {
                if let Some(ref markup) = reply_markup {
                    req = req.reply_markup(markup.clone());
                }
            }

            req.await?;
        }

        Ok(())
    }

    /// Edit a placeholder message with the full response.
    ///
    /// If the response fits in one message, edits the placeholder in-place.
    /// If it requires multiple chunks, edits the placeholder with the first chunk
    /// and sends remaining chunks as new messages.
    /// Returns `Some((format, msg_ids))` on success, `None` if editing failed.
    async fn edit_placeholder_with_response(
        bot: &Bot,
        chat_id: ChatId,
        placeholder_id: teloxide::types::MessageId,
        content: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
        _event_spine: Option<&EventSpineHandle>,
    ) -> Option<(&'static str, Vec<i64>)> {
        let contract = ChannelContract::telegram();
        let (html_chunks, plain_chunks) = html_renderer::render_for_telegram(content, &contract);
        let mut sent_ids = vec![placeholder_id.0 as i64];

        // Try editing placeholder with first HTML chunk
        let first_html = html_chunks.first()?;
        let mut edit_req = bot.edit_message_text(chat_id, placeholder_id, first_html.clone())
            .parse_mode(ParseMode::Html);
        if html_chunks.len() == 1 {
            if let Some(ref markup) = reply_markup {
                edit_req = edit_req.reply_markup(markup.clone());
            }
        }
        match edit_req.await {
            Ok(_) => {
                // Send remaining HTML chunks as new messages
                for (i, chunk) in html_chunks.iter().skip(1).enumerate() {
                    let mut req = bot.send_message(chat_id, chunk.clone())
                        .parse_mode(ParseMode::Html);
                    if i == html_chunks.len() - 2 {
                        if let Some(ref markup) = reply_markup {
                            req = req.reply_markup(markup.clone());
                        }
                    }
                    match req.await {
                        Ok(sent) => sent_ids.push(sent.id.0 as i64),
                        Err(e) => {
                            error!(error = %e, "Failed to send extra HTML chunk after placeholder edit");
                            return Some(("html", sent_ids));
                        }
                    }
                }
                Some(("html", sent_ids))
            }
            Err(e) => {
                debug!(error = %e, "HTML edit of placeholder failed, trying plain text");
                // Try plain text edit
                let first_plain = plain_chunks.first()?;
                let mut plain_edit = bot.edit_message_text(chat_id, placeholder_id, first_plain.clone());
                if plain_chunks.len() == 1 {
                    if let Some(ref markup) = reply_markup {
                        plain_edit = plain_edit.reply_markup(markup.clone());
                    }
                }
                match plain_edit.await {
                    Ok(_) => {
                        for (i, chunk) in plain_chunks.iter().skip(1).enumerate() {
                            let mut req = bot.send_message(chat_id, chunk.clone());
                            if i == plain_chunks.len() - 2 {
                                if let Some(ref markup) = reply_markup {
                                    req = req.reply_markup(markup.clone());
                                }
                            }
                            match req.await {
                                Ok(sent) => sent_ids.push(sent.id.0 as i64),
                                Err(e) => {
                                    error!(error = %e, "Failed to send extra plain chunk");
                                    return Some(("plain", sent_ids));
                                }
                            }
                        }
                        Some(("plain", sent_ids))
                    }
                    Err(_) => None, // give up, caller will delete placeholder and send fresh
                }
            }
        }
    }

    /// Send a reply using HTML formatting with plain-text fallback.
    ///
    /// Converts model markdown output to Telegram HTML via the core renderer,
    /// chunks according to the channel contract, and sends with `ParseMode::Html`.
    /// If HTML send fails, retries with plain text (no formatting).
    ///
    /// Returns `("html" | "plain", Vec<message_id>)` on success for event tracking.
    async fn send_html_reply(
        bot: &Bot,
        msg: &Message,
        content: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
        event_spine: Option<&EventSpineHandle>,
    ) -> Result<(&'static str, Vec<i64>), teloxide::RequestError> {
        let contract = ChannelContract::telegram();
        let (html_chunks, plain_chunks) = html_renderer::render_for_telegram(content, &contract);
        let mut sent_ids = Vec::new();

        // Try HTML first
        let mut html_failed = false;
        for (i, chunk) in html_chunks.iter().enumerate() {
            let mut req = bot
                .send_message(msg.chat.id, chunk.clone())
                .parse_mode(ParseMode::Html);

            if i == 0 {
                req = req.reply_parameters(ReplyParameters::new(msg.id));
            }
            if i == html_chunks.len() - 1 {
                if let Some(ref markup) = reply_markup {
                    req = req.reply_markup(markup.clone());
                }
            }

            match req.await {
                Ok(sent) => {
                    sent_ids.push(sent.id.0 as i64);
                }
                Err(e) => {
                    error!(error = %e, chunk_index = i, "HTML send failed, falling back to plain text");
                    html_failed = true;
                    break;
                }
            }
        }

        if !html_failed {
            return Ok(("html", sent_ids));
        }

        // Fallback: plain text (no formatting at all)
        sent_ids.clear();
        for (i, chunk) in plain_chunks.iter().enumerate() {
            let mut req = bot.send_message(msg.chat.id, chunk.clone());

            if i == 0 {
                req = req.reply_parameters(ReplyParameters::new(msg.id));
            }
            if i == plain_chunks.len() - 1 {
                if let Some(ref markup) = reply_markup {
                    req = req.reply_markup(markup.clone());
                }
            }

            match req.await {
                Ok(sent) => {
                    sent_ids.push(sent.id.0 as i64);
                }
                Err(e) => {
                    error!(error = %e, chunk_index = i, "Plain text fallback also failed");
                    if let Some(spine) = event_spine {
                        spine.emit_delivery_failure(
                            msg.chat.id.0,
                            "telegram",
                            &e.to_string(),
                            false,
                        );
                    }
                    return Err(e);
                }
            }
        }

        Ok(("plain", sent_ids))
    }

    /// Send a reply with error handling and event spine integration.
    ///
    /// Logs errors, emits delivery failure events, and falls back to plain text.
    /// This is the standard send path for all command responses.
    async fn send_reply_with_fallback(
        bot: &Bot,
        msg: &Message,
        content: &str,
        reply_markup: Option<InlineKeyboardMarkup>,
        event_spine: Option<&EventSpineHandle>,
    ) {
        match Self::send_html_reply(bot, msg, content, reply_markup, event_spine).await {
            Ok((format_used, msg_ids)) => {
                if let Some(spine) = event_spine {
                    for mid in &msg_ids {
                        spine.emit_delivery_success(msg.chat.id.0, "telegram", *mid, format_used);
                    }
                }
            }
            Err(e) => {
                error!(error = %e, chat_id = msg.chat.id.0, "Failed to send Telegram reply (all formats)");
                // Delivery failure already emitted inside send_html_reply
            }
        }
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

    /// Check if a chat is a group or supergroup.
    fn is_group_chat(msg: &Message) -> bool {
        use teloxide::types::ChatKind;
        matches!(&msg.chat.kind, ChatKind::Public(public) if matches!(
            public.kind,
            teloxide::types::PublicChatKind::Group | teloxide::types::PublicChatKind::Supergroup(_)
        ))
    }

    /// Determine if the bot should respond in a group chat based on trigger conditions.
    fn should_respond_in_group(
        msg: &Message,
        bot_username: &str,
        policy: &GroupChatPolicy,
    ) -> bool {
        let text = msg.text().unwrap_or_default();

        // Check @mention
        if policy.respond_on_mention {
            let mention = format!("@{}", bot_username);
            if text.contains(&mention) {
                return true;
            }
        }

        // Check reply-to-bot
        if policy.respond_on_reply {
            if let Some(reply) = msg.reply_to_message() {
                if let Some(from) = reply.from.as_ref() {
                    if from.username.as_deref() == Some(bot_username) {
                        return true;
                    }
                }
            }
        }

        // Check prefix trigger
        if let Some(ref prefix) = policy.respond_on_prefix {
            if text.starts_with(prefix.as_str()) {
                return true;
            }
        }

        false
    }

    /// React to a message with a contextually appropriate emoji.
    #[allow(dead_code)]
    async fn react_contextually(bot: &Bot, msg: &Message) {
        let text = msg.text().unwrap_or_default().to_lowercase();
        let emoji = if text.contains('?') {
            "🤔"
        } else {
            "👀"
        };
        if let Err(e) = bot
            .set_message_reaction(msg.chat.id, msg.id)
            .reaction(vec![ReactionType::Emoji {
                emoji: emoji.to_string(),
            }])
            .await
        {
            debug!("failed to add contextual group reaction: {e}");
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

        // Fetch bot username for group mention detection
        let bot_me = bot.get_me().await.map_err(|e| ChannelError::Telegram(format!("failed to get bot info: {e}")))?;
        let bot_username: Arc<str> = Arc::from(
            bot_me.username.as_deref().unwrap_or("bot").to_string().into_boxed_str()
        );

        let group_policy = Arc::new(self.config.group_chat_policy.clone());
        let group_context = Arc::new(TokioMutex::new(
            GroupContextBuffer::new(self.config.group_chat_policy.context_window),
        ));

        let index_url = self.config.marketplace_index_url.clone();
        let model_control = self.config.model_control.clone();
        let runtime_control = self.config.runtime_control.clone();
        let config_control = self.config.config_control.clone();
        let personality_control = self.config.personality_control.clone();
        let scheduler = self.config.scheduler.clone();
        let plugin_runtime = self.config.plugin_runtime.clone();
        let plugin_executor = self.config.plugin_executor.clone();
        let event_spine = self.event_spine.clone();
        let verbose_by_chat = Arc::new(TokioMutex::new(HashMap::<i64, bool>::new()));
        let installer = std::sync::Arc::new(TokioMutex::new(
            Installer::new(&self.config.marketplace_install_dir)
                .map_err(|e| ChannelError::Telegram(e.to_string()))?,
        ));

        let on_event = std::sync::Arc::new(on_event);
        let handler = Update::filter_message().endpoint(move |bot: Bot, msg: Message| {
            let event = Self::message_to_event(&msg);
            let on_event = on_event.clone();
            let installer = installer.clone();
            let index_url = index_url.clone();
            let model_control = model_control.clone();
            let runtime_control = runtime_control.clone();
            let config_control = config_control.clone();
            let personality_control = personality_control.clone();
            let scheduler = scheduler.clone();
            let plugin_runtime = plugin_runtime.clone();
            let plugin_executor = plugin_executor.clone();
            let verbose_by_chat = verbose_by_chat.clone();
            let event_spine = event_spine.clone();
            let bot_username = bot_username.clone();
            let group_policy = group_policy.clone();
            let group_context = group_context.clone();
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
                                Self::send_reply_with_fallback(&bot, &msg, &help, None, event_spine.as_ref()).await;
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
                                let version = env!("CARGO_PKG_VERSION");
                                let commit = option_env!("GIT_COMMIT_HASH").unwrap_or("unknown");
                                let event_spine_status = if event_spine.is_some() { "active" } else { "disabled" };
                                let uptime = {
                                    use std::time::SystemTime;
                                    let secs = SystemTime::now()
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let pid_start = std::fs::read_to_string(format!("/proc/{}/stat", std::process::id()))
                                        .ok()
                                        .and_then(|s| s.split_whitespace().nth(21).and_then(|t| t.parse::<u64>().ok()))
                                        .map(|ticks| secs.saturating_sub(ticks / 100))
                                        .unwrap_or(0);
                                    let hours = pid_start / 3600;
                                    let mins = (pid_start % 3600) / 60;
                                    format!("{hours}h {mins}m")
                                };
                                let home = std::env::var("HOME").unwrap_or_else(|_| "~".into());
                                let status = format!(
                                    "Pares Agens v{version} ({commit})\n\
                                     PID: {} | RSS: {memory} | Uptime: {uptime}\n\
                                     Model: {model_line}\n\
                                     Event Spine: {event_spine_status}\n\
                                     Rendering: HTML + plain text fallback\n\
                                     Tool Governance: active (30s timeout)\n\
                                     PluresDB: {home}/.pares-agens/memory/",
                                    std::process::id(),
                                );
                                Self::send_reply_with_fallback(&bot, &msg, &status, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "version" => {
                                let version = env!("CARGO_PKG_VERSION");
                                let commit = option_env!("GIT_COMMIT_HASH").unwrap_or("unknown");
                                let text = format!("v{version} ({commit})");
                                Self::send_reply_with_fallback(&bot, &msg, &text, None, event_spine.as_ref()).await;
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
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "reasoning" => {
                                let Some(control) = &model_control else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Runtime reasoning controls are unavailable for this deployment.",
                                        None, event_spine.as_ref(),).await;
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
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "model" => {
                                let Some(control) = &model_control else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Runtime model switching is unavailable for this deployment.",
                                        None, event_spine.as_ref(),).await;
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

                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
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
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "config" => {
                                let Some(control) = &config_control else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Runtime config editing is unavailable for this deployment.",
                                        None, event_spine.as_ref(),).await;
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

                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "agents" | "browse" => {
                                let message = match fetch_marketplace_index(&index_url).await {
                                    Ok(skills) => format_index_listing(&skills),
                                    Err(e) => format!("Marketplace lookup failed: {e}"),
                                };
                                Self::send_reply_with_fallback(&bot, &msg, &message, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "install" => {
                                let Some(id) = cmd_parts.next() else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Usage: /install <id>",
                                        None, event_spine.as_ref(),).await;
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

                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "tools" => {
                                use pares_agens_core::tool_governance::ToolGovernor;
                                let gov = ToolGovernor::with_defaults();
                                let reply = gov.format_policies();
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "logs" => {
                                if !is_update_authorized(&msg) {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Logs denied. Configure PARES_TELEGRAM_UPDATE_ALLOWED_USERS with approved Telegram usernames or numeric IDs.",
                                        None, event_spine.as_ref(),).await;
                                    return respond(());
                                }
                                let tail_lines = match parse_logs_tail_lines(cmd_parts.collect()) {
                                    Ok(lines) => lines,
                                    Err(usage) => {
                                        let _ =
                                            Self::send_reply_with_fallback(&bot, &msg, usage, None, event_spine.as_ref()).await;
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
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "update" => {
                                if !is_update_authorized(&msg) {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Update denied. Configure PARES_TELEGRAM_UPDATE_ALLOWED_USERS with approved Telegram usernames or numeric IDs.",
                                        None, event_spine.as_ref(),).await;
                                    return respond(());
                                }
                                Self::send_reply_with_fallback(
                                    &bot,
                                    &msg,
                                    &format!(
                                        "Running self-update in `{}` for host `{}`.",
                                        update_flake_dir, update_host
                                    ),
                                    None, event_spine.as_ref(),).await;
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
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "personality" => {
                                let Some(control) = &personality_control else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Personality control is unavailable for this deployment.",
                                        None, event_spine.as_ref(),).await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };

                                let args: Vec<&str> = cmd_parts.collect();
                                let reply = match args.first().copied() {
                                    None | Some("show") => {
                                        control.show(Some("telegram")).await
                                    }
                                    Some("set") => {
                                        if args.get(1).copied() == Some("tone") {
                                            if let Some(tone) = args.get(2) {
                                                match control.set_tone(tone).await {
                                                    Ok(()) => format!("Tone updated to '{tone}'."),
                                                    Err(e) => format!("Failed: {e}"),
                                                }
                                            } else {
                                                "Usage: /personality set tone <tone>".to_string()
                                            }
                                        } else {
                                            "Usage: /personality set tone <tone>".to_string()
                                        }
                                    }
                                    Some("rule") => {
                                        match args.get(1).copied() {
                                            Some("add") => {
                                                let rule_text: String = args[2..].join(" ");
                                                if rule_text.is_empty() {
                                                    "Usage: /personality rule add <rule text>".to_string()
                                                } else {
                                                    match control.add_rule(&rule_text).await {
                                                        Ok(id) => format!("Rule added: {id}"),
                                                        Err(e) => format!("Failed: {e}"),
                                                    }
                                                }
                                            }
                                            Some("remove") | Some("rm") => {
                                                if let Some(id) = args.get(2) {
                                                    match control.remove_rule(id).await {
                                                        Ok(()) => format!("Rule '{id}' removed."),
                                                        Err(e) => format!("Failed: {e}"),
                                                    }
                                                } else {
                                                    "Usage: /personality rule remove <id>".to_string()
                                                }
                                            }
                                            _ => "Usage: /personality rule add <text> | rule remove <id>".to_string(),
                                        }
                                    }
                                    Some("docs") => {
                                        control.list_documents().await
                                    }
                                    Some("doc") => {
                                        if let Some(doc_type) = args.get(1).copied() {
                                            if args.get(2).copied() == Some("set") {
                                                let content: String = args[3..].join(" ");
                                                if content.is_empty() {
                                                    "Usage: /personality doc <type> set <text>".to_string()
                                                } else {
                                                    match control.set_document(doc_type, &content).await {
                                                        Ok(()) => format!("Document '{doc_type}' updated."),
                                                        Err(e) => format!("Failed: {e}"),
                                                    }
                                                }
                                            } else {
                                                control.get_document(doc_type).await
                                            }
                                        } else {
                                            "Usage: /personality doc <type> [set <text>]".to_string()
                                        }
                                    }
                                    _ => "Usage: /personality [show | set tone <t> | rule add <text> | rule remove <id> | docs | doc <type> [set <text>]]".to_string(),
                                };
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "cron" => {
                                let Some(sched) = &scheduler else {
                                    Self::send_reply_with_fallback(
                                        &bot,
                                        &msg,
                                        "Scheduler is unavailable for this deployment.",
                                        None, event_spine.as_ref(),).await;
                                    Self::acknowledge_message(&bot, &msg).await;
                                    return respond(());
                                };

                                let args: Vec<&str> = cmd_parts.collect();
                                let reply = match args.first().copied() {
                                    None | Some("list") => {
                                        let tasks = sched.list().await;
                                        if tasks.is_empty() {
                                            "No scheduled tasks.".to_string()
                                        } else {
                                            let mut out = String::from("Scheduled tasks:\n");
                                            for t in &tasks {
                                                let status = if t.enabled { "✓" } else { "⏸" };
                                                let last = t.last_run
                                                    .map(|d| d.format("%Y-%m-%d %H:%M").to_string())
                                                    .unwrap_or_else(|| "never".to_string());
                                                out.push_str(&format!(
                                                    "\n{status} {id} — {name}\n  Last: {last}",
                                                    id = t.id, name = t.name,
                                                ));
                                            }
                                            out
                                        }
                                    }
                                    Some("add") => {
                                        // Re-parse the full text for proper quoting
                                        match Scheduler::parse_cron_add(text) {
                                            Ok(task) => {
                                                let reply = format!("✓ Scheduled '{}' ({})", task.name, task.id);
                                                sched.add(task).await;
                                                reply
                                            }
                                            Err(e) => format!("Error: {e}\nUsage: /cron add '<schedule>' '<command>'"),
                                        }
                                    }
                                    Some("remove") | Some("rm") | Some("delete") => {
                                        if let Some(id) = args.get(1) {
                                            if sched.remove(id).await {
                                                format!("Task '{id}' removed.")
                                            } else {
                                                format!("Task '{id}' not found.")
                                            }
                                        } else {
                                            "Usage: /cron remove <id>".to_string()
                                        }
                                    }
                                    Some("pause") => {
                                        if let Some(id) = args.get(1) {
                                            if sched.set_enabled(id, false).await {
                                                format!("Task '{id}' paused.")
                                            } else {
                                                format!("Task '{id}' not found.")
                                            }
                                        } else {
                                            "Usage: /cron pause <id>".to_string()
                                        }
                                    }
                                    Some("resume") => {
                                        if let Some(id) = args.get(1) {
                                            if sched.set_enabled(id, true).await {
                                                format!("Task '{id}' resumed.")
                                            } else {
                                                format!("Task '{id}' not found.")
                                            }
                                        } else {
                                            "Usage: /cron resume <id>".to_string()
                                        }
                                    }
                                    Some(_) => "Usage: /cron [list | add '<schedule>' '<command>' | remove <id> | pause <id> | resume <id>]".to_string(),
                                };
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            "plugin" => {
                                let args: Vec<&str> = cmd_parts.collect();
                                let reply = if let (Some(rt), Some(ex)) = (&plugin_runtime, &plugin_executor) {
                                    match args.first().copied() {
                                        Some("list") | None => {
                                            let plugins = rt.list().await;
                                            if plugins.is_empty() {
                                                "No plugins installed.".to_string()
                                            } else {
                                                let mut out = String::from("Installed plugins:\n");
                                                for p in &plugins {
                                                    out.push_str(&format!("• {} v{}", p.name, p.version));
                                                    if !p.schema.entities.is_empty() {
                                                        let counts: Vec<String> = p.schema.entities.iter().map(|e| {
                                                            let c = ex.count(&p.name, &e.name);
                                                            format!("{}: {c}", e.display_name)
                                                        }).collect();
                                                        out.push_str(&format!(" ({})", counts.join(", ")));
                                                    }
                                                    out.push('\n');
                                                }
                                                out
                                            }
                                        }
                                        Some("install") => {
                                            if let Some(path) = args.get(1) {
                                                match tokio::fs::read_to_string(path).await {
                                                    Ok(content) => {
                                                        match rt.install_from_toml(&content).await {
                                                            Ok(name) => {
                                                                // Persist manifest
                                                                if let Some(manifest) = rt.get(&name).await {
                                                                    if let Ok(value) = serde_json::to_value(&manifest) {
                                                                        let _ = ex.persist_manifest(&name, &value);
                                                                    }
                                                                }
                                                                format!("✓ Plugin '{name}' installed.")
                                                            }
                                                            Err(e) => format!("Install failed: {e}"),
                                                        }
                                                    }
                                                    Err(e) => format!("Failed to read file: {e}"),
                                                }
                                            } else {
                                                "Usage: /plugin install <path>".to_string()
                                            }
                                        }
                                        Some("uninstall") => {
                                            if let Some(name) = args.get(1) {
                                                match rt.uninstall(name, false).await {
                                                    Ok(()) => {
                                                        let _ = ex.remove_manifest(name);
                                                        format!("✓ Plugin '{name}' uninstalled.")
                                                    }
                                                    Err(e) => format!("Uninstall failed: {e}"),
                                                }
                                            } else {
                                                "Usage: /plugin uninstall <name>".to_string()
                                            }
                                        }
                                        Some("schema") => {
                                            if let Some(name) = args.get(1) {
                                                if let Some(manifest) = rt.get(name).await {
                                                    let mut out = format!("Schema for {} v{}:\n", manifest.name, manifest.version);
                                                    for entity in &manifest.schema.entities {
                                                        out.push_str(&format!("\n{} {}:\n", entity.icon.as_deref().unwrap_or("📦"), entity.display_name));
                                                        for field in &entity.fields {
                                                            let req = if field.required { " (required)" } else { "" };
                                                            out.push_str(&format!("  • {} — {:?}{}\n", field.name, field.field_type, req));
                                                        }
                                                    }
                                                    if !manifest.schema.relationships.is_empty() {
                                                        out.push_str("\nRelationships:\n");
                                                        for rel in &manifest.schema.relationships {
                                                            out.push_str(&format!("  {} → {} ({})", rel.from_entity, rel.to_entity, rel.cardinality));
                                                        }
                                                    }
                                                    out
                                                } else {
                                                    format!("Plugin '{name}' not found.")
                                                }
                                            } else {
                                                "Usage: /plugin schema <name>".to_string()
                                            }
                                        }
                                        Some(sub) => format!("Unknown: /plugin {sub}. Use list, install, uninstall, or schema."),
                                    }
                                } else {
                                    "Plugin framework not initialized.".to_string()
                                };
                                Self::send_reply_with_fallback(&bot, &msg, &reply, None, event_spine.as_ref()).await;
                                Self::acknowledge_message(&bot, &msg).await;
                                return respond(());
                            }
                            _ => {} // fall through to agent
                        }
                    }
                }

                // Normal message — send to agent
                if let Some(mut event) = event {
                    // ── Group chat gate ──────────────────────────────────────
                    let is_group = Self::is_group_chat(&msg);
                    if is_group {
                        // Always record in context buffer for passive observation
                        if group_policy.passive_observe {
                            let sender_name = msg.from.as_ref()
                                .map(|u| u.username.as_deref().unwrap_or(&u.first_name).to_string())
                                .unwrap_or_else(|| "unknown".into());
                            let text = msg.text().unwrap_or_default().to_string();
                            let ts = msg.date.timestamp();
                            let mut ctx = group_context.lock().await;
                            ctx.push(msg.chat.id.0, GroupMessage {
                                sender: sender_name,
                                text,
                                timestamp: ts,
                            });
                        }

                        // Check if bot should respond
                        if !Self::should_respond_in_group(&msg, &bot_username, &group_policy) {
                            // Not triggered — optionally react and skip model call
                            debug!(chat_id = msg.chat.id.0, "group message not triggered, skipping");
                            return respond(());
                        }

                        // Triggered in group — inject context into event content
                        if group_policy.passive_observe {
                            if let Event::Message { content, .. } = &mut event {
                                let ctx_lock = group_context.lock().await;
                                if let Some(context_str) = ctx_lock.format_context(msg.chat.id.0) {
                                    *content = format!("{context_str}\n\n---\nTriggered message: {content}");
                                }
                            }
                        }
                    }
                    // ── End group chat gate ──────────────────────────────────
                    // Emit inbound message to event spine
                    if let Some(ref spine) = event_spine {
                        let (user, text) = match &event {
                            Event::Message { sender, content, .. } => (sender.clone(), content.clone()),
                            _ => ("unknown".to_string(), String::new()),
                        };
                        spine.emit_inbound_message(msg.chat.id.0, &user, &text);
                    }

                    let verbose_enabled = {
                        let lock = verbose_by_chat.lock().await;
                        *lock.get(&msg.chat.id.0).unwrap_or(&false)
                    };
                    if verbose_enabled {
                        if let Event::Message { content, .. } = &mut event {
                            *content = format!("{TELEGRAM_VERBOSE_TOOL_DETAILS_MARKER}{content}");
                        }
                    }
                    // Progressive delivery: send placeholder, keep typing, edit when done
                    let placeholder_bot = bot.clone();
                    let placeholder_msg = placeholder_bot
                        .send_message(msg.chat.id, "⏳")
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await;

                    let placeholder_id = placeholder_msg.as_ref().ok().map(|m| m.id);

                    // Keep typing indicator alive while agent processes
                    let typing_bot = bot.clone();
                    let typing_chat_id = msg.chat.id;
                    let typing_cancel = tokio_util::sync::CancellationToken::new();
                    let typing_token = typing_cancel.clone();
                    tokio::spawn(async move {
                        loop {
                            let _ = typing_bot.send_chat_action(typing_chat_id, ChatAction::Typing).await;
                            tokio::select! {
                                _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {},
                                _ = typing_token.cancelled() => break,
                            }
                        }
                    });

                    if let Some(Event::ModelResponse {
                        request_id, content, ..
                    }) = on_event(event).await
                    {
                        typing_cancel.cancel();

                        // Emit model response to event spine
                        if let Some(ref spine) = event_spine {
                            spine.emit_model_response(msg.chat.id.0, "telegram", &content);
                        }

                        let reply_markup = if Self::is_approval_prompt(&content) {
                            Some(Self::approval_keyboard(&request_id))
                        } else {
                            None
                        };

                        // Try to edit the placeholder with the response
                        let delivered_via_edit = if let Some(pid) = placeholder_id {
                            Self::edit_placeholder_with_response(
                                &bot, msg.chat.id, pid, &content, reply_markup.clone(), event_spine.as_ref()
                            ).await
                        } else {
                            None
                        };

                        if let Some((format_used, msg_ids)) = delivered_via_edit {
                            // Delivered by editing the placeholder
                            if let Some(ref spine) = event_spine {
                                for mid in &msg_ids {
                                    spine.emit_delivery_success(msg.chat.id.0, "telegram", *mid, format_used);
                                }
                            }
                            Self::acknowledge_message(&bot, &msg).await;
                        } else {
                            // Placeholder edit failed or wasn't possible — delete placeholder and send fresh
                            if let Some(pid) = placeholder_id {
                                let _ = bot.delete_message(msg.chat.id, pid).await;
                            }
                            match Self::send_html_reply(&bot, &msg, &content, reply_markup, event_spine.as_ref()).await {
                                Ok((format_used, msg_ids)) => {
                                    if let Some(ref spine) = event_spine {
                                        for mid in &msg_ids {
                                            spine.emit_delivery_success(msg.chat.id.0, "telegram", *mid, format_used);
                                        }
                                    }
                                    Self::acknowledge_message(&bot, &msg).await;
                                }
                                Err(e) => {
                                    error!("Failed to send Telegram reply: {e}");
                                }
                            }
                        }
                    } else {
                        typing_cancel.cancel();
                        // No response — delete the placeholder
                        if let Some(pid) = placeholder_id {
                            let _ = bot.delete_message(msg.chat.id, pid).await;
                        }
                    }
                }
                respond(())
            }
        });

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

    // ── HTML renderer integration tests ───────────────────────────────────

    #[test]
    fn html_render_typical_model_output() {
        use pares_agens_core::channel_contract::ChannelContract;
        use pares_agens_core::renderers::telegram as renderer;

        let contract = ChannelContract::telegram();
        let content = "**Bold text** and *italic* with `code` and a [link](https://example.com)\n\n```rust\nfn main() {}\n```";
        let (html_chunks, plain_chunks) = renderer::render_for_telegram(content, &contract);

        assert!(!html_chunks.is_empty());
        assert!(html_chunks[0].contains("<b>Bold text</b>"));
        assert!(html_chunks[0].contains("<i>italic</i>"));
        assert!(html_chunks[0].contains("<code>code</code>"));

        assert!(!plain_chunks.is_empty());
        assert!(!plain_chunks[0].contains("<b>"));
    }

    #[test]
    fn html_render_chunking_long_message() {
        use pares_agens_core::channel_contract::ChannelContract;
        use pares_agens_core::renderers::telegram as renderer;

        let contract = ChannelContract::telegram();
        // Create a message that's definitely > 4096 chars
        let long_content = "Hello world\n".repeat(500);
        let (html_chunks, plain_chunks) = renderer::render_for_telegram(&long_content, &contract);

        assert!(html_chunks.len() > 1, "should chunk into multiple messages");
        assert!(plain_chunks.len() > 1);
        for chunk in &html_chunks {
            assert!(chunk.len() <= 4096, "each chunk must be within limit");
        }
    }

    #[test]
    fn html_render_html_entities_escaped() {
        use pares_agens_core::channel_contract::ChannelContract;
        use pares_agens_core::renderers::telegram as renderer;

        let contract = ChannelContract::telegram();
        let content = "Use a < b && c > d for comparison";
        let (html_chunks, _) = renderer::render_for_telegram(content, &contract);
        assert!(html_chunks[0].contains("&lt;"));
        assert!(html_chunks[0].contains("&amp;"));
        assert!(html_chunks[0].contains("&gt;"));
    }

    #[test]
    fn telegram_adapter_with_event_spine() {
        use pares_agens_core::event_spine::EventSpineHandle;
        use pluresdb::CrdtStore;

        let store = std::sync::Arc::new(CrdtStore::default());
        let handle = EventSpineHandle::from_arc_store(store, "test");
        let config = TelegramConfig::new("test-token");
        let adapter = TelegramAdapter::with_event_spine(config, handle);
        assert_eq!(adapter.name(), "telegram");
        assert!(adapter.event_spine.is_some());
    }
}
