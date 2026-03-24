use std::path::PathBuf;

/// Configuration for the local inference engine.
///
/// Loaded from `[inference]` section in the application config file
/// (TOML/JSON) or constructed programmatically.
///
/// # Example
///
/// ```rust
/// use pares_agens_inference::InferenceConfig;
/// use std::path::PathBuf;
///
/// let cfg = InferenceConfig {
///     model_dir: PathBuf::from("/var/lib/pares-agens/models"),
///     ..InferenceConfig::default()
/// };
/// assert_eq!(cfg.default_params.max_tokens, 256);
/// ```
#[derive(Debug, Clone)]
pub struct InferenceConfig {
    /// Directory where model files are stored.
    pub model_dir: PathBuf,

    /// Default generation parameters used when callers do not override them.
    pub default_params: crate::params::GenParams,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            model_dir: PathBuf::from("models"),
            default_params: crate::params::GenParams::default(),
        }
    }
}
