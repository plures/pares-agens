//! Local BitNet runner implementing the [`ModelClient`] trait.
//!
//! [`BitNetLocalRunner`] wraps [`pares_agens_bitnet::BitNetRunner`] and drives
//! the CPU-bound generation loop inside a `tokio::task::spawn_blocking` task,
//! streaming decoded text pieces through a tokio MPSC channel.
//!
//! Without the `native` Cargo feature every public entry-point returns
//! [`InferenceError::NativeUnavailable`] at runtime, keeping CI fast when the
//! `third_party/bitnet` submodule is absent.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

#[cfg(feature = "native")]
use crate::error::from_bitnet;
use crate::{
    client::{ModelClient, TokenSender},
    error::InferenceError,
    params::GenParams,
};

// ── RunnerInner ───────────────────────────────────────────────────────────────

/// Heap-allocated model state shared via `Arc` so the runner can be used from
/// async contexts while the CPU-bound blocking task holds a clone.
///
/// Only compiled when the `native` feature is enabled because
/// `pares_agens_bitnet::BitNetRunner` only implements `Send`/`Sync` when
/// `pares-agens-bitnet/inference` is active.
#[cfg(feature = "native")]
struct RunnerInner {
    runner: pares_agens_bitnet::BitNetRunner,
}

// SAFETY: RunnerInner wraps BitNetRunner which is Send+Sync when the
// `inference` feature is enabled.
#[cfg(feature = "native")]
unsafe impl Send for RunnerInner {}
#[cfg(feature = "native")]
unsafe impl Sync for RunnerInner {}

// ── BitNetLocalRunner ─────────────────────────────────────────────────────────

/// A local CPU-based BitNet runner that implements [`ModelClient`].
///
/// Load a model with [`BitNetLocalRunner::load`] and then use
/// [`ModelClient::stream`] or [`ModelClient::complete`] for inference.
///
/// # Feature flag
///
/// Requires the `native` Cargo feature (which transitively enables
/// `pares-agens-bitnet/inference`).  Without it every call returns
/// [`InferenceError::NativeUnavailable`].
///
/// # Thread safety
///
/// `BitNetLocalRunner` is `Send + Sync` — it can be shared across tasks via
/// `Arc`.  Each call to [`ModelClient::generate`] creates its own
/// [`pares_agens_bitnet::BitNetContext`], so concurrent generations are
/// supported as long as system memory allows.
///
/// # Example
///
/// ```rust,no_run
/// use pares_agens_inference::{BitNetLocalRunner, GenParams, ModelClient};
/// use std::path::Path;
///
/// # #[tokio::main]
/// # async fn main() -> Result<(), pares_agens_inference::InferenceError> {
/// let runner = BitNetLocalRunner::load(Path::new("model.bin"), "bitnet-b1.58-3b")?;
///
/// let params = GenParams {
///     max_tokens: 64,
///     stop_sequences: vec!["</s>".to_string()],
///     ..GenParams::default()
/// };
///
/// let mut rx = runner.stream("Hello, BitNet!", params).await?;
/// while let Some(piece) = rx.recv().await {
///     print!("{}", piece?);
/// }
/// # Ok(())
/// # }
/// ```
pub struct BitNetLocalRunner {
    model_id: String,
    model_path: PathBuf,

    #[cfg(feature = "native")]
    inner: std::sync::Arc<RunnerInner>,

    #[cfg(not(feature = "native"))]
    _marker: std::marker::PhantomData<()>,
}

impl std::fmt::Debug for BitNetLocalRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitNetLocalRunner")
            .field("model_id", &self.model_id)
            .field("model_path", &self.model_path)
            .finish_non_exhaustive()
    }
}

impl BitNetLocalRunner {
    /// Load a BitNet model from `path`, assigning it the given `model_id`.
    ///
    /// # Errors
    ///
    /// - [`InferenceError::NativeUnavailable`] — `native` feature not enabled.
    /// - [`InferenceError::ModelLoad`] — file not found or unsupported format.
    /// - [`InferenceError::CorruptModel`] — model file is corrupt.
    pub fn load(path: &Path, model_id: impl Into<String>) -> Result<Self, InferenceError> {
        let model_id = model_id.into();
        let model_path = path.to_path_buf();

        #[cfg(not(feature = "native"))]
        {
            let _ = (model_path, model_id);
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "native")]
        {
            let runner = pares_agens_bitnet::BitNetRunner::load(path).map_err(from_bitnet)?;
            Ok(Self {
                model_id,
                model_path,
                inner: std::sync::Arc::new(RunnerInner { runner }),
            })
        }
    }

    /// The filesystem path to the loaded model file.
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }
}

// ── ModelClient impl ──────────────────────────────────────────────────────────

#[async_trait]
impl ModelClient for BitNetLocalRunner {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Stream generated text pieces through `tx`.
    ///
    /// Spawns the CPU-bound inference loop in a
    /// [`tokio::task::spawn_blocking`] task.  Decoded token pieces are sent
    /// through `tx` as they are produced.  Generation stops when:
    ///
    /// - The EOS token is sampled.
    /// - `params.max_tokens` is reached.
    /// - A stop sequence appears in the accumulated output.
    /// - The receiver end of the channel is dropped ([`InferenceError::ChannelClosed`]).
    ///
    /// # Stop sequences
    ///
    /// Stop sequences are matched against the full accumulated output after
    /// each token.  Text generated before the match point (exclusive) is sent;
    /// the stop sequence itself is not included.
    ///
    /// > **Note:** A stop sequence that spans a token boundary is detected as
    /// > soon as the final character arrives.  Characters already sent as part
    /// > of the preceding token are not retracted.  For exact stop-sequence
    /// > trimming, use [`ModelClient::complete`] which accumulates the full
    /// > output before returning.
    async fn generate(
        &self,
        prompt: &str,
        params: GenParams,
        tx: TokenSender,
    ) -> Result<(), InferenceError> {
        #[cfg(not(feature = "native"))]
        {
            let _ = (prompt, params, tx);
            Err(InferenceError::NativeUnavailable)
        }

        #[cfg(feature = "native")]
        {
            let inner = std::sync::Arc::clone(&self.inner);
            let prompt = prompt.to_owned();

            // Spawn the CPU-bound loop in a blocking thread and return
            // immediately.  Any mid-generation error is sent through `tx` so
            // callers observe it when consuming the channel.
            tokio::task::spawn_blocking(move || {
                if let Err(e) = run_generation(inner, &prompt, &params, &tx) {
                    let _ = tx.blocking_send(Err(e));
                }
            });

            Ok(())
        }
    }
}

// ── run_generation ────────────────────────────────────────────────────────────

/// Inner blocking generation loop, extracted for readability.
///
/// Runs inside `tokio::task::spawn_blocking`; all I/O is synchronous.
#[cfg(feature = "native")]
fn run_generation(
    inner: std::sync::Arc<RunnerInner>,
    prompt: &str,
    params: &GenParams,
    tx: &TokenSender,
) -> Result<(), InferenceError> {
    let mut ctx = inner.runner.create_context().map_err(from_bitnet)?;

    let tokens = ctx.tokenize(prompt).map_err(from_bitnet)?;
    let bitnet_params = params.to_bitnet_params();
    let stream = ctx.generate(&tokens, &bitnet_params).map_err(from_bitnet)?;

    let mut accumulated = String::new();
    let mut sent_up_to: usize = 0;

    for token_result in stream {
        let token = token_result.map_err(from_bitnet)?;
        let piece = ctx.decode_token(token).map_err(from_bitnet)?;
        accumulated.push_str(&piece);

        // Find the earliest stop sequence in the full accumulated text.
        let stop_pos: Option<usize> = if params.stop_sequences.is_empty() {
            None
        } else {
            params
                .stop_sequences
                .iter()
                .filter_map(|s| accumulated.find(s.as_str()))
                .min()
        };

        if let Some(pos) = stop_pos {
            // Send only the portion before the stop sequence.
            if pos > sent_up_to
                && tx
                    .blocking_send(Ok(accumulated[sent_up_to..pos].to_string()))
                    .is_err()
            {
                return Err(InferenceError::ChannelClosed);
            }
            return Ok(());
        }

        if tx.blocking_send(Ok(piece)).is_err() {
            return Err(InferenceError::ChannelClosed);
        }
        sent_up_to = accumulated.len();
    }

    Ok(())
}
