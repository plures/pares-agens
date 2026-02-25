//! Provider and router configuration, plus the PluresDB [`ConfigStore`] abstraction.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Error;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Connection details for a single model provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Base URL of the OpenAI-compatible endpoint, e.g. `http://localhost:12434`.
    pub base_url: String,
    /// Optional bearer token / API key sent as `Authorization: Bearer <key>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl ProviderConfig {
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self { base_url: base_url.into(), api_key }
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// A single routing rule.
///
/// If `model_prefix` is `Some`, the rule only matches models whose name starts
/// with that prefix (e.g. `"gpt-"` matches `"gpt-4o"`).
/// Rules are evaluated in order; the first match wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    /// Optional model-name prefix to match against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_prefix: Option<String>,
    /// Name of the provider (key in [`RouterConfig::providers`]) to route to.
    pub provider: String,
}

/// Full router configuration: a set of named providers plus an ordered list of
/// routing rules and a fallback default provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouterConfig {
    /// Named provider configurations.
    pub providers: HashMap<String, ProviderConfig>,
    /// Ordered routing rules evaluated against each request's model name.
    #[serde(default)]
    pub rules: Vec<RoutingRule>,
    /// Provider name to use when no rule matches.
    pub default_provider: String,
}

impl RouterConfig {
    /// Build a simple single-provider config with no routing rules.
    pub fn single(name: impl Into<String>, provider: ProviderConfig) -> Self {
        let name = name.into();
        Self {
            providers: HashMap::from([(name.clone(), provider)]),
            rules: vec![],
            default_provider: name,
        }
    }
}

// ---------------------------------------------------------------------------
// ConfigStore — PluresDB integration hook
// ---------------------------------------------------------------------------

/// Trait for loading [`RouterConfig`] from a persistent store (PluresDB).
///
/// Implement this on a PluresDB client to allow the router to reload its
/// configuration from application state at runtime.
#[async_trait]
pub trait ConfigStore: Send + Sync {
    /// Return the current router configuration.
    async fn router_config(&self) -> Result<RouterConfig, Error>;
}
