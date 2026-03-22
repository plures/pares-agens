//! Error types for `pares-agens-inference`.

use thiserror::Error;

/// All errors that can arise in the inference crate.
#[derive(Debug, Error)]
pub enum InferenceError {
    /// The requested model is not available locally and has not been downloaded.
    #[error("model not found: {0}")]
    ModelNotFound(String),

    /// The model file on disk is corrupt or has an unexpected format.
    #[error("invalid model file: {0}")]
    InvalidModel(String),

    /// The native bitnet.cpp backend returned a non-zero status code.
    #[error("bitnet backend error (code {code}): {message}")]
    BackendError { code: i32, message: String },

    /// The native `bitnet` feature was not enabled at compile time.
    #[error(
        "native inference unavailable — recompile with `--features native` \
         and initialise the bitnet.cpp submodule"
    )]
    NativeUnavailable,

    /// An I/O error while reading the model file or the cache directory.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// An HTTP error while downloading a model from HuggingFace.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// The HuggingFace download URL for the requested model is unknown.
    #[error("unknown model id: {0}")]
    UnknownModel(String),

    /// The download was interrupted or produced a file that does not match the
    /// expected size.
    #[error("download incomplete: expected {expected} bytes, got {got}")]
    DownloadIncomplete { expected: u64, got: u64 },

    /// Token generation was cancelled or timed out by the caller.
    #[error("generation cancelled")]
    Cancelled,

    /// A parameter value is out of the acceptable range.
    #[error("invalid parameter: {0}")]
    InvalidParam(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
