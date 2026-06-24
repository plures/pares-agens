use thiserror::Error;

/// All errors that can surface from the BitNet inference layer.
#[derive(Debug, Error)]
pub enum InferenceError {
    /// The `inference` Cargo feature was not enabled at compile time.
    ///
    /// Enable it with `cargo build --features inference` (requires the
    /// `third_party/bitnet` submodule and CMake ≥ 3.21).
    #[error(
        "native BitNet inference is unavailable: recompile with the `inference` feature enabled"
    )]
    NativeUnavailable,

    /// The model file could not be loaded (file not found, wrong format, etc.).
    #[error("failed to load model from `{path}`: {reason}")]
    ModelLoad {
        /// Path to the model file that failed to load.
        path: String,
        /// Human-readable description of the load failure.
        reason: String,
    },

    /// An inference context could not be created (out of memory, etc.).
    #[error("failed to create inference context: {0}")]
    ContextCreate(String),

    /// The input text could not be tokenised.
    #[error("tokenisation failed: {0}")]
    Tokenise(String),

    /// A token ID could not be decoded to a text piece.
    #[error("token decode failed for token {token}: {reason}")]
    TokenDecode {
        /// The numeric token ID that could not be decoded.
        token: i32,
        /// Human-readable description of the decode failure.
        reason: String,
    },

    /// The model forward pass (eval step) returned an error code.
    #[error("model eval failed with code {0}")]
    Eval(i32),

    /// Token sampling returned an error code.
    #[error("token sampling failed with code {0}")]
    Sample(i32),

    /// The provided path contains characters that cannot be represented as a
    /// C string (interior null byte).
    #[error("model path contains an interior null byte: {0}")]
    InvalidPath(#[from] std::ffi::NulError),
}
