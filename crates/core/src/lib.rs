//! `pares-agens-core` — PluresLM native memory integration and procedure executor.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use pares_agens_core::memory::{
//!     embed::MockEmbedder,
//!     entry::{Exchange, MemoryCategory},
//!     store::InMemoryStore,
//!     PluresLm,
//! };
//!
//! # #[tokio::main]
//! # async fn main() {
//! let lm = PluresLm::new(
//!     Box::new(InMemoryStore::new()),
//!     Box::new(MockEmbedder),
//!     128_000,
//! );
//!
//! let exchange = Exchange {
//!     user: "How do I use async/await in Rust?".into(),
//!     assistant: "Use `async fn` and `.await` on futures. Add tokio to Cargo.toml.".into(),
//! };
//! let ids = lm.capture(&exchange).await.unwrap();
//!
//! let memories = lm.recall("async rust futures", 5, &[]).await.unwrap();
//! let ctx = lm.inject_context(&memories, None);
//! # }
//! ```

pub mod memory;
