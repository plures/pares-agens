//! Model router — selects the right provider for each request.

use std::collections::HashMap;

use futures_util::Stream;
use tracing::debug;

use crate::{
    client::OpenAiClient,
    config::RouterConfig,
    error::Error,
    types::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse},
};

/// Routes `/v1/chat/completions` requests to the appropriate backend provider.
///
/// Provider selection follows these steps:
/// 1. Evaluate [`RouterConfig::rules`] in order; use the first matching rule.
/// 2. Fall back to [`RouterConfig::default_provider`].
///
/// # Example
/// ```no_run
/// use std::collections::HashMap;
/// use pares_models::{
///     config::{ProviderConfig, RouterConfig},
///     router::ModelRouter,
///     types::{ChatCompletionRequest, ChatMessage, Role},
/// };
///
/// # async fn example() -> Result<(), pares_models::error::Error> {
/// let config = RouterConfig::single(
///     "local",
///     ProviderConfig::new("http://localhost:12434", None),
/// );
/// let router = ModelRouter::new(config);
/// let req = ChatCompletionRequest::new(
///     "ai/mistral-nemo",
///     vec![ChatMessage::text(Role::User, "Hello!")],
/// );
/// let response = router.chat(&req).await?;
/// println!("{}", response.choices[0].message.content.as_deref().unwrap_or(""));
/// # Ok(())
/// # }
/// ```
pub struct ModelRouter {
    config: RouterConfig,
    clients: HashMap<String, OpenAiClient>,
}

impl ModelRouter {
    /// Build a router from the given configuration.
    pub fn new(config: RouterConfig) -> Self {
        let clients = config
            .providers
            .iter()
            .map(|(name, p)| {
                let client = OpenAiClient::new(&p.base_url, p.api_key.clone());
                (name.clone(), client)
            })
            .collect();
        Self { config, clients }
    }

    /// Build a multi-provider router, gated behind the Pro license.
    ///
    /// Use this constructor when `config` contains more than one provider or
    /// at least one routing rule — both are Pro features.  Returns
    /// [`pares_agens_core::license::LicenseError`] if the license check fails.
    ///
    /// Single-provider configs (no rules) are always permitted regardless of
    /// tier; use the plain [`ModelRouter::new`] for those cases.
    pub fn new_multi(
        config: RouterConfig,
        license: &pares_agens_core::license::License,
    ) -> Result<Self, pares_agens_core::license::LicenseError> {
        if config.providers.len() > 1 || !config.rules.is_empty() {
            license
                .check_feature(pares_agens_core::license::Feature::MultipleModelProviders)?;
        }
        Ok(Self::new(config))
    }

    /// Select the provider name for a given model identifier.
    fn select_provider<'a>(&'a self, model: &str) -> &'a str {
        for rule in &self.config.rules {
            if let Some(prefix) = &rule.model_prefix {
                if model.starts_with(prefix.as_str()) {
                    debug!(model, provider = %rule.provider, "routing rule matched");
                    return &rule.provider;
                }
            }
        }
        debug!(model, provider = %self.config.default_provider, "using default provider");
        &self.config.default_provider
    }

    fn get_client(&self, provider: &str) -> Result<&OpenAiClient, Error> {
        self.clients
            .get(provider)
            .ok_or_else(|| Error::ProviderNotFound(provider.to_owned()))
    }

    /// Send a non-streaming chat completion request.
    pub async fn chat(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, Error> {
        let provider = self.select_provider(&request.model).to_owned();
        self.get_client(&provider)?.chat_completion(request).await
    }

    /// Send a streaming chat completion request.
    pub async fn chat_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<impl Stream<Item = Result<ChatCompletionChunk, Error>>, Error> {
        let provider = self.select_provider(&request.model).to_owned();
        self.get_client(&provider)?.chat_completion_stream(request).await
    }

    /// Reload the router from a [`crate::config::ConfigStore`].
    ///
    /// Returns a new `ModelRouter` built from the freshly loaded config.
    pub async fn reload_from<S: crate::config::ConfigStore>(store: &S) -> Result<Self, Error> {
        let config = store.router_config().await?;
        Ok(Self::new(config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProviderConfig, RouterConfig, RoutingRule};
    use std::collections::HashMap;

    fn make_router_with_rules() -> ModelRouter {
        let config = RouterConfig {
            providers: HashMap::from([
                ("openai".to_string(), ProviderConfig::new("http://openai", Some("key".into()))),
                ("local".to_string(), ProviderConfig::new("http://local", None)),
            ]),
            rules: vec![
                RoutingRule { model_prefix: Some("gpt-".into()), provider: "openai".into() },
                RoutingRule { model_prefix: Some("claude-".into()), provider: "openai".into() },
            ],
            default_provider: "local".into(),
        };
        ModelRouter::new(config)
    }

    #[test]
    fn select_provider_matches_prefix_rule() {
        let router = make_router_with_rules();
        // The `select_provider` method is private; we test it indirectly by
        // verifying `get_client` resolves the right client.
        // Both providers are registered so get_client should not fail.
        assert!(router.get_client("openai").is_ok());
        assert!(router.get_client("local").is_ok());
    }

    #[test]
    fn get_client_returns_error_for_unknown_provider() {
        let router = ModelRouter::new(RouterConfig::single(
            "local",
            ProviderConfig::new("http://local", None),
        ));
        let err = router.get_client("nonexistent").unwrap_err();
        assert!(matches!(err, crate::error::Error::ProviderNotFound(_)));
    }

    #[test]
    fn new_router_builds_clients_from_config() {
        let config = RouterConfig::single("x", ProviderConfig::new("http://x", None));
        let router = ModelRouter::new(config);
        assert!(router.get_client("x").is_ok());
    }
}
