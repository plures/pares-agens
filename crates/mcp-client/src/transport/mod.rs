use async_trait::async_trait;
use crate::error::Result;
use crate::protocol::{JsonRpcRequest, JsonRpcResponse};

pub mod http;
pub mod stdio;

/// A transport layer that sends JSON-RPC requests and receives responses.
#[async_trait]
pub trait Transport: Send {
    /// Send a request and return the corresponding response.
    async fn send(&mut self, request: JsonRpcRequest) -> Result<JsonRpcResponse>;
}
