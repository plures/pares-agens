//! Headroom bridge — re-export of the canonical implementation in `pares-agens-core`.
//!
//! Per ADR-0010, the `HeadroomHook` (compresses a `ChatMessage` list before a
//! model call) and its helpers (`count_text_tokens`, `count_message_tokens`)
//! live in exactly ONE place: [`pares_agens_core::headroom_bridge`]. This crate
//! previously carried a STALE verbatim copy which duplicated those fns across
//! the `core` and `agens-plugin` crates (ADR-0010 violation) and had drifted
//! behind core (older `[×3]` log-dedup marker, no `json` SmartCrusher branch).
//!
//! Both crates bind the bridge to the identical external types
//! (`pares_radix_core::model::ChatMessage`, `pares_radix_core::state::StateStore`,
//! same `pluresdb`/`pluresdb-px` revs), so this delegation is behavior-preserving
//! and adopts core's canonical (newer) compression behavior.
//!
//! The public path `agens_plugin::headroom::bridge::{HeadroomHook, ...}` is
//! preserved by this re-export.

pub use pares_agens_core::headroom_bridge::{
    count_message_tokens, count_text_tokens, HeadroomHook, DEFAULT_MIN_TOKENS,
};
