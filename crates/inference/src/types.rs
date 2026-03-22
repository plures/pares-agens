//! Shared data types for the inference crate.

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::error::InferenceError;

// ── ModelInfo ─────────────────────────────────────────────────────────────────

/// Metadata describing a loaded inference model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Stable identifier, e.g. `"bitnet-2b"` or `"llama3-8b-bitnet"`.
    pub id: String,

    /// Human-readable display name.
    pub name: String,

    /// Total parameter count.
    pub param_count: u64,

    /// On-disk size in bytes.
    pub size_bytes: u64,

    /// Quantisation format, e.g. `"1.58-bit ternary"` or `"Q4_K_M"`.
    pub quantization: String,

    /// Maximum context length (in tokens) supported by this model.
    pub context_length: u32,
}

// ── GenParams ─────────────────────────────────────────────────────────────────

/// Parameters that control a single generation request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenParams {
    /// Maximum number of new tokens to generate.
    ///
    /// Defaults to `256`.
    #[serde(default = "GenParams::default_max_tokens")]
    pub max_tokens: u32,

    /// Sampling temperature; higher values produce more varied output.
    ///
    /// Range `[0.0, 2.0]`.  Defaults to `0.8`.
    #[serde(default = "GenParams::default_temperature")]
    pub temperature: f32,

    /// Nucleus sampling probability cutoff.
    ///
    /// Range `(0.0, 1.0]`.  Defaults to `0.95`.
    #[serde(default = "GenParams::default_top_p")]
    pub top_p: f32,

    /// Sequences that, when generated, cause generation to stop early.
    #[serde(default)]
    pub stop_sequences: Vec<String>,
}

impl GenParams {
    fn default_max_tokens() -> u32 {
        256
    }
    fn default_temperature() -> f32 {
        0.8
    }
    fn default_top_p() -> f32 {
        0.95
    }

    /// Validate that all parameter values are in their acceptable ranges.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError::InvalidParam`] if any parameter is out of range.
    pub fn validate(&self) -> Result<(), InferenceError> {
        if self.max_tokens == 0 {
            return Err(InferenceError::InvalidParam(
                "max_tokens must be at least 1".into(),
            ));
        }
        if !(0.0..=2.0).contains(&self.temperature) {
            return Err(InferenceError::InvalidParam(format!(
                "temperature must be in [0.0, 2.0], got {}",
                self.temperature
            )));
        }
        if !(0.0..=1.0).contains(&self.top_p) || self.top_p == 0.0 {
            return Err(InferenceError::InvalidParam(format!(
                "top_p must be in (0.0, 1.0], got {}",
                self.top_p
            )));
        }
        Ok(())
    }
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            max_tokens: Self::default_max_tokens(),
            temperature: Self::default_temperature(),
            top_p: Self::default_top_p(),
            stop_sequences: Vec::new(),
        }
    }
}

// ── TokenStream ───────────────────────────────────────────────────────────────

/// An async stream of generated tokens.
///
/// Each message is a `Result<String, InferenceError>`.  An error message
/// signals that generation aborted; receivers should stop consuming after the
/// first error.
///
/// # Example
///
/// ```rust,no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # use pares_agens_inference::{BitNetRunner, ModelClient, GenParams};
/// # let runner: BitNetRunner = unimplemented!();
/// let mut stream = runner.generate("Hello!", &GenParams::default()).await?;
/// while let Some(result) = stream.recv().await {
///     match result {
///         Ok(token) => print!("{}", token),
///         Err(e) => eprintln!("Error: {}", e),
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct TokenStream {
    rx: mpsc::Receiver<Result<String, InferenceError>>,
}

impl TokenStream {
    /// Create a new [`TokenStream`] backed by an mpsc channel.
    ///
    /// Returns the stream and the corresponding sender so the producer can push
    /// tokens.
    #[allow(dead_code)] // used in runner.rs under #[cfg(feature = "native")]
    pub(crate) fn channel(capacity: usize) -> (Self, mpsc::Sender<Result<String, InferenceError>>) {
        let (tx, rx) = mpsc::channel(capacity);
        (Self { rx }, tx)
    }

    /// Receive the next token from the stream, or `None` if generation has
    /// finished.
    pub async fn recv(&mut self) -> Option<Result<String, InferenceError>> {
        self.rx.recv().await
    }

    /// Collect all tokens into a single `String`, returning any error
    /// encountered during generation.
    ///
    /// # Errors
    ///
    /// Returns the first [`InferenceError`] encountered during generation.
    pub async fn collect(mut self) -> Result<String, InferenceError> {
        let mut buf = String::new();
        while let Some(result) = self.rx.recv().await {
            buf.push_str(&result?);
        }
        Ok(buf)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen_params_default_valid() {
        let p = GenParams::default();
        assert!(p.validate().is_ok());
    }

    #[test]
    fn gen_params_zero_max_tokens_invalid() {
        let p = GenParams {
            max_tokens: 0,
            ..GenParams::default()
        };
        assert!(matches!(p.validate(), Err(InferenceError::InvalidParam(_))));
    }

    #[test]
    fn gen_params_temperature_out_of_range() {
        let p = GenParams {
            temperature: 3.0,
            ..GenParams::default()
        };
        assert!(matches!(p.validate(), Err(InferenceError::InvalidParam(_))));
    }

    #[test]
    fn gen_params_top_p_zero_invalid() {
        let p = GenParams {
            top_p: 0.0,
            ..GenParams::default()
        };
        assert!(matches!(p.validate(), Err(InferenceError::InvalidParam(_))));
    }

    #[tokio::test]
    async fn token_stream_collect() {
        let (mut stream, tx) = TokenStream::channel(8);
        tx.send(Ok("Hello".into())).await.unwrap();
        tx.send(Ok(", world".into())).await.unwrap();
        drop(tx);
        assert_eq!(stream.collect().await.unwrap(), "Hello, world");
    }

    #[tokio::test]
    async fn token_stream_error_propagates() {
        let (stream, tx) = TokenStream::channel(8);
        tx.send(Ok("token".into())).await.unwrap();
        tx.send(Err(InferenceError::Cancelled)).await.unwrap();
        drop(tx);
        assert!(stream.collect().await.is_err());
    }
}
