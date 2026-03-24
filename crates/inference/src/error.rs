use thiserror::Error;

/// All errors that can surface from the inference layer.
#[derive(Debug, Error)]
pub enum InferenceError {
    /// The `native` Cargo feature was not enabled at compile time.
    ///
    /// Enable it with `cargo build --features native` (requires the
    /// `third_party/bitnet` submodule and CMake ≥ 3.21).
    #[error(
        "native inference is unavailable: recompile with the `native` feature enabled"
    )]
    NativeUnavailable,

    /// The model file could not be loaded (file not found, wrong format, etc.).
    #[error("failed to load model from `{path}`: {reason}")]
    ModelLoad { path: String, reason: String },

    /// An inference context could not be created (out of memory, etc.).
    #[error("failed to create inference context: {0}")]
    ContextCreate(String),

    /// The process ran out of memory during model loading or inference.
    #[error("out of memory during inference: {0}")]
    OutOfMemory(String),

    /// The model file appears to be corrupt or has an unsupported format.
    #[error("corrupt or incompatible model file `{path}`: {reason}")]
    CorruptModel { path: String, reason: String },

    /// The input text could not be tokenised.
    #[error("tokenisation failed: {0}")]
    Tokenise(String),

    /// A token ID could not be decoded to a text piece.
    #[error("token decode failed for token {token}: {reason}")]
    TokenDecode { token: i32, reason: String },

    /// The model forward pass (eval step) returned an error code.
    #[error("model eval failed with code {0}")]
    Eval(i32),

    /// Token sampling returned an error code.
    #[error("token sampling failed with code {0}")]
    Sample(i32),

    /// The streaming channel was closed before generation completed.
    #[error("streaming channel closed before generation completed")]
    ChannelClosed,

    /// An I/O error occurred (e.g. reading a model registry file).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Map a [`pares_agens_bitnet::InferenceError`] to our richer error type,
/// giving callers a uniform surface without coupling them to the bitnet crate.
#[cfg(feature = "native")]
pub(crate) fn from_bitnet(e: pares_agens_bitnet::InferenceError) -> InferenceError {
    use pares_agens_bitnet::InferenceError as B;
    match e {
        B::NativeUnavailable => InferenceError::NativeUnavailable,
        B::ModelLoad { path, reason } => InferenceError::ModelLoad { path, reason },
        B::ContextCreate(msg) => {
            // ContextCreate is the primary indicator of OOM at context
            // allocation time; surface it directly for clarity.
            if msg.to_ascii_lowercase().contains("alloc")
                || msg.to_ascii_lowercase().contains("memory")
            {
                InferenceError::OutOfMemory(msg)
            } else {
                InferenceError::ContextCreate(msg)
            }
        }
        B::Tokenise(msg) => InferenceError::Tokenise(msg),
        B::TokenDecode { token, reason } => InferenceError::TokenDecode { token, reason },
        B::Eval(code) => InferenceError::Eval(code),
        B::Sample(code) => InferenceError::Sample(code),
        B::InvalidPath(nul) => {
            InferenceError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, nul))
        }
    }
}
