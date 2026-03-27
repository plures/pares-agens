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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_error_display() {
        let e = McpError::Transport("connection refused".into());
        assert!(e.to_string().contains("connection refused"));
    }

    #[test]
    fn jsonrpc_error_display_includes_code_and_message() {
        let e = McpError::JsonRpc {
            code: -32601,
            message: "Method not found".into(),
        };
        let s = e.to_string();
        assert!(s.contains("-32601"));
        assert!(s.contains("Method not found"));
    }

    #[test]
    fn tool_not_found_error_display() {
        let e = McpError::ToolNotFound("search_web".into());
        assert!(e.to_string().contains("search_web"));
    }

    #[test]
    fn unexpected_response_error_display() {
        let e = McpError::UnexpectedResponse("missing result field".into());
        assert!(e.to_string().contains("missing result field"));
    }

    #[test]
    fn json_error_converted_from_serde() {
        let json_err = serde_json::from_str::<serde_json::Value>("{not json}").unwrap_err();
        let e: McpError = json_err.into();
        assert!(matches!(e, McpError::Json(_)));
    }

    #[test]
    fn io_error_converted_from_std_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let e: McpError = io_err.into();
        assert!(matches!(e, McpError::Io(_)));
    }
}
