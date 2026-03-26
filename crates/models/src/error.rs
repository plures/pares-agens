//! Typed error variants for `pares-models`.

use thiserror::Error;

/// All errors that can arise when using the `pares-models` client.
#[derive(Debug, Error)]
pub enum Error {
    /// An underlying HTTP transport error from `reqwest`.
    #[error("HTTP transport error: {0}")]
    Http(#[from] reqwest::Error),

    /// The API returned a non-2xx status code.
    #[error("API error {status}: {body}")]
    ApiError {
        /// HTTP status code returned by the server.
        status: u16,
        /// Response body text (may contain the provider's error message).
        body: String,
    },

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// The requested provider name has no matching entry in the router config.
    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    /// No providers are configured at all.
    #[error("no providers configured")]
    NoProvider,

    /// An error occurred while reading an SSE stream.
    #[error("stream error: {0}")]
    Stream(String),
}
