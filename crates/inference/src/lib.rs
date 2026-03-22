//! `pares-agens-inference` — Native local model inference via BitNet.cpp FFI.
//!
//! # Overview
//!
//! This crate provides CPU-native LLM inference using Microsoft's
//! [bitnet.cpp](https://github.com/microsoft/BitNet) as the backend.
//!
//! BitNet uses 1.58-bit ternary weights, enabling:
//! - 100B-parameter models on a single CPU at 5–7 tok/s
//! - 10–20× smaller footprint than FP16 (a 2B model ≈ 0.5 GB)
//! - 70–82% lower energy consumption vs FP16
//!
//! # Architecture
//!
//! ```text
//! ModelRegistry          ModelDownloader
//!    (catalogue)            (HuggingFace → ~/.pares-agens/models/)
//!        │                         │
//!        └──────────┬──────────────┘
//!                   │
//!              BitNetRunner           ← implements ModelClient
//!                   │ (native feature)
//!              bitnet.cpp FFI
//!              (C++ static library)
//! ```
//!
//! # Features
//!
//! - **`native`** *(off by default)* — links the bitnet.cpp C++ static library
//!   and enables real inference.  Requires the `crates/inference/bitnet.cpp`
//!   git submodule.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use pares_agens_inference::{BitNetRunner, GenParams, InferenceConfig, ModelClient};
//! use std::path::PathBuf;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let config = InferenceConfig::default();
//! let runner = BitNetRunner::load(
//!     PathBuf::from("~/.pares-agens/models/bitnet-2b.gguf"),
//!     config,
//! )?;
//!
//! let mut stream = runner
//!     .generate("Explain BitNet in one sentence.", &GenParams::default())
//!     .await?;
//!
//! while let Some(token) = stream.recv().await {
//!     print!("{}", token?);
//! }
//! # Ok(())
//! # }
//! ```

pub mod cli;
pub mod client;
pub mod config;
pub mod downloader;
pub mod error;
pub mod ffi;
pub mod registry;
pub mod runner;
pub mod types;

pub use client::ModelClient;
pub use config::InferenceConfig;
pub use downloader::{DownloadProgress, ModelDownloader};
pub use error::InferenceError;
pub use registry::{KnownModel, LocalModel, ModelRegistry, KNOWN_MODELS};
pub use runner::BitNetRunner;
pub use types::{GenParams, ModelInfo, TokenStream};
