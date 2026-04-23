//! # pares-agens-inference
//!
//! Streaming local inference and [`ModelClient`] trait for Pares Agens.
//!
//! This crate wraps [`pares_agens_bitnet`] and exposes:
//!
//! | Type | Description |
//! |------|-------------|
//! | [`ModelClient`] | Async trait for streaming token generation (local **and** remote). |
//! | [`BitNetLocalRunner`] | CPU BitNet implementation of [`ModelClient`]. |
//! | [`GenParams`] | Sampling hyper-parameters including stop sequences. |
//! | [`InferenceConfig`] | Configuration for the inference engine. |
//! | [`LocalModelsConfig`] | `[models.local]` config section (cache dir, default model). |
//! | [`ModelRegistry`] | Registry of locally available models. |
//! | [`ModelDownloader`] | Manages model file caching on disk. |
//! | [`CpuExpertPool`] | Multi-model CPU expert pool with shared KV cache and RAM-aware scheduling. |
//! | [`DistributedInferenceRouter`] | Routes prompts to the best `(node, expert)` target across a cluster. |
//! | [`InferenceError`] | Unified error type for all inference failures. |
//! | [`ModelCommand`] | CLI sub-commands for `pares-agens model`. |
//!
//! # Feature flags
//!
//! | Feature       | Description |
//! |---------------|-------------|
//! | `native`      | Enable native bitnet.cpp FFI linkage (requires the `third_party/bitnet` submodule and CMake ≥ 3.21). |
//! | `hf-download` | Enable downloading models from Hugging Face Hub over HTTPS (adds reqwest and serde_json). |
//!
//! Without the `native` feature all public entry-points that require a live
//! model return [`InferenceError::NativeUnavailable`].  This lets CI run
//! `cargo check` and `cargo test` without the native toolchain.
//!
//! # Quick start (with `native` feature)
//!
//! ```rust,no_run
//! use pares_agens_inference::{BitNetLocalRunner, GenParams, ModelClient};
//! use std::path::Path;
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), pares_agens_inference::InferenceError> {
//! // Load the model (returns NativeUnavailable without `native` feature).
//! let runner = BitNetLocalRunner::load(Path::new("model.bin"), "bitnet-b1.58-3b")?;
//!
//! // Build generation params with a stop sequence.
//! let params = GenParams {
//!     max_tokens: 128,
//!     stop_sequences: vec!["</s>".to_string()],
//!     ..GenParams::default()
//! };
//!
//! // Stream tokens — each `piece` is a decoded text fragment.
//! let mut rx = runner.stream("Hello, BitNet!", params).await?;
//! while let Some(piece) = rx.recv().await {
//!     print!("{}", piece?);
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod cli;
pub mod client;
pub mod config;
pub mod distributed;
pub mod downloader;
pub mod error;
pub mod expert_pool;
pub mod params;
pub mod registry;
pub mod runner;

pub use cli::{run_cli, ModelCommand};
pub use client::{ModelClient, TokenReceiver, TokenSender};
pub use config::{InferenceConfig, LocalModelsConfig};
pub use distributed::{DistributedInferenceRouter, NodeExpertRoute, NodeInferenceCapability};
pub use downloader::ModelDownloader;
pub use error::InferenceError;
pub use expert_pool::{CpuExpert, CpuExpertPool, CpuExpertPoolConfig, SharedKvCacheManager};
pub use params::GenParams;
pub use registry::{default_cache_dir, ModelEntry, ModelRegistry};
pub use runner::BitNetLocalRunner;
