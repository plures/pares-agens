//! [`ClassifierBackend`] implementation backed by local BitNet inference.
//!
//! This module is gated behind the `classifier` feature flag.

use std::sync::Arc;

use pares_radix_core::classifier::ClassifierBackend;
use tokio::sync::Mutex;

use crate::runner::{BitNetRunner, GenParams};

/// A [`ClassifierBackend`] that runs classification through local BitNet
/// inference.
///
/// The runner is shared (via `Arc`) with any [`BitnetModelClient`] using the
/// same model.
pub struct BitNetClassifierBackend {
    runner: Arc<BitNetRunner>,
    gen_params: GenParams,
}

impl BitNetClassifierBackend {
    /// Create a new classifier backend from an existing runner.
    pub fn new(runner: Arc<BitNetRunner>) -> Self {
        Self {
            runner,
            gen_params: GenParams {
                max_tokens: 200,
                ..GenParams::default()
            },
        }
    }

    /// Create with custom generation parameters.
    pub fn with_params(runner: Arc<BitNetRunner>, gen_params: GenParams) -> Self {
        Self { runner, gen_params }
    }
}

impl ClassifierBackend for BitNetClassifierBackend {
    fn classify(&self, system_prompt: &str, user_message: &str) -> Result<String, String> {
        let prompt = format!("{system_prompt}\n\nUser message: {user_message}\n\nJSON:");
        let mut ctx = self.runner.create_context().map_err(|e| e.to_string())?;
        let tokens = ctx.tokenize(&prompt).map_err(|e| e.to_string())?;

        let mut output = String::new();
        for token in ctx
            .generate(&tokens, &self.gen_params)
            .map_err(|e| e.to_string())?
        {
            let tok = token.map_err(|e| e.to_string())?;
            let piece = ctx.decode_token(tok).map_err(|e| e.to_string())?;
            output.push_str(&piece);
            // Stop at end of JSON object to avoid generating extra tokens
            if output.contains('}') {
                break;
            }
        }

        Ok(output)
    }
}
