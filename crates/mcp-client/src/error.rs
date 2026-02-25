use thiserror::Error;

/// Errors that can occur in the MCP client.
#[derive(Debug, Error)]
pub enum McpError {
    #[error("transport error: {0}")]
    Transport(String),

    #[error("JSON-RPC error {code}: {message}")]
    JsonRpc { code: i64, message: String },

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tool not found: {0}")]
    ToolNotFound(String),

    #[error("unexpected response: {0}")]
    UnexpectedResponse(String),
}

pub type Result<T> = std::result::Result<T, McpError>;
