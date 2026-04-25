//! Provider and router configuration, plus the PluresDB [`ConfigStore`] abstraction.

use std::collections::HashMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Error;

/// Canonical provider name for local BitNet inference.
pub const LOCAL_BITNET_PROVIDER: &str = "local-bitnet";

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
    /// Constructs a new [`ProviderConfig`].
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key,
        }
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
    /// Ordered list of fallback models to try when the primary model returns
    /// a client error (4xx). Each entry is a model name string routed through
    /// the normal provider selection logic.
    #[serde(default)]
    pub fallback_models: Vec<String>,
}

impl RouterConfig {
    /// Build a simple single-provider config with no routing rules.
    pub fn single(name: impl Into<String>, provider: ProviderConfig) -> Self {
        let name = name.into();
        Self {
            providers: HashMap::from([(name.clone(), provider)]),
            rules: vec![],
            default_provider: name,
            fallback_models: vec![],
        }
    }

    /// Build a single-provider config wired to the canonical local BitNet
    /// provider name (`"local-bitnet"`).
    pub fn local_bitnet(base_url: impl Into<String>) -> Self {
        Self::single(LOCAL_BITNET_PROVIDER, ProviderConfig::new(base_url, None))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_new() {
        let p = ProviderConfig::new("http://localhost:12434", Some("sk-key".into()));
        assert_eq!(p.base_url, "http://localhost:12434");
        assert_eq!(p.api_key.as_deref(), Some("sk-key"));
    }

    #[test]
    fn provider_config_no_key() {
        let p = ProviderConfig::new("http://localhost:11434", None);
        assert!(p.api_key.is_none());
    }

    #[test]
    fn provider_config_serde_skips_none_api_key() {
        let p = ProviderConfig::new("http://host", None);
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("api_key"));
    }

    #[test]
    fn provider_config_serde_roundtrip_with_key() {
        let p = ProviderConfig::new("http://host", Some("token".into()));
        let json = serde_json::to_string(&p).unwrap();
        let decoded: ProviderConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.api_key.as_deref(), Some("token"));
    }

    #[test]
    fn router_config_single_constructor() {
        let p = ProviderConfig::new("http://local", None);
        let cfg = RouterConfig::single("local", p.clone());
        assert_eq!(cfg.default_provider, "local");
        assert!(cfg.rules.is_empty());
        assert!(cfg.providers.contains_key("local"));
    }

    #[test]
    fn router_config_local_bitnet_constructor() {
        let cfg = RouterConfig::local_bitnet("http://127.0.0.1:12434");
        assert_eq!(cfg.default_provider, LOCAL_BITNET_PROVIDER);
        assert!(cfg.providers.contains_key(LOCAL_BITNET_PROVIDER));
        assert!(cfg.rules.is_empty());
        assert_eq!(
            cfg.providers[LOCAL_BITNET_PROVIDER].base_url,
            "http://127.0.0.1:12434"
        );
        assert!(cfg.providers[LOCAL_BITNET_PROVIDER].api_key.is_none());
    }

    #[test]
    fn routing_rule_serde_roundtrip() {
        let rule = RoutingRule {
            model_prefix: Some("gpt-".into()),
            provider: "openai".into(),
        };
        let json = serde_json::to_string(&rule).unwrap();
        let decoded: RoutingRule = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.model_prefix.as_deref(), Some("gpt-"));
        assert_eq!(decoded.provider, "openai");
    }

    #[test]
    fn routing_rule_without_prefix_serde() {
        let rule = RoutingRule {
            model_prefix: None,
            provider: "fallback".into(),
        };
        let json = serde_json::to_string(&rule).unwrap();
        assert!(!json.contains("model_prefix"));
        let decoded: RoutingRule = serde_json::from_str(&json).unwrap();
        assert!(decoded.model_prefix.is_none());
    }

    #[test]
    fn router_config_serde_roundtrip() {
        let cfg = RouterConfig::single("x", ProviderConfig::new("http://x", None));
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: RouterConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.default_provider, "x");
    }
}
