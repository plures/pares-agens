/// Minimum number of characters required for a memory to pass the quality gate.
pub const MIN_CONTENT_LEN: usize = 20;

/// Cosine-similarity threshold above which a candidate is considered an echo of
/// an existing memory and will be discarded.
pub const ECHO_THRESHOLD: f32 = 0.95;

/// Short phrases that carry no durable information.
const NOISE_PATTERNS: &[&str] = &[
    "ok",
    "okay",
    "sure",
    "yes",
    "no",
    "nope",
    "got it",
    "thanks",
    "thank you",
    "great",
    "good",
    "nice",
    "cool",
    "awesome",
    "perfect",
    "alright",
    "right",
    "understood",
    "noted",
    "bye",
    "hello",
    "hi",
];

/// Return `true` when `text` is too short to be worth storing.
pub fn is_too_short(text: &str) -> bool {
    text.trim().len() < MIN_CONTENT_LEN
}

/// Return `true` when `text` is pure noise (acknowledgements, greetings, etc.).
///
/// A candidate is noise if it is too short **or** matches a known noise pattern
/// after stripping trailing punctuation.
pub fn is_noise(text: &str) -> bool {
    if is_too_short(text) {
        return true;
    }
    let lower = text.trim().to_lowercase();
    let stripped = lower.trim_end_matches(|c: char| c.is_ascii_punctuation());
    NOISE_PATTERNS.contains(&stripped)
}

/// Return `true` when `embedding` is nearly identical to an existing memory.
///
/// Uses cosine similarity; `existing` is a slice of all stored entries.
pub fn is_echo(embedding: &[f32], existing: &[&crate::memory::entry::MemoryEntry]) -> bool {
    existing
        .iter()
        .any(|m| cosine_similarity(embedding, &m.embedding) > ECHO_THRESHOLD)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_short_rejects_short_strings() {
        assert!(is_too_short("hi"));
        assert!(is_too_short("ok thanks"));
        assert!(!is_too_short(
            "This is a longer string with enough content."
        ));
    }

    #[test]
    fn noise_patterns_are_caught() {
        assert!(is_noise("ok"));
        assert!(is_noise("Thanks."));
        assert!(is_noise("Got it!"));
        assert!(!is_noise(
            "Here is a detailed explanation of async/await in Rust."
        ));
    }

    #[test]
    fn echo_detection_uses_cosine_threshold() {
        use crate::memory::entry::{MemoryCategory, MemoryEntry};

        let emb: Vec<f32> = (0..384).map(|i| if i == 0 { 1.0 } else { 0.0 }).collect();
        let existing_entry = MemoryEntry {
            id: "x".into(),
            content: "test".into(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: emb.clone(),
            score: 0.0,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let refs: Vec<&MemoryEntry> = vec![&existing_entry];
        // Identical vector → echo
        assert!(is_echo(&emb, &refs));
        // Orthogonal vector → not echo
        let other: Vec<f32> = (0..384).map(|i| if i == 1 { 1.0 } else { 0.0 }).collect();
        assert!(!is_echo(&other, &refs));
    }
}
