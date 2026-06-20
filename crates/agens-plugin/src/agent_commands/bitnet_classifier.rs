//! BitNet-backed cerebellum classifier — single-token classification.
//!
//! Uses the local BitNet model for fast message routing decisions.
//! Each classification is a single-token generation (~20ms on modern CPUs).

use pares_agens_bitnet::BitNetRunner;
use pares_agens_core::cerebellum::classifier::{
    ClassifierBackend, MessageClassification, MessageIntent,
};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// BitNet-powered classifier that uses single-token prompts for speed.
///
/// Instead of generating multi-field JSON (slow, unreliable on 2B models),
/// this asks simple categorical questions and parses single-token responses.
///
/// # Performance targets
///
/// | Machine | Cores | Expected per-classification |
/// |---------|-------|---------------------------|
/// | DevBox | 16 | ~20ms |
/// | Xeon server | 64 | ~5ms |
/// | Cluster (batched) | 256+ | ~2ms amortized |
pub struct BitNetClassifier {
    runner: Arc<BitNetRunner>,
    // Metrics
    total_classifications: AtomicU64,
    total_latency_us: AtomicU64,
    min_latency_us: AtomicU64,
    max_latency_us: AtomicU64,
}

impl BitNetClassifier {
    /// Create a new classifier from a model path.
    pub fn new(model_path: &Path) -> Result<Self, String> {
        let runner = BitNetRunner::load(model_path)
            .map_err(|e| format!("failed to load BitNet model: {e}"))?;
        Ok(Self {
            runner: Arc::new(runner),
            total_classifications: AtomicU64::new(0),
            total_latency_us: AtomicU64::new(0),
            min_latency_us: AtomicU64::new(u64::MAX),
            max_latency_us: AtomicU64::new(0),
        })
    }

    /// Classify intent with a minimal prompt optimized for 2B models.
    fn classify_intent_fast(&self, message: &str) -> MessageIntent {
        // Truncate long messages for classification (first 100 chars is enough)
        let truncated = if message.len() > 100 {
            &message[..100]
        } else {
            message
        };

        let prompt = format!(
            "Classify as question/task/command/chat/feedback:\n\"{}\"\nType: ",
            truncated
        );

        match self
            .generate_short(&prompt, 5)
            .trim()
            .to_lowercase()
            .as_str()
        {
            s if s.starts_with("question") || s.starts_with('q') => MessageIntent::Question,
            s if s.starts_with("task") || s.starts_with('t') => MessageIntent::Task,
            s if s.starts_with("command") || s.starts_with("cmd") || s.starts_with('/') => {
                MessageIntent::Command
            }
            s if s.starts_with("chat") || s.starts_with('c') => MessageIntent::Chat,
            s if s.starts_with("feedback") || s.starts_with('f') => MessageIntent::Feedback,
            _ => MessageIntent::Chat, // safe default
        }
    }

    /// Rate complexity 1-5.
    fn classify_complexity_fast(&self, message: &str) -> u8 {
        let truncated = if message.len() > 100 {
            &message[..100]
        } else {
            message
        };

        let prompt = format!(
            "Rate complexity 1(trivial) to 5(very complex):\n\"{}\"\nScore: ",
            truncated
        );

        self.generate_short(&prompt, 2)
            .trim()
            .chars()
            .next()
            .and_then(|c| c.to_digit(10))
            .map(|d| d as u8)
            .unwrap_or(3)
            .clamp(1, 5)
    }

    /// Does this message need external tools?
    fn needs_tools_fast(&self, message: &str) -> bool {
        let truncated = if message.len() > 100 {
            &message[..100]
        } else {
            message
        };

        let prompt = format!(
            "Does this need shell/web/api tools? yes or no:\n\"{}\"\nAnswer: ",
            truncated
        );

        self.generate_short(&prompt, 3)
            .trim()
            .to_lowercase()
            .starts_with('y')
    }

    /// Generate a very short response (1-5 tokens max).
    fn generate_short(&self, prompt: &str, max_tokens: usize) -> String {
        let params = pares_agens_bitnet::GenParams {
            temperature: 0.1, // near-deterministic for classification
            top_p: 0.5,
            seed: Some(42),
            max_tokens,
            n_threads: num_cpus(),
        };

        match self.runner.create_context() {
            Ok(mut ctx) => {
                let tokens = match ctx.tokenize(prompt) {
                    Ok(t) => t,
                    Err(_) => return String::new(),
                };
                match ctx.generate(&tokens, &params) {
                    Ok(stream) => {
                        let mut output = String::new();
                        for token_result in stream {
                            match token_result {
                                Ok(token_id) => {
                                    if let Ok(piece) = ctx.decode_token(token_id) {
                                        output.push_str(&piece);
                                        // Stop on text-level end markers
                                        if output.contains("<|end|>")
                                            || output.contains("<|eot_id|>")
                                            || output.contains("<|end_of_text|>")
                                        {
                                            // Trim the stop marker
                                            if let Some(pos) = output
                                                .find("<|end")
                                                .or_else(|| output.find("<|eot"))
                                            {
                                                output.truncate(pos);
                                            }
                                            break;
                                        }
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        output
                    }
                    Err(_) => String::new(),
                }
            }
            Err(_) => String::new(),
        }
    }
}

impl ClassifierBackend for BitNetClassifier {
    fn classify(&self, _system_prompt: &str, user_message: &str) -> Result<String, String> {
        let start = Instant::now();

        let intent = self.classify_intent_fast(user_message);
        let complexity = self.classify_complexity_fast(user_message);
        let needs_tools = self.needs_tools_fast(user_message);

        let elapsed_us = start.elapsed().as_micros() as u64;

        // Update metrics
        self.total_classifications.fetch_add(1, Ordering::Relaxed);
        self.total_latency_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        self.min_latency_us.fetch_min(elapsed_us, Ordering::Relaxed);
        self.max_latency_us.fetch_max(elapsed_us, Ordering::Relaxed);

        let count = self.total_classifications.load(Ordering::Relaxed);
        let avg_us = self.total_latency_us.load(Ordering::Relaxed) / count.max(1);

        tracing::info!(
            intent = ?intent,
            complexity,
            needs_tools,
            latency_ms = elapsed_us / 1000,
            avg_latency_ms = avg_us / 1000,
            total_classifications = count,
            "BitNet cerebellum classification"
        );

        // Construct the JSON that the existing classifier expects
        let classification = MessageClassification {
            intent,
            complexity,
            topic: extract_topic(user_message),
            topic_shift: false,
            entities: Vec::new(),
            plugin_match: None,
            completion_hint: None,
            needs_tools,
            needs_deep_model: complexity >= 4,
        };

        serde_json::to_string(&classification).map_err(|e| e.to_string())
    }
}

/// Extract a 1-3 word topic from the message (heuristic, no model call).
fn extract_topic(message: &str) -> String {
    let words: Vec<&str> = message.split_whitespace().collect();
    if words.len() <= 3 {
        message.to_string()
    } else {
        // Take the first noun-like words (skip question starters, articles)
        let skip = [
            "what", "how", "can", "you", "please", "the", "a", "an", "is", "are", "do",
        ];
        words
            .iter()
            .filter(|w| !skip.contains(&w.to_lowercase().as_str()))
            .take(3)
            .copied()
            .collect::<Vec<&str>>()
            .join(" ")
    }
}

/// Get available CPU cores.
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

// ── Cluster optimization notes ──────────────────────────────────────────────
//
// For high-core-count servers and multi-node clusters:
//
// 1. BATCH CLASSIFICATION
//    Instead of classifying one message at a time, batch N messages into a
//    single prompt with numbered responses. The KV cache is shared across
//    the batch, amortizing the prompt encoding cost.
//
//    prompt: "Classify each:\n1. {msg1}\n2. {msg2}\n...\nTypes: "
//    response: "1.question 2.task 3.chat"
//
// 2. PERSISTENT CONTEXT
//    Keep the model loaded in memory with a pre-encoded system prompt.
//    Only the user message portion needs new encoding per request.
//    Save ~50% of prompt processing time.
//
// 3. SPECULATIVE DECODING
//    For single-token classification, speculative decode is unnecessary.
//    But for longer generation (summaries, explanations), a smaller draft
//    model (e.g., BitNet 100M) could propose tokens verified by the 2B.
//
// 4. NUMA-AWARE THREAD PINNING
//    On multi-socket Xeons, pin inference threads to the NUMA node closest
//    to the model memory. Avoids cross-socket memory access penalties.
//    Set via: llama_context_params.numa = GGML_NUMA_DISTRIBUTE
//
// 5. CLUSTER ROUTING
//    With pares-rector, distribute classification across nodes:
//    - Each node loads the model once
//    - Rector routes messages to the least-loaded node
//    - Sub-millisecond classification at cluster scale
//    - Fault tolerance: any node can classify any message
//
// 6. MODEL SHARDING (future)
//    For larger models (8B+), shard across multiple nodes:
//    - Tensor parallelism across Hyperswarm peers
//    - Each node holds a slice of the model
//    - Coordinated inference via PluresDB CRDT sync
//
// 7. n_batch TUNING
//    For prompt processing (multiple tokens at once):
//    - n_batch = 512 is good for single requests
//    - n_batch = 2048 better for batch classification
//    - Should scale with available memory (each batch slot = ~2KB)
//
// 8. CONTEXT RECYCLING
//    For repeated classification patterns (same system prompt):
//    - Cache the KV state after system prompt processing
//    - Clone cached state for each new message
//    - Eliminates re-encoding the ~100 token system prompt every time

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_topic() {
        assert_eq!(extract_topic("What is the weather?"), "weather?");
        assert_eq!(extract_topic("hello"), "hello");
        assert_eq!(
            extract_topic("Can you close ADO work item 123"),
            "close ADO work"
        );
    }

    #[test]
    fn test_num_cpus() {
        assert!(num_cpus() >= 1);
    }
}
