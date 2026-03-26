//! HTTP transport: sends JSON-RPC 2.0 requests as HTTP POST to a configured
//! URL and reads the response body as JSON.
//!
//! The caller is responsible for providing the appropriate endpoint (for
//! example, an MCP `/message` URL) when constructing the transport.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::{
    error::Result,
    protocol::{JsonRpcRequest, JsonRpcResponse},
};

use super::Transport;

/// Sends every JSON-RPC request as an HTTP POST and parses the body as a
/// `JsonRpcResponse`.
pub struct HttpTransport {
    client: Client,
    url: String,
}

impl HttpTransport {
    /// Create a new transport that posts to `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            url: url.into(),
        }
    }

    /// Create a new transport with a pre-configured [`reqwest::Client`].
    pub fn with_client(client: Client, url: impl Into<String>) -> Self {
        Self {
            client,
            url: url.into(),
        }
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        let is_notification = request.id.is_none();

        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?;

        // Notifications don't require a response body.
        if is_notification {
            return Ok(JsonRpcResponse {
                jsonrpc: "2.0".into(),
                id: Value::Null,
                result: Some(json!({})),
                error: None,
            });
        }

        Ok(response.json::<JsonRpcResponse>().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_transport_new_stores_url() {
        let t = HttpTransport::new("http://localhost:3000/message");
        assert_eq!(t.url, "http://localhost:3000/message");
    }

    #[test]
    fn http_transport_with_client_stores_url() {
        let client = Client::new();
        let t = HttpTransport::with_client(client, "http://custom:8080/mcp");
        assert_eq!(t.url, "http://custom:8080/mcp");
    }
}
