//! Inlined LoRA adapter types (formerly from `pares-trainer`).

use serde::{Deserialize, Serialize};

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
