//! Low-level OpenAI-compatible HTTP client.

use futures_util::Stream;
use reqwest::Client;
use tracing::debug;

use crate::{
    error::Error,
    streaming::parse_sse_stream,
    types::{ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse},
};

/// HTTP client for a single OpenAI-compatible endpoint.
///
/// Use [`crate::router::ModelRouter`] when you need multi-provider routing.
#[derive(Debug, Clone)]
pub struct OpenAiClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
}

impl OpenAiClient {
    /// Create a new client.
    ///
    /// * `base_url` — root URL of the endpoint, e.g. `https://api.openai.com`
    ///   or `http://localhost:12434`.
    /// * `api_key` — bearer token; pass `None` for unauthenticated local servers.
    pub fn new(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into(),
            api_key,
        }
    }

    fn completions_url(&self) -> String {
        format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        )
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => req.bearer_auth(key),
            None => req,
        }
    }

    /// Send a non-streaming `/v1/chat/completions` request and return the full response.
    pub async fn chat_completion(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, Error> {
        debug!(model = %request.model, "sending chat completion request");

        let req = self.apply_auth(self.client.post(self.completions_url()).json(request));
        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ApiError { status, body });
        }

        Ok(response.json::<ChatCompletionResponse>().await?)
    }

    /// Send a streaming `/v1/chat/completions` request.
    ///
    /// Returns an async [`Stream`] that yields [`ChatCompletionChunk`]s as they
    /// arrive over SSE.
    pub async fn chat_completion_stream(
        &self,
        request: &ChatCompletionRequest,
    ) -> Result<impl Stream<Item = Result<ChatCompletionChunk, Error>>, Error> {
        debug!(model = %request.model, "sending streaming chat completion request");

        // Force streaming on.
        let mut req_body = request.clone();
        req_body.stream = Some(true);

        let req = self.apply_auth(self.client.post(self.completions_url()).json(&req_body));
        let response = req.send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::ApiError { status, body });
        }

        Ok(parse_sse_stream(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_url_appends_path() {
        let c = OpenAiClient::new("https://api.openai.com", None);
        assert_eq!(
            c.completions_url(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn completions_url_trims_trailing_slash() {
        let c = OpenAiClient::new("http://localhost:12434/", None);
        assert_eq!(
            c.completions_url(),
            "http://localhost:12434/v1/chat/completions"
        );
    }

    #[test]
    fn client_stores_api_key() {
        let c = OpenAiClient::new("http://host", Some("sk-abc".into()));
        assert_eq!(c.api_key.as_deref(), Some("sk-abc"));
    }

    #[test]
    fn client_stores_no_api_key() {
        let c = OpenAiClient::new("http://host", None);
        assert!(c.api_key.is_none());
    }
}
