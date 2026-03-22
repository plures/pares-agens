//! [`BitNetRunner`] — safe Rust wrapper around the bitnet.cpp inference
//! engine.
//!
//! When the `native` feature is **enabled** this module wraps the real C++ FFI.
//! When it is **disabled** every call returns [`InferenceError::NativeUnavailable`]
//! so that the rest of the codebase can still depend on this crate without
//! requiring a native build.

use std::path::PathBuf;

use async_trait::async_trait;
use tracing::warn;
#[cfg(feature = "native")]
use tracing::{debug, info};

use crate::{
    client::ModelClient,
    config::InferenceConfig,
    error::InferenceError,
    types::{GenParams, ModelInfo, TokenStream},
};

// ── BitNetRunner ──────────────────────────────────────────────────────────────

/// A loaded BitNet model ready for inference.
///
/// Construct via [`BitNetRunner::load`]; drop to release the model from memory.
///
/// # Thread safety
///
/// `BitNetRunner` is `Send + Sync`.  The inner FFI context is wrapped in a
/// `Mutex` so that concurrent `generate` calls are serialised at the Rust
/// boundary (the C++ engine is not re-entrant).
pub struct BitNetRunner {
    /// Path to the GGUF model file on disk.
    pub model_path: PathBuf,
    /// Inference configuration (thread count, context size, …).
    pub config: InferenceConfig,
    /// Cached model metadata populated when the model is loaded.
    info: ModelInfo,
    /// Native context handle (only present when `native` feature is on).
    #[cfg(feature = "native")]
    ctx: std::sync::Mutex<NativeCtx>,
}

// ── Native context wrapper ────────────────────────────────────────────────────

#[cfg(feature = "native")]
struct NativeCtx(*mut crate::ffi::BitnetContext);

// SAFETY: bitnet_generate is serialised by the Mutex on BitNetRunner.
#[cfg(feature = "native")]
unsafe impl Send for NativeCtx {}
#[cfg(feature = "native")]
unsafe impl Sync for NativeCtx {}

#[cfg(feature = "native")]
impl Drop for NativeCtx {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: pointer was returned by bitnet_init and not yet freed.
            unsafe { crate::ffi::bitnet_free(self.0) };
        }
    }
}

// ── Constructor ───────────────────────────────────────────────────────────────

impl BitNetRunner {
    /// Load a model from `model_path` using `config`.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::ModelNotFound`] if the file does not exist.
    /// - [`InferenceError::NativeUnavailable`] when compiled without the
    ///   `native` feature.
    /// - [`InferenceError::BackendError`] if the bitnet.cpp engine rejects the
    ///   model.
    pub fn load(model_path: PathBuf, config: InferenceConfig) -> Result<Self, InferenceError> {
        if !model_path.exists() {
            return Err(InferenceError::ModelNotFound(
                model_path.display().to_string(),
            ));
        }

        #[cfg(not(feature = "native"))]
        {
            warn!("native feature disabled — BitNetRunner::load returns stub");
            // Build a minimal info struct from the path so callers can at least
            // inspect the model metadata.
            let stem = model_path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into());
            let size_bytes = std::fs::metadata(&model_path)
                .map(|m| m.len())
                .unwrap_or(0);

            Ok(Self {
                info: ModelInfo {
                    id: stem.clone(),
                    name: stem,
                    param_count: 0,
                    size_bytes,
                    quantization: "unknown".into(),
                    context_length: config.context_size,
                },
                model_path,
                config,
            })
        }

        #[cfg(feature = "native")]
        {
            use std::ffi::CString;

            let path_cstr = CString::new(model_path.to_string_lossy().as_ref())
                .map_err(|e| InferenceError::InvalidModel(e.to_string()))?;

            let ctx_ptr = unsafe {
                crate::ffi::bitnet_init(
                    path_cstr.as_ptr(),
                    config.context_size as std::ffi::c_int,
                    config.thread_count as std::ffi::c_int,
                )
            };

            if ctx_ptr.is_null() {
                return Err(InferenceError::BackendError {
                    code: -1,
                    message: "bitnet_init returned null".into(),
                });
            }

            let name = read_model_name(ctx_ptr)?;
            let param_count =
                unsafe { crate::ffi::bitnet_n_params(ctx_ptr) };
            let size_bytes = std::fs::metadata(&model_path)
                .map(|m| m.len())
                .unwrap_or(0);

            info!(%name, param_count, "loaded BitNet model");

            Ok(Self {
                info: ModelInfo {
                    id: name.clone(),
                    name,
                    param_count,
                    size_bytes,
                    quantization: "1.58-bit ternary".into(),
                    context_length: config.context_size,
                },
                model_path,
                config,
                ctx: std::sync::Mutex::new(NativeCtx(ctx_ptr)),
            })
        }
    }
}

// ── Native helpers ────────────────────────────────────────────────────────────

#[cfg(feature = "native")]
fn read_model_name(
    ctx: *mut crate::ffi::BitnetContext,
) -> Result<String, InferenceError> {
    use std::ffi::c_int;

    const BUF_LEN: usize = 256;
    let mut buf = vec![0u8; BUF_LEN];
    let rc = unsafe {
        crate::ffi::bitnet_model_name(
            ctx,
            buf.as_mut_ptr() as *mut std::ffi::c_char,
            BUF_LEN as c_int,
        )
    };
    if rc != 0 {
        return Err(InferenceError::BackendError {
            code: rc,
            message: "bitnet_model_name failed".into(),
        });
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(BUF_LEN);
    String::from_utf8(buf[..end].to_vec())
        .map_err(|e| InferenceError::InvalidModel(e.to_string()))
}

// ── ModelClient impl ──────────────────────────────────────────────────────────

#[async_trait]
impl ModelClient for BitNetRunner {
    async fn generate(
        &self,
        prompt: &str,
        params: &GenParams,
    ) -> Result<TokenStream, InferenceError> {
        params.validate()?;

        #[cfg(not(feature = "native"))]
        {
            let _ = prompt;
            return Err(InferenceError::NativeUnavailable);
        }

        #[cfg(feature = "native")]
        {
            use std::ffi::{c_int, c_void, CString};

            let prompt_cstr = CString::new(prompt)
                .map_err(|e| InferenceError::InvalidParam(e.to_string()))?;
            let (stream, tx) = TokenStream::channel(64);

            // The callback must have a 'static lifetime to be passed as a
            // function pointer, so we heap-allocate the sender and pass it via
            // user_data.
            let tx_ptr = Box::into_raw(Box::new(tx));

            let max_tokens = params.max_tokens as c_int;
            let temperature = params.temperature;
            let top_p = params.top_p;

            // Lock the context for the entire duration of bitnet_generate to
            // serialise concurrent generate calls.  `ctx_guard` stays in scope
            // until after the unsafe block completes, keeping the mutex locked.
            let ctx_mutex = &self.ctx;
            let ctx_guard = ctx_mutex.lock().map_err(|_| InferenceError::BackendError {
                code: -2,
                message: "mutex poisoned".into(),
            })?;
            let ctx_ptr = ctx_guard.0;

            debug!(max_tokens, temperature, top_p, "starting bitnet generation");

            let rc = unsafe {
                crate::ffi::bitnet_generate(
                    ctx_ptr,
                    prompt_cstr.as_ptr(),
                    max_tokens,
                    temperature,
                    top_p,
                    Some(native_token_callback),
                    tx_ptr as *mut c_void,
                )
            };

            // `ctx_guard` is explicitly dropped here, releasing the mutex only
            // after the C++ call returns.
            drop(ctx_guard);

            // Reclaim the sender so its destructor runs and the channel closes.
            let _ = unsafe { Box::from_raw(tx_ptr) };

            if rc < 0 {
                return Err(InferenceError::BackendError {
                    code: rc,
                    message: format!("bitnet_generate returned {rc}"),
                });
            }

            debug!(tokens_generated = rc, "bitnet generation complete");
            Ok(stream)
        }
    }

    fn model_info(&self) -> ModelInfo {
        self.info.clone()
    }
}

// ── Native token callback ─────────────────────────────────────────────────────

#[cfg(feature = "native")]
unsafe extern "C" fn native_token_callback(
    token: *const std::ffi::c_char,
    user_data: *mut std::ffi::c_void,
) {
    use tokio::sync::mpsc::Sender;

    if token.is_null() || user_data.is_null() {
        return;
    }

    // SAFETY: user_data was set to Box::into_raw(Box::new(tx)) in generate().
    let tx = &*(user_data as *const Sender<Result<String, InferenceError>>);

    let token_str = match std::ffi::CStr::from_ptr(token).to_str() {
        Ok(s) => s.to_owned(),
        Err(_) => return,
    };

    // Ignore send errors — the receiver may have been dropped by the caller.
    let _ = tx.blocking_send(Ok(token_str));
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_nonexistent_model_errors() {
        let result =
            BitNetRunner::load(PathBuf::from("/nonexistent/model.gguf"), InferenceConfig::default());
        assert!(matches!(result, Err(InferenceError::ModelNotFound(_))));
    }

    #[cfg(not(feature = "native"))]
    #[test]
    fn load_existing_file_succeeds_in_stub_mode() {
        let mut tmp = std::env::temp_dir();
        tmp.push("pares_agens_stub_model.gguf");
        let mut f = std::fs::File::create(&tmp).unwrap();
        f.write_all(b"dummy").unwrap();

        let runner = BitNetRunner::load(tmp.clone(), InferenceConfig::default())
            .expect("stub load should succeed");
        assert_eq!(runner.model_path, tmp);
        std::fs::remove_file(tmp).ok();
    }

    #[cfg(not(feature = "native"))]
    #[tokio::test]
    async fn generate_returns_native_unavailable_in_stub_mode() {
        let mut tmp = std::env::temp_dir();
        tmp.push("pares_agens_stub_gen.gguf");
        std::fs::write(&tmp, b"dummy").unwrap();

        let runner = BitNetRunner::load(tmp.clone(), InferenceConfig::default()).unwrap();
        let result = runner.generate("hello", &GenParams::default()).await;
        assert!(matches!(result, Err(InferenceError::NativeUnavailable)));
        std::fs::remove_file(tmp).ok();
    }
}
