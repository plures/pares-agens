//! FFI declarations for the bitnet.cpp C inference API.
//!
//! These bindings are only compiled when the `native` feature is enabled.
//! The corresponding C++ symbols are provided by the static library that
//! `build.rs` compiles from the `bitnet.cpp/` git submodule.
//!
//! # Safety
//!
//! All functions in this module are `unsafe` because they cross the
//! Rust/C++ boundary.  Safe wrappers live in [`crate::runner`].

#![cfg(feature = "native")]

use std::ffi::{c_char, c_float, c_int, c_void};

/// Opaque handle returned by [`bitnet_init`].
///
/// The struct is never dereferenced on the Rust side; it is only passed back
/// to other `bitnet_*` functions as a raw pointer.
#[repr(C)]
pub struct BitnetContext {
    _opaque: [u8; 0],
}

/// Callback invoked by [`bitnet_generate`] for each generated token.
///
/// - `token`     — null-terminated UTF-8 string for the current token piece.
/// - `user_data` — the pointer originally supplied to [`bitnet_generate`];
///                 callers use it to pass a Rust closure or channel sender.
pub type TokenCallback =
    unsafe extern "C" fn(token: *const c_char, user_data: *mut c_void);

extern "C" {
    /// Initialise a bitnet inference context from a GGUF model file.
    ///
    /// Returns a non-null opaque pointer on success or a null pointer on
    /// failure (e.g. file not found, incompatible format).
    ///
    /// Must be paired with a call to [`bitnet_free`].
    pub fn bitnet_init(
        model_path: *const c_char,
        n_ctx: c_int,
        n_threads: c_int,
    ) -> *mut BitnetContext;

    /// Release all resources owned by `ctx`.
    ///
    /// After this call `ctx` must not be used.
    pub fn bitnet_free(ctx: *mut BitnetContext);

    /// Copy the model name into `buf` (at most `buf_len - 1` bytes, null-
    /// terminated).
    ///
    /// Returns 0 on success, non-zero on failure.
    pub fn bitnet_model_name(
        ctx: *const BitnetContext,
        buf: *mut c_char,
        buf_len: c_int,
    ) -> c_int;

    /// Return the total number of parameters in the loaded model.
    pub fn bitnet_n_params(ctx: *const BitnetContext) -> u64;

    /// Run autoregressive token generation.
    ///
    /// `callback` is invoked once for each generated token piece.
    /// Generation stops when `max_new_tokens` tokens have been produced, a
    /// stop sequence is encountered, or the context window is exhausted.
    ///
    /// Returns the number of tokens generated (>= 0) or a negative error
    /// code.
    pub fn bitnet_generate(
        ctx: *mut BitnetContext,
        prompt: *const c_char,
        max_new_tokens: c_int,
        temperature: c_float,
        top_p: c_float,
        callback: Option<TokenCallback>,
        user_data: *mut c_void,
    ) -> c_int;
}
