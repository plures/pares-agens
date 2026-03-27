//! Raw, unsafe C FFI bindings for **bitnet.cpp**.
//!
//! This crate exposes the minimal C API surface needed by the safe
//! [`pares-agens-bitnet`] wrapper.  Do **not** use this crate directly from
//! application code; use the safe wrapper instead.
//!
//! # Feature flags
//!
//! | Feature  | Description |
//! |----------|-------------|
//! | `native` | Link against the locally-built bitnet.cpp static library.  Requires the `third_party/bitnet` submodule and CMake ≥ 3.21. |
//!
//! Without the `native` feature every symbol resolves to a stub that panics
//! at runtime.  This allows `cargo check --features native` to pass in CI
//! even when the submodule is absent.

#![warn(missing_docs)]
#![allow(non_camel_case_types, non_snake_case)]

#[cfg(feature = "native")]
use std::os::raw::c_char;
use std::os::raw::{c_float, c_int};

// ── Opaque handle types ───────────────────────────────────────────────────────

/// Opaque handle to a loaded BitNet model.
///
/// Ownership: created by [`bitnet_model_load`], freed by [`bitnet_model_free`].
/// Never construct or copy this type — treat it as a raw pointer target only.
#[repr(C)]
pub struct BitNetModelOpaque {
    _private: [u8; 0],
}

/// Opaque handle to an inference context bound to a [`BitNetModelOpaque`].
///
/// Ownership: created by [`bitnet_context_create`], freed by
/// [`bitnet_context_free`].
#[repr(C)]
pub struct BitNetContextOpaque {
    _private: [u8; 0],
}

// ── Generation parameters ─────────────────────────────────────────────────────

/// Parameters controlling the token-sampling strategy during generation.
///
/// Pass a pointer to this struct to [`bitnet_sample`].
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BitNetGenParams {
    /// Sampling temperature (1.0 = neutral, <1 = sharper, >1 = flatter).
    pub temperature: c_float,
    /// Nucleus-sampling probability mass threshold (0–1).
    pub top_p: c_float,
    /// RNG seed; pass `-1` to use a time-based seed.
    pub seed: c_int,
    /// Maximum new tokens to generate; `-1` for no limit.
    pub n_predict: c_int,
    /// Number of CPU threads to use during generation.
    pub n_threads: c_int,
}

impl Default for BitNetGenParams {
    fn default() -> Self {
        Self {
            temperature: 1.0,
            top_p: 0.9,
            seed: -1,
            n_predict: 256,
            n_threads: 4,
        }
    }
}

// ── Special token constants ───────────────────────────────────────────────────

/// Sentinel value returned by [`bitnet_sample`] when generation is complete
/// (EOS token or `n_predict` exhausted).
pub const BITNET_TOKEN_EOS: c_int = -1;

/// Sentinel value returned by various functions to signal an error.
pub const BITNET_TOKEN_ERROR: c_int = -2;

// ── FFI declarations ──────────────────────────────────────────────────────────

#[cfg(feature = "native")]
extern "C" {
    /// Load a BitNet model from `model_path`.
    ///
    /// Returns a non-null opaque pointer on success, or null on failure
    /// (e.g. file not found, unsupported format).
    ///
    /// # Safety
    /// `model_path` must be a valid, null-terminated UTF-8 string.
    pub fn bitnet_model_load(model_path: *const c_char) -> *mut BitNetModelOpaque;

    /// Free all resources associated with a model previously returned by
    /// [`bitnet_model_load`].
    ///
    /// # Safety
    /// `model` must be a non-null pointer previously returned by
    /// `bitnet_model_load` that has not already been freed.
    pub fn bitnet_model_free(model: *mut BitNetModelOpaque);

    /// Create a new inference context for `model`.
    ///
    /// Returns a non-null pointer on success, or null if allocation fails.
    ///
    /// # Safety
    /// `model` must be a valid pointer returned by [`bitnet_model_load`] that
    /// has not been freed.  The returned context must be freed with
    /// [`bitnet_context_free`] before the model is freed.
    pub fn bitnet_context_create(model: *const BitNetModelOpaque) -> *mut BitNetContextOpaque;

    /// Free an inference context.
    ///
    /// # Safety
    /// `ctx` must be a non-null pointer previously returned by
    /// `bitnet_context_create` that has not already been freed.
    pub fn bitnet_context_free(ctx: *mut BitNetContextOpaque);

    /// Reset the KV-cache and generation state of a context without freeing it.
    ///
    /// # Safety
    /// `ctx` must be a valid, non-null, non-freed context pointer.
    pub fn bitnet_context_reset(ctx: *mut BitNetContextOpaque);

    /// Tokenise `text` into token IDs.
    ///
    /// Writes at most `max_tokens` token IDs into `out_tokens`.  Returns the
    /// number of tokens written, or [`BITNET_TOKEN_ERROR`] on failure.
    ///
    /// # Safety
    /// - `ctx` must be a valid context pointer.
    /// - `text` must be a valid null-terminated string.
    /// - `out_tokens` must point to a buffer of at least `max_tokens` `i32` values.
    pub fn bitnet_tokenize(
        ctx: *const BitNetContextOpaque,
        text: *const c_char,
        out_tokens: *mut c_int,
        max_tokens: c_int,
    ) -> c_int;

    /// Decode a single `token` ID into its UTF-8 text piece.
    ///
    /// Writes at most `buf_len − 1` bytes into `buf` and null-terminates it.
    /// Returns the number of bytes written (excluding the null terminator), or
    /// [`BITNET_TOKEN_ERROR`] on failure.
    ///
    /// # Safety
    /// - `ctx` must be a valid context pointer.
    /// - `buf` must point to a writable buffer of at least `buf_len` bytes.
    pub fn bitnet_token_to_piece(
        ctx: *const BitNetContextOpaque,
        token: c_int,
        buf: *mut c_char,
        buf_len: c_int,
    ) -> c_int;

    /// Feed `tokens` through the model's forward pass, updating the KV-cache.
    ///
    /// Returns `0` on success or a negative error code.
    ///
    /// # Safety
    /// - `ctx` must be a valid context pointer.
    /// - `tokens` must point to a buffer of at least `n_tokens` `i32` values.
    pub fn bitnet_eval(
        ctx: *mut BitNetContextOpaque,
        tokens: *const c_int,
        n_tokens: c_int,
    ) -> c_int;

    /// Sample the next token from the logits stored in `ctx`.
    ///
    /// Returns the sampled token ID, [`BITNET_TOKEN_EOS`] when generation is
    /// complete, or [`BITNET_TOKEN_ERROR`] on failure.
    ///
    /// # Safety
    /// - `ctx` must be a valid context pointer.
    /// - `params` must be a valid pointer to a [`BitNetGenParams`] value.
    pub fn bitnet_sample(ctx: *mut BitNetContextOpaque, params: *const BitNetGenParams) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen_params_default_values() {
        let p = BitNetGenParams::default();
        assert!((p.temperature - 1.0).abs() < f32::EPSILON);
        assert!((p.top_p - 0.9).abs() < 1e-6);
        assert_eq!(p.seed, -1);
        assert_eq!(p.n_predict, 256);
        assert_eq!(p.n_threads, 4);
    }

    #[test]
    fn gen_params_is_copy() {
        let a = BitNetGenParams::default();
        let b = a; // Copy
        assert!((a.temperature - b.temperature).abs() < f32::EPSILON);
    }

    #[test]
    fn gen_params_debug_output() {
        let p = BitNetGenParams::default();
        let s = format!("{p:?}");
        assert!(s.contains("temperature"));
    }

    #[test]
    fn eos_and_error_constants_are_negative() {
        // Use let-bindings so clippy doesn't treat these as constant-folded
        // assertions (clippy::assertions_on_constants).
        let eos = BITNET_TOKEN_EOS;
        let err = BITNET_TOKEN_ERROR;
        assert!(eos < 0);
        assert!(err < 0);
        assert_ne!(eos, err);
    }

    #[test]
    fn opaque_types_are_zero_sized_in_repr() {
        // BitNetModelOpaque and BitNetContextOpaque should have zero-byte
        // private fields — they are only ever used behind raw pointers.
        assert_eq!(std::mem::size_of::<BitNetModelOpaque>(), 0);
        assert_eq!(std::mem::size_of::<BitNetContextOpaque>(), 0);
    }
}
