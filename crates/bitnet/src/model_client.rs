//! [`ModelClient`] implementation backed by local BitNet inference.
//!
//! This module is gated behind the `model-client` feature flag.  When the
//! `inference` feature is also enabled, completions run through the native
//! bitnet.cpp FFI.  Without `inference`, all calls return an error indicating
//! that native inference is unavailable.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use pares_agens_core::model::{
    ChatMessage, ChatOptions, ModelClient, ModelCompletion, ToolDefinition,
};
use tokio::sync::Mutex;

use crate::runner::{BitNetRunner, GenParams};
use crate::InferenceError;

/// A [`ModelClient`] that runs inference locally via bitnet.cpp.
///
/// The runner is loaded lazily on first use and cached behind a mutex.
/// Each completion creates a fresh context from the shared runner.
pub struct BitnetModelClient {
    /// Path to the BitNet model file (e.g. `model.gguf`).
    model_path: PathBuf,
    /// Lazily-loaded runner, shared across requests.
    runner: Mutex<Option<Arc<BitNetRunner>>>,
    /// Generation parameters applied to every request.
    gen_params: GenParams,
}

impl BitnetModelClient {
    /// Create a new client that will load the model from `path`.
    ///
    /// The model is not loaded until the first completion request.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            model_path: path.as_ref().to_owned(),
            runner: Mutex::new(None),
            gen_params: GenParams::default(),
        }
    }

    /// Create a new client with custom generation parameters.
    pub fn with_params(path: impl AsRef<Path>, params: GenParams) -> Self {
        Self {
            model_path: path.as_ref().to_owned(),
            runner: Mutex::new(None),
            gen_params: params,
        }
    }

    /// Ensure the runner is loaded, returning a shared reference.
    async fn ensure_runner(&self) -> Result<Arc<BitNetRunner>, String> {
        let mut guard = self.runner.lock().await;
        if let Some(ref runner) = *guard {
            return Ok(Arc::clone(runner));
        }
        let path = self.model_path.clone();
        let runner = tokio::task::spawn_blocking(move || BitNetRunner::load(&path))
            .await
            .map_err(|e| format!("bitnet load task panicked: {e}"))?
            .map_err(|e| format!("bitnet model load failed: {e}"))?;
        let runner = Arc::new(runner);
        *guard = Some(Arc::clone(&runner));
        Ok(runner)
    }
}

impl std::fmt::Debug for BitnetModelClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BitnetModelClient")
            .field("model_path", &self.model_path)
            .finish()
    }
}

#[async_trait]
impl ModelClient for BitnetModelClient {
    /// Run a completion using local BitNet inference.
    ///
    /// Tool calling is not supported — the model will only produce text content.
    /// All messages are concatenated into a single prompt with role prefixes.
    async fn complete(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolDefinition],
        options: &ChatOptions,
    ) -> Result<ModelCompletion, String> {
        let runner = self.ensure_runner().await?;

        // Build a simple prompt from messages.
        let prompt = messages
            .iter()
            .map(|m| format!("<|{}|>\n{}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");

        let mut params = self.gen_params.clone();
        if let Some(temp) = options.temperature {
            params.temperature = temp as f32;
        }

        tracing::debug!(
            model_path = %self.model_path.display(),
            prompt_len = prompt.len(),
            max_tokens = params.max_tokens,
            "bitnet: starting local inference"
        );

        // Run inference in a blocking task since bitnet.cpp is synchronous.
        let result = tokio::task::spawn_blocking(move || -> Result<String, InferenceError> {
            let mut ctx = runner.create_context()?;
            let tokens = ctx.tokenize(&prompt)?;
            let mut output = String::new();
            for tok_result in ctx.generate(&tokens, &params)? {
                let tok = tok_result?;
                let piece = ctx.decode_token(tok)?;
                output.push_str(&piece);
            }
            Ok(output)
        })
        .await
        .map_err(|e| format!("bitnet inference task panicked: {e}"))?
        .map_err(|e| format!("bitnet inference failed: {e}"))?;

        Ok(ModelCompletion {
            content: Some(result),
            tool_calls: vec![],
            logprobs: None,
        })
    }
}
