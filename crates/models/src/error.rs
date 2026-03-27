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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_error_display_includes_status_and_body() {
        let e = Error::ApiError {
            status: 429,
            body: "rate limit exceeded".into(),
        };
        let s = e.to_string();
        assert!(s.contains("429"));
        assert!(s.contains("rate limit exceeded"));
    }

    #[test]
    fn provider_not_found_display() {
        let e = Error::ProviderNotFound("ghost-provider".into());
        assert!(e.to_string().contains("ghost-provider"));
    }

    #[test]
    fn no_provider_display() {
        let e = Error::NoProvider;
        assert!(e.to_string().contains("no providers configured"));
    }

    #[test]
    fn stream_error_display() {
        let e = Error::Stream("unexpected EOF".into());
        assert!(e.to_string().contains("unexpected EOF"));
    }

    #[test]
    fn json_error_converted_from_serde() {
        let json_err = serde_json::from_str::<serde_json::Value>("{not json}").unwrap_err();
        let e: Error = json_err.into();
        assert!(matches!(e, Error::Json(_)));
    }
}
