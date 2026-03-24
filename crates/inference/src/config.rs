use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for locally cached models.
///
/// Maps to the `[models.local]` section in the application config file.
///
/// # TOML example
///
/// ```toml
/// [models.local]
/// cache_dir = "~/.pares-agens/models"
/// default_model = "BitNet-b1.58-2B-4T"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelsConfig {
    /// Directory where model files are cached.
    ///
    /// Defaults to `~/.pares-agens/models` via
    /// [`crate::registry::default_cache_dir`].
    #[serde(default = "crate::registry::default_cache_dir")]
    pub cache_dir: PathBuf,

    /// The model ID to use by default when no explicit model is specified.
    pub default_model: Option<String>,
}

impl Default for LocalModelsConfig {
    fn default() -> Self {
        Self {
            cache_dir: crate::registry::default_cache_dir(),
            default_model: None,
        }
    }
}

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

    /// Local model management configuration (`[models.local]`).
    pub local_models: LocalModelsConfig,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        let local_models = LocalModelsConfig::default();
        Self {
            model_dir: local_models.cache_dir.clone(),
            default_params: crate::params::GenParams::default(),
            local_models,
        }
    }
}
