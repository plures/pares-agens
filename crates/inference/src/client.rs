//! `ModelClient` — the common trait implemented by all inference backends.

use async_trait::async_trait;

use crate::{GenParams, InferenceError, ModelInfo, TokenStream};

/// Shared interface for all local and remote model inference backends.
///
/// Implementors must be both `Send` and `Sync` so they can be stored in shared
/// application state (e.g. inside `Arc<dyn ModelClient>`).
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Stream token-by-token generation for the given prompt.
    ///
    /// # Errors
    ///
    /// Returns [`InferenceError`] if the model is not loaded, a parameter is
    /// invalid, or the backend reports a failure.
    async fn generate(
        &self,
        prompt: &str,
        params: &GenParams,
    ) -> Result<TokenStream, InferenceError>;

    /// Return static metadata about the model backing this client.
    fn model_info(&self) -> ModelInfo;
}
