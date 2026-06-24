//! Memory entry data structures and category taxonomy.
//!
//! These persisted-data DTOs ([`MemoryCategory`], [`MemoryEntry`],
//! [`Exchange`], [`ChatTurn`]) are **defined in the platform**
//! (`pares_radix_core::memory::entry`) because they are serde schema for
//! the agent's persisted memory, owned alongside `state`, `license`, and
//! `model`. The cognition layer re-exports them here so existing
//! `crate::memory::entry::*` paths resolve unchanged, and layers its
//! behavior (embedding, recall, quality-gating, forgetting) on top.
//!
//! See [`crate::memory::PluresLm`] for the cognition behavior that operates
//! on these types.

pub use pares_radix_core::memory::entry::{ChatTurn, Exchange, MemoryCategory, MemoryEntry};
