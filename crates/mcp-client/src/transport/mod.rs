//! Transport abstraction for the MCP client.
//!
//! Provides the [`Transport`] trait plus two built-in implementations:
//! - [`stdio::StdioTransport`] — communicates with a spawned subprocess over stdin/stdout.
//! - [`http::HttpTransport`] — sends requests as HTTP POST to a configured URL.

use crate::error::Result;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};
use async_trait::async_trait;

pub mod http;
pub mod stdio;

/// A transport layer that sends JSON-RPC requests and receives responses.
#[async_trait]
pub trait Transport: Send {
    /// Send a request and return the corresponding response.
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse>;
}
