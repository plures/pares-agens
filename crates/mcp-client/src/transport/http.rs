//! HTTP transport: sends JSON-RPC 2.0 requests as HTTP POST to a configured
//! URL and reads the response body as JSON.
//!
//! The caller is responsible for providing the appropriate endpoint (for
//! example, an MCP `/message` URL) when constructing the transport.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;

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
    next_id: u64,
}

impl HttpTransport {
    /// Create a new transport that posts to `url`.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            url: url.into(),
            next_id: 1,
        }
    }

    /// Create a new transport with a pre-configured [`reqwest::Client`].
    pub fn with_client(client: Client, url: impl Into<String>) -> Self {
        Self {
            client,
            url: url.into(),
            next_id: 1,
        }
    }

    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::Number(id.into())
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&mut self, mut request: JsonRpcRequest) -> Result<JsonRpcResponse> {
        if request.id.is_null() {
            request.id = self.next_id();
        }

        let response = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json::<JsonRpcResponse>()
            .await?;

        Ok(response)
    }
}
