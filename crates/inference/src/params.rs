//! Token-sampling hyper-parameters.

/// Hyper-parameters that control the token-sampling strategy.
///
/// All fields have sensible defaults via [`GenParams::default`].
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

    /// Sequences that halt generation early when encountered in the output.
    ///
    /// Generation stops as soon as the accumulated text contains any of these
    /// strings.  An empty list disables stop-sequence checking.
    pub stop_sequences: Vec<String>,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            temperature: 1.0,
            top_p: 0.9,
            seed: None,
            max_tokens: 256,
            n_threads: 4,
            stop_sequences: Vec::new(),
        }
    }
}

impl GenParams {
    /// Convert to the inner [`pares_agens_bitnet::GenParams`] used by the FFI
    /// wrapper.  Stop-sequence handling is done at the inference layer, not
    /// inside bitnet itself.
    #[cfg(feature = "native")]
    pub(crate) fn to_bitnet_params(&self) -> pares_agens_bitnet::GenParams {
        pares_agens_bitnet::GenParams {
            temperature: self.temperature,
            top_p: self.top_p,
            seed: self.seed,
            max_tokens: self.max_tokens,
            n_threads: self.n_threads,
        }
    }
}
