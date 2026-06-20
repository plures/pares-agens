//! Persistent configuration file support for pares-radix.
//!
//! Config file location: `~/.config/pares-radix/config.toml` (XDG standard).
//! CLI args and env vars override config values.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RadixConfig {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub memory: MemoryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Primary model name
    #[serde(default = "default_model")]
    pub primary: String,
    /// Deep/escalation model
    #[serde(default = "default_deep_model")]
    pub deep: String,
    /// API endpoint
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Use GitHub Copilot auth
    #[serde(default = "default_copilot")]
    pub copilot: bool,
    /// Fallback models
    #[serde(default)]
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (off, error, warn, info, debug, trace)
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Log directory
    #[serde(default = "default_log_dir")]
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// PluresDB store path
    #[serde(default = "default_memory_path")]
    pub path: PathBuf,
}

fn default_model() -> String {
    "auto".into()
}
fn default_deep_model() -> String {
    "auto".into()
}
fn default_endpoint() -> String {
    "https://api.individual.githubcopilot.com".into()
}
fn default_copilot() -> bool {
    true
}
fn default_log_level() -> String {
    "info".into()
}
fn default_log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".pares-radix/logs")
}
fn default_memory_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".pares-radix/memory")
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            primary: default_model(),
            deep: default_deep_model(),
            endpoint: default_endpoint(),
            copilot: default_copilot(),
            fallbacks: vec![],
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            dir: default_log_dir(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            path: default_memory_path(),
        }
    }
}

impl RadixConfig {
    /// Load config from disk, or create a default config file on first run.
    pub fn load() -> Self {
        let config_path = Self::config_path();
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).unwrap_or_default();
            toml::from_str(&content).unwrap_or_default()
        } else {
            let config = Self::default();
            if let Some(parent) = config_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(content) = toml::to_string_pretty(&config) {
                let _ = std::fs::write(&config_path, content);
            }
            config
        }
    }

    /// Path to the config file.
    pub fn config_path() -> PathBuf {
        if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(xdg).join("pares-radix/config.toml");
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".config/pares-radix/config.toml")
    }
}
