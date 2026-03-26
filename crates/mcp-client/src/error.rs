//! Error types for the MCP client.

use thiserror::Error;

/// Errors that can occur in the MCP client.
#[derive(Debug, Error)]
pub enum McpError {
    /// A transport-level error occurred (e.g. connection refused).
    #[error("transport error: {0}")]
    Transport(String),

    /// The JSON-RPC server returned an application-level error.
    #[error("JSON-RPC error {code}: {message}")]
    JsonRpc {
        /// The JSON-RPC error code returned by the server.
        code: i64,
        /// Human-readable description of the JSON-RPC error.
        message: String,
    },

    /// JSON serialization or deserialization failed.
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// An HTTP-level error occurred.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// An I/O error occurred.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// The requested tool was not found on the MCP server.
    #[error("tool not found: {0}")]
    ToolNotFound(String),

    /// The server returned a response that did not match the expected format.
    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}

/// Convenience `Result` type for MCP client operations.
pub type Result<T> = std::result::Result<T, McpError>;
