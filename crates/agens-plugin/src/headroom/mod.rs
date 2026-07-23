//! Headroom: agens-brought context-compression capability.
//!
//! This module is the preserved agens-unique IP (context compression in the
//! agent loop, agens commit `10abc2c`). It was carved out of the (now deleted)
//! agens `pares-agens-core` fork during the Stage-4 collapse and ported to the
//! radix v1.47.2 `pares-agens-core` surface.
//!
//! - [`handler`] - the `HeadroomActionHandler` `.px` ActionHandler implementing
//!   tiktoken-based token counting + compression strategies. Self-contained
//!   (depends only on `pluresdb`, `pluresdb-px`, `tiktoken_rs`, `sha2`,
//!   `unicode_segmentation`).
//! - [`bridge`] - the `HeadroomHook` that compresses a `ChatMessage` list
//!   before a model call, using a `StateStore` for observability. Depends on
//!   radix core's `model::ChatMessage` + `state::StateStore`.

pub mod bridge;
pub mod handler;

pub use bridge::{count_message_tokens, count_text_tokens, HeadroomHook};
pub use handler::HeadroomActionHandler;

use std::sync::Arc;

use pares_radix_core::state::{PluresDbStateStore, StateStore};
use pluresdb::CrdtStore;

/// Build an enabled in-memory [`HeadroomHook`] (the agens-brought compression
/// seam) with a fresh PluresDB-backed observability store and a dedicated
/// [`CrdtStore`] for the leaf-actor handler.
///
/// `min_tokens` is the aggregate token floor below which compression is
/// skipped (`0` normalizes to the bridge default).
pub fn in_memory_hook(min_tokens: usize) -> HeadroomHook {
    let store: Arc<dyn StateStore> = Arc::new(PluresDbStateStore::in_memory());
    let handler = Arc::new(HeadroomActionHandler::new(Arc::new(CrdtStore::default())));
    HeadroomHook::new(store, handler, min_tokens)
}

/// Build a disabled in-memory [`HeadroomHook`] (transparent pass-through).
pub fn in_memory_hook_disabled() -> HeadroomHook {
    let store: Arc<dyn StateStore> = Arc::new(PluresDbStateStore::in_memory());
    let handler = Arc::new(HeadroomActionHandler::new(Arc::new(CrdtStore::default())));
    HeadroomHook::disabled(store, handler)
}
