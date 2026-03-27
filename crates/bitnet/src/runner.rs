//! Safe, ownership-based wrapper around the raw bitnet-sys FFI.
//!
//! The module provides three public types:
//!
//! | Type | Description |
//! |------|-------------|
//! | [`BitNetRunner`] | Owns a loaded model; the entry-point for all inference. |
//! | [`BitNetContext`] | Owns an inference context bound to a model; borrows the runner. |
//! | [`GenParams`] | Sampling hyper-parameters passed to [`BitNetContext::generate`]. |
//! | [`TokenStream`] | Lazy iterator over generated token IDs. |

use crate::InferenceError;

// ── GenParams ─────────────────────────────────────────────────────────────────

/// Hyper-parameters that control the token-sampling strategy.
#[derive(Debug, Clone)]
pub struct GenParams {
    /// Sampling temperature (1.0 = neutral; lower = sharper; higher = flatter).
    pub temperature: f32,
    /// Nucleus-sampling probability mass (0 < top_p ≤ 1.0).
    pub top_p: f32,
    /// RNG seed — use `None` for a time-based seed.
    pub seed: Option<i32>,
    /// Maximum number of new tokens to generate.
    pub max_tokens: usize,
    /// Number of CPU threads to use during generation.
    pub n_threads: usize,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            temperature: 1.0,
            top_p: 0.9,
            seed: None,
            max_tokens: 256,
            n_threads: 4,
        }
    }
}

// ── BitNetRunner ──────────────────────────────────────────────────────────────

/// Owns a loaded BitNet model.
///
/// # Lifetime
///
/// The runner must outlive every [`BitNetContext`] created from it.  The
/// lifetime parameter on [`BitNetContext`] enforces this at compile time.
///
/// # Example
///
/// ```rust,no_run
/// use pares_agens_bitnet::BitNetRunner;
/// use std::path::Path;
///
/// # fn main() -> Result<(), pares_agens_bitnet::InferenceError> {
/// let runner = BitNetRunner::load(Path::new("model.bin"))?;
/// let mut ctx = runner.create_context()?;
/// # Ok(())
/// # }
/// ```
pub struct BitNetRunner {
    /// Raw pointer to the C-side model object.
    ///
    /// - Created by `bitnet_model_load`.
    /// - Freed in `Drop`.
    /// - Non-null when `inference` feature is enabled and load succeeds.
    #[cfg(feature = "inference")]
    raw: *mut pares_agens_bitnet_sys::BitNetModelOpaque,

    /// Ensures the type is non-empty when the `inference` feature is disabled.
    #[cfg(not(feature = "inference"))]
    _marker: std::marker::PhantomData<()>,
}

impl std::fmt::Debug for BitNetRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitNetRunner").finish_non_exhaustive()
    }
}

// SAFETY: BitNetModelOpaque is an opaque C struct that bitnet.cpp guarantees
// is safe to send across threads (it is immutable after construction).
#[cfg(feature = "inference")]
unsafe impl Send for BitNetRunner {}

// SAFETY: Shared references to BitNetRunner only allow creating contexts; no
// mutation of the underlying model object occurs through `&self`.
#[cfg(feature = "inference")]
unsafe impl Sync for BitNetRunner {}

impl BitNetRunner {
    /// Load a BitNet model from `path`.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `inference` feature not enabled.
    /// - [`InferenceError::ModelLoad`] — file not found or unsupported format.
    /// - [`InferenceError::InvalidPath`] — path contains an interior null byte.
    pub fn load(path: &std::path::Path) -> Result<Self, InferenceError> {
        #[cfg(not(feature = "inference"))]
        {
            let _ = path;
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "inference")]
        {
            use std::ffi::CString;

            let path_str = path.to_str().ok_or_else(|| InferenceError::ModelLoad {
                path: path.display().to_string(),
                reason: "path is not valid UTF-8".into(),
            })?;
            let c_path = CString::new(path_str.as_bytes())?;

            // SAFETY: `c_path` is a valid null-terminated string.
            let raw = unsafe { pares_agens_bitnet_sys::bitnet_model_load(c_path.as_ptr()) };

            if raw.is_null() {
                return Err(InferenceError::ModelLoad {
                    path: path.display().to_string(),
                    reason: "bitnet_model_load returned null".into(),
                });
            }

            Ok(Self { raw })
        }
    }

    /// Create a new inference context bound to this model.
    ///
    /// The context borrows the runner for its entire lifetime, preventing the
    /// model from being dropped while the context is live.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `inference` feature not enabled.
    /// - [`InferenceError::ContextCreate`] — memory allocation failed.
    pub fn create_context(&self) -> Result<BitNetContext<'_>, InferenceError> {
        #[cfg(not(feature = "inference"))]
        {
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "inference")]
        {
            // SAFETY: `self.raw` is a valid, non-null model pointer (invariant
            // maintained by `load`).
            let raw = unsafe {
                pares_agens_bitnet_sys::bitnet_context_create(
                    self.raw as *const pares_agens_bitnet_sys::BitNetModelOpaque,
                )
            };

            if raw.is_null() {
                return Err(InferenceError::ContextCreate(
                    "bitnet_context_create returned null".into(),
                ));
            }

            Ok(BitNetContext {
                raw,
                _runner: std::marker::PhantomData,
            })
        }
    }
}

#[cfg(feature = "inference")]
impl Drop for BitNetRunner {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is non-null and was returned by `bitnet_model_load`;
        // `Drop` is called exactly once.
        unsafe { pares_agens_bitnet_sys::bitnet_model_free(self.raw) }
    }
}

// ── BitNetContext ─────────────────────────────────────────────────────────────

/// An inference context bound to a [`BitNetRunner`].
///
/// A context holds a KV-cache and the sampling state for one conversation
/// thread.  Create one per concurrent conversation with
/// [`BitNetRunner::create_context`].
///
/// # Memory safety
///
/// The lifetime parameter `'runner` ensures the context cannot outlive the
/// model it was created from, preventing use-after-free at the type level.
pub struct BitNetContext<'runner> {
    #[cfg(feature = "inference")]
    raw: *mut pares_agens_bitnet_sys::BitNetContextOpaque,

    /// Ties the context's lifetime to the runner.
    _runner: std::marker::PhantomData<&'runner BitNetRunner>,
}

// SAFETY: The C context object is bound to a specific model; it is safe to
// move across threads as long as it is not shared concurrently.
#[cfg(feature = "inference")]
unsafe impl Send for BitNetContext<'_> {}

impl<'runner> BitNetContext<'runner> {
    /// Tokenise `text` into a `Vec` of token IDs.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `inference` feature not enabled.
    /// - [`InferenceError::Tokenise`] — the tokeniser returned an error.
    pub fn tokenize(&self, text: &str) -> Result<Vec<i32>, InferenceError> {
        #[cfg(not(feature = "inference"))]
        {
            let _ = text;
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "inference")]
        {
            use std::ffi::CString;

            let c_text = CString::new(text.as_bytes())?;

            // Pre-allocate a generous token buffer.  Cap at i32::MAX to avoid
            // overflow on pathologically large inputs; in practice BitNet
            // models will hit context-length limits far below this.
            let char_count = text.len().min(i32::MAX as usize) as i32;
            let max_tokens: i32 = (char_count + 16).max(128);
            let mut buf: Vec<i32> = vec![0i32; max_tokens as usize];

            // SAFETY: `self.raw` is valid; `c_text` is null-terminated;
            // `buf` has exactly `max_tokens` elements.
            let n = unsafe {
                pares_agens_bitnet_sys::bitnet_tokenize(
                    self.raw as *const pares_agens_bitnet_sys::BitNetContextOpaque,
                    c_text.as_ptr(),
                    buf.as_mut_ptr(),
                    max_tokens,
                )
            };

            if n == pares_agens_bitnet_sys::BITNET_TOKEN_ERROR {
                return Err(InferenceError::Tokenise(
                    "bitnet_tokenize returned error code".into(),
                ));
            }

            buf.truncate(n as usize);
            Ok(buf)
        }
    }

    /// Decode a single `token` ID into its UTF-8 text piece.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `inference` feature not enabled.
    /// - [`InferenceError::TokenDecode`] — decode failed (unknown token, etc.).
    pub fn decode_token(&self, token: i32) -> Result<String, InferenceError> {
        #[cfg(not(feature = "inference"))]
        {
            let _ = token;
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "inference")]
        {
            const BUF_LEN: usize = 256;
            let mut buf = vec![0u8; BUF_LEN];

            // SAFETY: `self.raw` is valid; `buf` has `BUF_LEN` bytes.
            let n = unsafe {
                pares_agens_bitnet_sys::bitnet_token_to_piece(
                    self.raw as *const pares_agens_bitnet_sys::BitNetContextOpaque,
                    token,
                    buf.as_mut_ptr() as *mut std::os::raw::c_char,
                    BUF_LEN as i32,
                )
            };

            if n == pares_agens_bitnet_sys::BITNET_TOKEN_ERROR {
                return Err(InferenceError::TokenDecode {
                    token,
                    reason: "bitnet_token_to_piece returned error code".into(),
                });
            }

            buf.truncate(n as usize);
            String::from_utf8(buf).map_err(|e| InferenceError::TokenDecode {
                token,
                reason: e.to_string(),
            })
        }
    }

    /// Reset the KV-cache and generation state without freeing the context.
    ///
    /// Call this between conversations to avoid stale cached state bleeding
    /// into new generations.
    pub fn reset(&mut self) {
        #[cfg(feature = "inference")]
        {
            // SAFETY: `self.raw` is a valid, non-null, non-freed context pointer.
            unsafe { pares_agens_bitnet_sys::bitnet_context_reset(self.raw) }
        }
    }

    /// Generate tokens from `prompt_tokens`, returning a lazy [`TokenStream`].
    ///
    /// The stream yields successive token IDs until either the EOS token is
    /// produced or `params.max_tokens` have been generated.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `inference` feature not enabled.
    /// - [`InferenceError::Eval`] — the model eval step failed.
    pub fn generate(
        &mut self,
        prompt_tokens: &[i32],
        params: &GenParams,
    ) -> Result<TokenStream, InferenceError> {
        #[cfg(not(feature = "inference"))]
        {
            let _ = (prompt_tokens, params);
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "inference")]
        {
            // Feed the prompt through the model's forward pass.
            // SAFETY: `self.raw` is valid; `prompt_tokens` slice is valid.
            let rc = unsafe {
                pares_agens_bitnet_sys::bitnet_eval(
                    self.raw,
                    prompt_tokens.as_ptr(),
                    prompt_tokens.len() as i32,
                )
            };

            if rc != 0 {
                return Err(InferenceError::Eval(rc));
            }

            let sys_params = pares_agens_bitnet_sys::BitNetGenParams {
                temperature: params.temperature,
                top_p: params.top_p,
                seed: params.seed.unwrap_or(-1),
                n_predict: params.max_tokens as i32,
                n_threads: params.n_threads as i32,
            };

            Ok(TokenStream {
                ctx_ptr: self.raw,
                params: sys_params,
                remaining: params.max_tokens,
                _ctx: std::marker::PhantomData,
            })
        }
    }
}

#[cfg(feature = "inference")]
impl Drop for BitNetContext<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is non-null and was returned by
        // `bitnet_context_create`; `Drop` is called exactly once.
        unsafe { pares_agens_bitnet_sys::bitnet_context_free(self.raw) }
    }
}

// ── TokenStream ───────────────────────────────────────────────────────────────

/// A lazy iterator over generated token IDs produced by
/// [`BitNetContext::generate`].
///
/// Each call to [`Iterator::next`] drives one forward+sample step in the model.
/// The iterator ends when the EOS token is sampled or the token budget
/// (`params.max_tokens`) is exhausted.
pub struct TokenStream {
    #[cfg(feature = "inference")]
    ctx_ptr: *mut pares_agens_bitnet_sys::BitNetContextOpaque,
    #[cfg(feature = "inference")]
    params: pares_agens_bitnet_sys::BitNetGenParams,
    /// Remaining token budget.
    remaining: usize,
    /// Ties the stream's lifetime to the context it was created from.
    _ctx: std::marker::PhantomData<*mut ()>,
}

// SAFETY: The stream holds a raw pointer to the context, but access is
// single-threaded (no Sync impl); it is safe to move across threads.
#[cfg(feature = "inference")]
unsafe impl Send for TokenStream {}

impl Iterator for TokenStream {
    type Item = Result<i32, InferenceError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        #[cfg(not(feature = "inference"))]
        {
            None
        }

        #[cfg(feature = "inference")]
        {
            // SAFETY: `self.ctx_ptr` is a valid, live context pointer (the
            // lifetime bound on `BitNetContext` prevents the context from
            // being dropped before the stream).
            let token =
                unsafe { pares_agens_bitnet_sys::bitnet_sample(self.ctx_ptr, &self.params) };

            if token == pares_agens_bitnet_sys::BITNET_TOKEN_EOS {
                return None;
            }

            if token == pares_agens_bitnet_sys::BITNET_TOKEN_ERROR {
                return Some(Err(InferenceError::Sample(token)));
            }

            // Feed the sampled token back so the next step attends to it.
            // SAFETY: single i32 value; `self.ctx_ptr` valid.
            let rc = unsafe { pares_agens_bitnet_sys::bitnet_eval(self.ctx_ptr, &token, 1) };

            if rc != 0 {
                return Some(Err(InferenceError::Eval(rc)));
            }

            self.remaining -= 1;
            Some(Ok(token))
        }
    }
}
