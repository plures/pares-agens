//! [`ModelClient`] async trait and streaming channel type aliases.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::{error::InferenceError, params::GenParams};

/// A receiver end of a streaming token channel.
///
/// Each message is either a decoded text piece (`Ok(String)`) or an error
/// that terminated generation (`Err(InferenceError)`).
pub type TokenReceiver = mpsc::Receiver<Result<String, InferenceError>>;

/// A sender end of a streaming token channel.
///
/// Passed to [`ModelClient::generate`] so implementors can push decoded text
/// pieces as they are produced.
pub type TokenSender = mpsc::Sender<Result<String, InferenceError>>;

/// Standard interface for local and remote model clients.
///
/// Implementors drive token generation and push decoded text pieces through a
/// [`TokenSender`].  Callers receive tokens as they arrive via the paired
/// [`TokenReceiver`] returned by [`ModelClient::stream`].
///
/// # Streaming contract
///
/// - `generate` **must** send at least one message before returning `Ok(())`,
///   unless the prompt already hit a stop sequence.
/// - On unrecoverable errors, implementations should send the error through
///   `tx` and return `Ok(())`, not propagate the error as `Err`.  The
///   top-level `Err` return is reserved for failures that prevent the channel
///   from being set up at all (e.g. the model is not loaded).
/// - `generate` should **not** hold `tx` after returning — dropping it signals
///   end-of-stream to the caller.
///
/// # Non-streaming helper
///
/// [`ModelClient::complete`] is provided as a convenience wrapper that
/// accumulates all token pieces into a single `String`.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// A short, stable identifier for this model (e.g. `"bitnet-b1.58-3b"`).
    fn model_id(&self) -> &str;

    /// Stream generated text pieces to `tx`.
    ///
    /// The caller creates a `(tx, rx)` pair with [`tokio::sync::mpsc::channel`]
    /// (or via [`ModelClient::stream`]) and awaits on `rx` to collect tokens.
    ///
    /// # Errors
    ///
    /// Returns `Err` only when the client cannot begin generation at all
    /// (e.g. the model is not loaded).  Mid-generation failures are sent
    /// through `tx`.
    async fn generate(
        &self,
        prompt: &str,
        params: GenParams,
        tx: TokenSender,
    ) -> Result<(), InferenceError>;

    /// Open a channel and start streaming.
    ///
    /// Calls [`ModelClient::generate`] (which returns immediately after
    /// spawning the generation work) and returns the [`TokenReceiver`].
    /// The buffer size defaults to 32 messages.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `generate` cannot begin at all (e.g. the native
    /// feature is not enabled).  Mid-generation errors arrive through the
    /// returned receiver.
    async fn stream(&self, prompt: &str, params: GenParams) -> Result<TokenReceiver, InferenceError>
    where
        Self: 'static,
    {
        let (tx, rx) = mpsc::channel(32);
        self.generate(prompt, params, tx).await?;
        Ok(rx)
    }

    /// Generate a complete response (non-streaming).
    ///
    /// Accumulates all token pieces from [`ModelClient::stream`] into a single
    /// `String`.
    ///
    /// # Errors
    ///
    /// Returns `Err` on any error produced during generation.
    async fn complete(&self, prompt: &str, params: GenParams) -> Result<String, InferenceError>
    where
        Self: 'static,
    {
        let (tx, mut rx) = mpsc::channel(32);
        self.generate(prompt, params, tx).await?;

        let mut output = String::new();
        while let Some(piece) = rx.recv().await {
            output.push_str(&piece?);
        }
        Ok(output)
    }
}
