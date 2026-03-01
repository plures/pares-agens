//! `pares-trainer` — Model fine-tuning trainer for Pares Agens.
//!
//! Provides LoRA-based fine-tuning, training data preparation, evaluation,
//! and scheduling for periodic model refresh.

pub mod skill_detection;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during trainer operations.
#[derive(Debug, Error)]
pub enum TrainerError {
    /// The supplied JSONL training file could not be read or parsed.
    #[error("invalid training data: {0}")]
    InvalidData(String),

    /// A configuration value is out of the acceptable range.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// The training run itself failed.
    #[error("training failed: {0}")]
    TrainingFailed(String),

    /// Evaluation of the adapter failed.
    #[error("evaluation failed: {0}")]
    EvaluationFailed(String),

    /// JSON (de)serialisation failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// A single supervised training example with a prompt and its target completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingExample {
    /// The input prompt presented to the model.
    pub prompt: String,

    /// The desired model completion for the given prompt.
    pub completion: String,
}

/// Configuration for a LoRA fine-tuning run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainerConfig {
    /// LoRA rank — controls the number of trainable parameters.
    /// Must be a power of two in the range \[1, 256\].
    pub lora_rank: u16,

    /// Learning rate for the optimizer.  Must be positive.
    pub learning_rate: f32,

    /// Number of examples to process per gradient-update step.  Must be ≥ 1.
    pub batch_size: u32,

    /// Maximum number of full passes over the training data.  Must be ≥ 1.
    pub max_epochs: u32,
}

impl TrainerConfig {
    /// Construct a `TrainerConfig` with sensible defaults.
    ///
    /// | field           | default |
    /// |-----------------|---------|
    /// | `lora_rank`     | 16      |
    /// | `learning_rate` | 3e-4    |
    /// | `batch_size`    | 8       |
    /// | `max_epochs`    | 3       |
    #[must_use]
    pub fn default_config() -> Self {
        Self {
            lora_rank: 16,
            learning_rate: 3e-4,
            batch_size: 8,
            max_epochs: 3,
        }
    }

    /// Validate that all fields are within acceptable ranges.
    ///
    /// # Errors
    ///
    /// Returns [`TrainerError::InvalidConfig`] if any field is invalid.
    pub fn validate(&self) -> Result<(), TrainerError> {
        if self.lora_rank == 0 || !self.lora_rank.is_power_of_two() || self.lora_rank > 256 {
            return Err(TrainerError::InvalidConfig(format!(
                "lora_rank must be a power of two in [1, 256], got {}",
                self.lora_rank
            )));
        }
        if self.learning_rate <= 0.0 {
            return Err(TrainerError::InvalidConfig(format!(
                "learning_rate must be positive, got {}",
                self.learning_rate
            )));
        }
        if self.batch_size == 0 {
            return Err(TrainerError::InvalidConfig(
                "batch_size must be at least 1".to_string(),
            ));
        }
        if self.max_epochs == 0 {
            return Err(TrainerError::InvalidConfig(
                "max_epochs must be at least 1".to_string(),
            ));
        }
        Ok(())
    }
}

/// A trained LoRA adapter ready for inference or further evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoRAAdapter {
    /// Path on disk where the adapter weights are stored.
    pub adapter_path: String,

    /// The LoRA rank used when this adapter was trained.
    pub lora_rank: u16,

    /// Number of training epochs completed.
    pub epochs_trained: u32,
}

/// Aggregate evaluation metrics for a trained adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResults {
    /// Perplexity on the evaluation split (lower is better).
    pub perplexity: f32,

    /// Accuracy on any classification tasks in the evaluation set (0.0–1.0).
    pub accuracy: f32,

    /// Number of examples used for evaluation.
    pub num_examples: usize,
}

/// Orchestrates LoRA fine-tuning: data preparation, training, and evaluation.
#[derive(Debug, Default)]
pub struct Trainer;

impl Trainer {
    /// Create a new `Trainer` instance.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Prepare training data from a JSONL file.
    ///
    /// Reads `jsonl_path`, validates that every line is valid JSON, and
    /// returns the path to the prepared dataset directory.
    ///
    /// # Errors
    ///
    /// Returns [`TrainerError::InvalidData`] when the file is empty,
    /// cannot be read, or contains malformed JSON lines.
    pub fn prepare_data(&self, jsonl_path: &str) -> Result<String, TrainerError> {
        if jsonl_path.is_empty() {
            return Err(TrainerError::InvalidData(
                "jsonl_path must not be empty".to_string(),
            ));
        }

        let content = std::fs::read_to_string(jsonl_path)
            .map_err(|e| TrainerError::InvalidData(e.to_string()))?;

        let mut count = 0usize;
        for (line_no, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let _: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                TrainerError::InvalidData(format!("line {}: {e}", line_no + 1))
            })?;
            count += 1;
        }

        if count == 0 {
            return Err(TrainerError::InvalidData(
                "training file contains no valid JSON lines".to_string(),
            ));
        }

        // Return the canonical input path as the "prepared dataset" path.
        // A real implementation would write processed tensors to a separate
        // directory; here we keep the dependency footprint at zero.
        Ok(jsonl_path.to_string())
    }

    /// Execute a LoRA fine-tuning run with the given configuration.
    ///
    /// Validates `config`, then (in a real deployment) invokes the
    /// vLLM / Hugging Face Transformers training backend.  This stub
    /// returns a [`LoRAAdapter`] immediately so the pipeline can be
    /// exercised without a GPU.
    ///
    /// # Errors
    ///
    /// Returns [`TrainerError::InvalidConfig`] when `config` fails
    /// validation, or [`TrainerError::TrainingFailed`] on backend errors.
    pub fn train_lora(&self, config: TrainerConfig) -> Result<LoRAAdapter, TrainerError> {
        config.validate()?;

        // Placeholder: real implementation would shell out to a Python
        // training script or call a Rust-native training backend.
        Ok(LoRAAdapter {
            adapter_path: format!("lora-rank{}-adapter", config.lora_rank),
            lora_rank: config.lora_rank,
            epochs_trained: config.max_epochs,
        })
    }

    /// Evaluate a trained [`LoRAAdapter`] on the held-out evaluation set.
    ///
    /// # Errors
    ///
    /// Returns [`TrainerError::EvaluationFailed`] when the adapter path is
    /// empty or the evaluation backend encounters an error.
    pub fn evaluate(&self, adapter: &LoRAAdapter) -> Result<EvalResults, TrainerError> {
        if adapter.adapter_path.is_empty() {
            return Err(TrainerError::EvaluationFailed(
                "adapter_path must not be empty".to_string(),
            ));
        }

        // Placeholder: real implementation would run the model on an eval
        // split and compute actual metrics.
        Ok(EvalResults {
            perplexity: 10.0,
            accuracy: 0.85,
            num_examples: 100,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Trainer initialisation ───────────────────────────────────────────────

    #[test]
    fn trainer_initializes() {
        let trainer = Trainer::new();
        // Just verifies that Trainer::new() compiles and returns a value.
        let _ = trainer;
    }

    // ── Config validation ────────────────────────────────────────────────────

    #[test]
    fn default_config_is_valid() {
        assert!(TrainerConfig::default_config().validate().is_ok());
    }

    #[test]
    fn config_rejects_zero_lora_rank() {
        let mut cfg = TrainerConfig::default_config();
        cfg.lora_rank = 0;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_non_power_of_two_lora_rank() {
        let mut cfg = TrainerConfig::default_config();
        cfg.lora_rank = 3;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_lora_rank_above_256() {
        let mut cfg = TrainerConfig::default_config();
        cfg.lora_rank = 512;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_non_positive_learning_rate() {
        let mut cfg = TrainerConfig::default_config();
        cfg.learning_rate = -0.001;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_zero_learning_rate() {
        let mut cfg = TrainerConfig::default_config();
        cfg.learning_rate = 0.0;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_zero_batch_size() {
        let mut cfg = TrainerConfig::default_config();
        cfg.batch_size = 0;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    #[test]
    fn config_rejects_zero_max_epochs() {
        let mut cfg = TrainerConfig::default_config();
        cfg.max_epochs = 0;
        assert!(matches!(cfg.validate(), Err(TrainerError::InvalidConfig(_))));
    }

    // ── Training pipeline ────────────────────────────────────────────────────

    #[test]
    fn train_lora_produces_adapter_with_matching_rank() {
        let trainer = Trainer::new();
        let cfg = TrainerConfig::default_config();
        let rank = cfg.lora_rank;
        let adapter = trainer.train_lora(cfg).expect("training should succeed");
        assert_eq!(adapter.lora_rank, rank);
    }

    #[test]
    fn train_lora_rejects_invalid_config() {
        let trainer = Trainer::new();
        let mut cfg = TrainerConfig::default_config();
        cfg.lora_rank = 0;
        assert!(trainer.train_lora(cfg).is_err());
    }

    #[test]
    fn evaluate_returns_results_for_valid_adapter() {
        let trainer = Trainer::new();
        let adapter = LoRAAdapter {
            adapter_path: "some/path".to_string(),
            lora_rank: 16,
            epochs_trained: 3,
        };
        let results = trainer.evaluate(&adapter).expect("evaluation should succeed");
        assert!(results.perplexity > 0.0);
        assert!((0.0..=1.0).contains(&results.accuracy));
    }

    #[test]
    fn evaluate_rejects_empty_adapter_path() {
        let trainer = Trainer::new();
        let adapter = LoRAAdapter {
            adapter_path: String::new(),
            lora_rank: 16,
            epochs_trained: 3,
        };
        assert!(matches!(
            trainer.evaluate(&adapter),
            Err(TrainerError::EvaluationFailed(_))
        ));
    }

    #[test]
    fn prepare_data_rejects_empty_path() {
        let trainer = Trainer::new();
        assert!(matches!(
            trainer.prepare_data(""),
            Err(TrainerError::InvalidData(_))
        ));
    }

    #[test]
    fn prepare_data_rejects_missing_file() {
        let trainer = Trainer::new();
        assert!(matches!(
            trainer.prepare_data("/nonexistent/path/data.jsonl"),
            Err(TrainerError::InvalidData(_))
        ));
    }

    #[test]
    fn prepare_data_accepts_valid_jsonl() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, r#"{{"prompt":"hello","completion":"world"}}"#).unwrap();
        writeln!(f, r#"{{"prompt":"foo","completion":"bar"}}"#).unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let trainer = Trainer::new();
        assert!(trainer.prepare_data(&path).is_ok());
    }

    #[test]
    fn prepare_data_rejects_invalid_json_line() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "not json").unwrap();
        let path = f.path().to_str().unwrap().to_string();
        let trainer = Trainer::new();
        assert!(matches!(
            trainer.prepare_data(&path),
            Err(TrainerError::InvalidData(_))
        ));
    }
}
