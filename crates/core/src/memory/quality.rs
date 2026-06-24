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

    /// Kill mutant: `<` → `<=` in is_too_short boundary.
    #[test]
    fn too_short_boundary_exactly_min_len() {
        // Exactly MIN_CONTENT_LEN chars must NOT be too short
        let exactly_min: String = "a".repeat(MIN_CONTENT_LEN);
        assert!(!is_too_short(&exactly_min));
        // One less must be too short
        let one_less: String = "a".repeat(MIN_CONTENT_LEN - 1);
        assert!(is_too_short(&one_less));
    }

    /// Kill mutant: trimming behavior — whitespace-padded short strings
    #[test]
    fn too_short_trims_whitespace() {
        // String with content < MIN_CONTENT_LEN but whitespace padding
        let padded = format!("{}hi{}", " ".repeat(20), " ".repeat(20));
        assert!(is_too_short(&padded));
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

    /// Kill mutant: is_noise must return true for short content even if it's
    /// not in NOISE_PATTERNS.
    #[test]
    fn noise_short_non_pattern_still_noise() {
        // Short content that isn't a noise pattern but is too short
        assert!(is_noise("xyz abc"));
    }

    /// Kill mutant: is_noise must strip trailing punctuation before matching.
    #[test]
    fn noise_strips_multiple_punctuation() {
        assert!(is_noise("ok!!!"));
        assert!(is_noise("  thanks...  "));
        assert!(is_noise("COOL?!"));
    }

    /// Kill mutant: long non-noise string that contains a noise word as substring.
    #[test]
    fn noise_rejects_long_string_with_noise_substring() {
        // Contains "ok" but is long and not just noise
        assert!(!is_noise(
            "This approach looks ok but needs more testing before merge."
        ));
    }

    /// Kill mutant: is_noise with exactly MIN_CONTENT_LEN content that IS a noise pattern.
    /// After trimming punctuation if the stripped version matches a pattern, it's noise.
    #[test]
    fn noise_exact_min_len_noise_pattern() {
        // "understood" (10 chars) + padding to reach exactly MIN_CONTENT_LEN
        // with punctuation that gets stripped
        let padded = format!("understood{}", "!".repeat(MIN_CONTENT_LEN - 10));
        assert!(is_noise(&padded));
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

    /// Kill mutant: `>` → `>=` in ECHO_THRESHOLD comparison.
    /// A vector with cosine similarity exactly equal to ECHO_THRESHOLD must
    /// NOT be considered an echo (the check is strictly greater-than).
    #[test]
    fn echo_at_exact_threshold_is_not_echo() {
        use crate::memory::entry::{MemoryCategory, MemoryEntry};

        // Construct two vectors with cosine similarity = ECHO_THRESHOLD exactly.
        // cos(a,b) = dot/(|a|*|b|). For unit vectors: cos = dot.
        // Unit vector along dim 0:
        let a: Vec<f32> = (0..384).map(|i| if i == 0 { 1.0 } else { 0.0 }).collect();
        // Unit vector: cos(theta) = ECHO_THRESHOLD → b[0] = ECHO_THRESHOLD,
        // b[1] = sqrt(1 - ECHO_THRESHOLD^2), rest 0.
        let sin_t = (1.0 - ECHO_THRESHOLD * ECHO_THRESHOLD).sqrt();
        let b: Vec<f32> = (0..384)
            .map(|i| match i {
                0 => ECHO_THRESHOLD,
                1 => sin_t,
                _ => 0.0,
            })
            .collect();

        let existing_entry = MemoryEntry {
            id: "x".into(),
            content: "test".into(),
            category: MemoryCategory::Conversation,
            tags: vec![],
            embedding: a.clone(),
            score: 0.0,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let refs: Vec<&MemoryEntry> = vec![&existing_entry];
        // Cosine similarity is exactly ECHO_THRESHOLD → NOT an echo
        assert!(
            !is_echo(&b, &refs),
            "similarity at exactly ECHO_THRESHOLD should NOT be flagged as echo"
        );
    }

    /// Kill mutant: cosine_similarity returns 0.0 for mismatched dimensions.
    #[test]
    fn cosine_similarity_mismatched_lengths() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    /// Kill mutant: `dot / (norm_a * norm_b)` — the division and multiplication
    /// in the denominator must be correct. Non-unit vectors expose swapping / and *.
    #[test]
    fn cosine_similarity_non_unit_vectors() {
        // a = [3.0, 4.0] → |a| = 5.0
        // b = [1.0, 0.0] → |b| = 1.0
        // dot = 3.0
        // cos = 3.0 / (5.0 * 1.0) = 0.6
        let a = vec![3.0, 4.0];
        let b = vec![1.0, 0.0];
        let result = cosine_similarity(&a, &b);
        assert!((result - 0.6).abs() < 1e-6, "expected 0.6, got {result}");
        // With mutant `/ → *`: 3.0 * (5.0 * 1.0) = 15.0 ≠ 0.6 ✓
        // With mutant `* → /`: 3.0 / (5.0 / 1.0) = 0.6 — still passes!
        // Need vectors where norm_a ≠ 1.0 AND norm_b ≠ 1.0:
        let c = vec![2.0, 0.0]; // |c| = 2.0
        let d = vec![3.0, 0.0]; // |d| = 3.0
                                // dot = 6.0, cos = 6.0 / (2.0 * 3.0) = 1.0
                                // With mutant `* → /`: 6.0 / (2.0 / 3.0) = 9.0 ≠ 1.0 ✓
        let result2 = cosine_similarity(&c, &d);
        assert!(
            (result2 - 1.0).abs() < 1e-6,
            "expected 1.0 for parallel vectors, got {result2}"
        );
    }

    /// Kill mutant: cosine_similarity returns 0.0 for zero-magnitude vectors.
    #[test]
    fn cosine_similarity_zero_vector() {
        let zero = vec![0.0; 384];
        let normal: Vec<f32> = (0..384).map(|i| if i == 0 { 1.0 } else { 0.0 }).collect();
        assert_eq!(cosine_similarity(&zero, &normal), 0.0);
        assert_eq!(cosine_similarity(&normal, &zero), 0.0);
        assert_eq!(cosine_similarity(&zero, &zero), 0.0);
    }

    /// Kill mutant: is_echo with empty existing set always returns false.
    #[test]
    fn echo_empty_existing_is_not_echo() {
        let emb: Vec<f32> = (0..384).map(|i| if i == 0 { 1.0 } else { 0.0 }).collect();
        let refs: Vec<&crate::memory::entry::MemoryEntry> = vec![];
        assert!(!is_echo(&emb, &refs));
    }
}
