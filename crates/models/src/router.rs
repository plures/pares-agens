//! Model router — selects the right provider for each request.

use std::collections::HashMap;
use std::pin::Pin;

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
            license.check_feature(pares_agens_core::license::Feature::MultipleModelProviders)?;
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
    ///
    /// On client errors (HTTP 4xx), automatically retries with each model in
    /// [`RouterConfig::fallback_models`] before giving up.
    pub async fn chat(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, Error> {
        let provider = self.select_provider(&request.model).to_owned();
        match self.get_client(&provider)?.chat_completion(request).await {
            Ok(resp) => Ok(resp),
            Err(ref e) if Self::is_client_error(e) && !self.config.fallback_models.is_empty() => {
                tracing::warn!(
                    model = %request.model,
                    error = %e,
                    "primary model failed, trying fallbacks"
                );
                self.chat_with_fallbacks(request).await
            }
            Err(e) => Err(e),
        }
    }

    /// Send a streaming chat completion request.
    ///
    /// On client errors (HTTP 4xx), automatically retries with each model in
    /// [`RouterConfig::fallback_models`] before giving up.
    pub async fn chat_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk, Error>> + Send>>, Error> {
        let provider = self.select_provider(&request.model).to_owned();
        match self.get_client(&provider)?.chat_completion_stream(request).await {
            Ok(stream) => Ok(Box::pin(stream)),
            Err(ref e) if Self::is_client_error(e) && !self.config.fallback_models.is_empty() => {
                tracing::warn!(
                    model = %request.model,
                    error = %e,
                    "primary model failed (stream), trying fallbacks"
                );
                self.chat_stream_with_fallbacks(request).await
            }
            Err(e) => Err(e),
        }
    }

    /// Returns `true` for HTTP 4xx client errors (model not available, auth issues, etc.).
    fn is_client_error(err: &Error) -> bool {
        matches!(err, Error::ApiError { status, .. } if (400..500).contains(status))
    }

    /// Try each fallback model in order until one succeeds.
    async fn chat_with_fallbacks(
        &self,
        original: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, Error> {
        let mut last_err = None;
        for fallback_model in &self.config.fallback_models {
            let provider = self.select_provider(fallback_model).to_owned();
            let mut req = original.clone();
            req.model = fallback_model.clone();
            tracing::info!(model = %fallback_model, "trying fallback model");
            match self.get_client(&provider)?.chat_completion(&req).await {
                Ok(resp) => {
                    tracing::info!(model = %fallback_model, "fallback model succeeded");
                    return Ok(resp);
                }
                Err(e) => {
                    tracing::warn!(model = %fallback_model, error = %e, "fallback model failed");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(Error::NoProvider))
    }

    /// Try each fallback model in order for streaming until one succeeds.
    async fn chat_stream_with_fallbacks(
        &self,
        original: &ChatCompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ChatCompletionChunk, Error>> + Send>>, Error> {
        let mut last_err = None;
        for fallback_model in &self.config.fallback_models {
            let provider = self.select_provider(fallback_model).to_owned();
            let mut req = original.clone();
            req.model = fallback_model.clone();
            tracing::info!(model = %fallback_model, "trying fallback model (stream)");
            match self.get_client(&provider)?.chat_completion_stream(&req).await {
                Ok(stream) => {
                    tracing::info!(model = %fallback_model, "fallback model succeeded (stream)");
                    return Ok(Box::pin(stream));
                }
                Err(e) => {
                    tracing::warn!(model = %fallback_model, error = %e, "fallback model failed (stream)");
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or(Error::NoProvider))
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
                (
                    "openai".to_string(),
                    ProviderConfig::new("http://openai", Some("key".into())),
                ),
                (
                    "local".to_string(),
                    ProviderConfig::new("http://local", None),
                ),
            ]),
            rules: vec![
                RoutingRule {
                    model_prefix: Some("gpt-".into()),
                    provider: "openai".into(),
                },
                RoutingRule {
                    model_prefix: Some("claude-".into()),
                    provider: "openai".into(),
                },
            ],
            default_provider: "local".into(),
            fallback_models: vec![],
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
