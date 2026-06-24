//! `pares-models` — OpenAI-compatible model router for Pares Radix.
//!
//! ## Quick start
//!
//! ```no_run
//! use pares_models::{
//!     config::{ProviderConfig, RouterConfig},
//!     router::ModelRouter,
//!     types::{ChatCompletionRequest, ChatMessage, Role},
//! };
//!
//! # async fn example() -> Result<(), pares_models::error::Error> {
//! let config = RouterConfig::single(
//!     "docker-model-runner",
//!     ProviderConfig::new("http://localhost:12434", None),
//! );
//! let router = ModelRouter::new(config);
//!
//! let request = ChatCompletionRequest::new(
//!     "ai/mistral-nemo",
//!     vec![ChatMessage::text(Role::User, "What is 2 + 2?")],
//! );
//!
//! let response = router.chat(&request).await?;
//! println!("{}", response.choices[0].message.content.as_deref().unwrap_or(""));
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod client;
pub mod config;
pub mod error;
pub mod router;
pub mod streaming;
pub mod types;

pub use client::OpenAiClient;
pub use error::Error;
pub use router::ModelRouter;
