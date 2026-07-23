//! # pares-agens-bitnet
//!
//! Safe Rust wrapper around **bitnet.cpp** for local BitNet CPU inference.
//!
//! # Feature flags
//!
//! | Feature     | Description |
//! |-------------|-------------|
//! | `inference` | Enable native bitnet.cpp FFI linkage (requires the `third_party/bitnet` submodule and CMake ≥ 3.21). |
//!
//! Without the `inference` feature all public entry-points return
//! [`InferenceError::NativeUnavailable`] at runtime and the crate compiles
//! with zero native dependencies.  This keeps the CI `cargo check
//! --features inference` fast even when the submodule is absent.
//!
//! # Quick start (with `inference` feature)
//!
//! ```rust,no_run
//! use pares_agens_bitnet::{BitNetRunner, GenParams};
//! use std::path::Path;
//!
//! # fn main() -> Result<(), pares_agens_bitnet::InferenceError> {
//! let runner = BitNetRunner::load(Path::new("model.bin"))?;
//! let mut ctx = runner.create_context()?;
//!
//! let tokens = ctx.tokenize("Hello, BitNet!")?;
//! let params = GenParams { max_tokens: 64, ..GenParams::default() };
//!
//! for token in ctx.generate(&tokens, &params)? {
//!     let piece = ctx.decode_token(token?)?;
//!     print!("{piece}");
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

mod error;
pub mod runner;

#[cfg(feature = "model-client")]
pub mod model_client;

#[cfg(feature = "classifier")]
pub mod classifier_backend;

pub use error::InferenceError;
pub use runner::{BitNetContext, BitNetRunner, GenParams, TokenStream};

#[cfg(feature = "model-client")]
pub use model_client::BitnetModelClient;

#[cfg(feature = "classifier")]
pub use classifier_backend::BitNetClassifierBackend;
