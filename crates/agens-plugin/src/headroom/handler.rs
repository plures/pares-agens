//! Headroom handler — re-export of the canonical implementation in `pares-agens-core`.
//!
//! Per ADR-0010, the `HeadroomActionHandler` (tiktoken token-counting +
//! compression `.px` ActionHandler) lives in exactly ONE place:
//! [`pares_agens_core::headroom`]. This crate previously carried a verbatim
//! (and by now STALE) copy of the whole handler here — duplicating dozens of
//! fns (`detect_content_type`, `detect_language`, `extract_rust_fn_signatures`,
//! `compute_embedding_impl`, `is_code_content`, …) across the `core` and
//! `agens-plugin` crates. That is the textbook ADR-0010 cross-crate
//! duplication the CI gate rejects.
//!
//! Both crates build the handler against the SAME external type universe
//! (`pares_radix_core` v1.55.13, `pluresdb`/`pluresdb-px` at identical revs,
//! `tiktoken-rs`, `sha2`, `unicode-segmentation`), so delegating is fully
//! behavior-preserving — and it additionally picks up core's newer features
//! (e.g. `compress_json_array` SmartCrusher + the terse `[xN]` log-dedup marker).
//!
//! The public path `agens_plugin::headroom::handler::HeadroomActionHandler`
//! (used by this crate's examples/integration tests) is preserved by this
//! re-export.

pub use pares_agens_core::headroom::{
    compress_json_array, message_token_count, score_message_importance, select_by_importance,
    HeadroomActionHandler, JsonCrushConfig, MessageMeta, MessageScore, RetentionPlan,
};
