//! Consensus mode — multi-expert query and output merging.
//!
//! [`ConsensusEngine`] implements the "query 2-3 experts, merge outputs for
//! higher quality" requirement from the issue.
//!
//! Because actual inference is handled by the runtime (not this crate), the
//! engine works with plain text responses.  The merge strategy is pluggable
//! via the [`MergeStrategy`] enum.

use serde::{Deserialize, Serialize};

use crate::{EnsembleError, ExpertDomain};

// ── ExpertResponse ────────────────────────────────────────────────────────────

/// A single expert's response to a query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpertResponse {
    /// The expert that produced this response.
    pub expert_id: String,
    /// The text of the response.
    pub text: String,
    /// Confidence score in `[0.0, 1.0]`, if available.  Defaults to `0.5`.
    pub confidence: f32,
}

impl ExpertResponse {
    /// Create a new response with the given expert ID, text, and confidence.
    #[must_use]
    pub fn new(expert_id: &str, text: &str, confidence: f32) -> Self {
        Self {
            expert_id: expert_id.to_string(),
            text: text.to_string(),
            confidence,
        }
    }

    /// Create a response with default confidence of `0.5`.
    #[must_use]
    pub fn with_default_confidence(expert_id: &str, text: &str) -> Self {
        Self::new(expert_id, text, 0.5)
    }
}

// ── MergeStrategy ─────────────────────────────────────────────────────────────

/// How multiple expert responses are combined into a single output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Return the response from the highest-confidence expert.
    Highest,
    /// Concatenate all responses separated by `"\n\n---\n\n"`.
    Concatenate,
    /// Return the majority response (by textual equality); fall back to
    /// [`MergeStrategy::Highest`] when no majority exists.
    Majority,
}

// ── ConsensusConfig ───────────────────────────────────────────────────────────

/// Configuration for the [`ConsensusEngine`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusConfig {
    /// Number of experts to consult.  Must be ≥ 2.
    pub k: usize,
    /// How to combine the collected responses.
    pub strategy: MergeStrategy,
}

impl ConsensusConfig {
    /// Default consensus configuration: top-2 experts, highest-confidence
    /// strategy.
    #[must_use]
    pub fn default_k2() -> Self {
        Self {
            k: 2,
            strategy: MergeStrategy::Highest,
        }
    }
}

// ── MergedResponse ────────────────────────────────────────────────────────────

/// The output of a consensus merge operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergedResponse {
    /// The merged text.
    pub text: String,
    /// The domain the query was routed to.
    pub domain: ExpertDomain,
    /// IDs of the experts that contributed to this response.
    pub contributing_experts: Vec<String>,
    /// Strategy used to produce this merged response.
    pub strategy_used: MergeStrategy,
}

// ── ConsensusEngine ───────────────────────────────────────────────────────────

/// Merges responses from multiple experts into a single high-quality output.
///
/// # Example
/// ```
/// use pares_agens_ensemble::ExpertDomain;
/// use pares_agens_ensemble::consensus::{
///     ConsensusConfig, ConsensusEngine, ExpertResponse, MergeStrategy,
/// };
///
/// let engine = ConsensusEngine::new(ConsensusConfig::default_k2());
/// let responses = vec![
///     ExpertResponse::new("e1", "Paris", 0.9),
///     ExpertResponse::new("e2", "Paris", 0.7),
/// ];
/// let merged = engine.merge(ExpertDomain::Factual, responses).unwrap();
/// assert_eq!(merged.text, "Paris");
/// ```
#[derive(Debug, Clone)]
pub struct ConsensusEngine {
    config: ConsensusConfig,
}

impl ConsensusEngine {
    /// Create a new engine with the given configuration.
    #[must_use]
    pub fn new(config: ConsensusConfig) -> Self {
        Self { config }
    }

    /// Merge `responses` into a single [`MergedResponse`].
    ///
    /// Requires at least 2 responses (matching `config.k`).
    ///
    /// # Errors
    ///
    /// Returns [`EnsembleError::InsufficientConsensusResponses`] when fewer
    /// than 2 responses are provided, or
    /// [`EnsembleError::ExpertResponseError`] when a response text is empty.
    pub fn merge(
        &self,
        domain: ExpertDomain,
        responses: Vec<ExpertResponse>,
    ) -> Result<MergedResponse, EnsembleError> {
        if responses.len() < 2 {
            return Err(EnsembleError::InsufficientConsensusResponses(
                responses.len(),
            ));
        }
        for r in &responses {
            if r.text.trim().is_empty() {
                return Err(EnsembleError::ExpertResponseError(format!(
                    "expert '{}' returned an empty response",
                    r.expert_id
                )));
            }
        }

        let contributing: Vec<String> = responses.iter().map(|r| r.expert_id.clone()).collect();

        let merged_text = match self.config.strategy {
            MergeStrategy::Highest => Self::merge_highest(&responses),
            MergeStrategy::Concatenate => Self::merge_concatenate(&responses),
            MergeStrategy::Majority => Self::merge_majority(&responses),
        };

        Ok(MergedResponse {
            text: merged_text,
            domain,
            contributing_experts: contributing,
            strategy_used: self.config.strategy,
        })
    }

    /// Return the text of the highest-confidence response.
    fn merge_highest(responses: &[ExpertResponse]) -> String {
        responses
            .iter()
            .max_by(|a, b| {
                a.confidence
                    .partial_cmp(&b.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|r| r.text.clone())
            .unwrap_or_default()
    }

    /// Concatenate all responses separated by a horizontal rule.
    fn merge_concatenate(responses: &[ExpertResponse]) -> String {
        responses
            .iter()
            .map(|r| r.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }

    /// Return the majority response; fall back to highest-confidence on ties.
    fn merge_majority(responses: &[ExpertResponse]) -> String {
        let mut counts: std::collections::HashMap<&str, (usize, f32)> =
            std::collections::HashMap::new();

        for r in responses {
            let entry = counts.entry(r.text.as_str()).or_insert((0, 0.0));
            entry.0 += 1;
            if r.confidence > entry.1 {
                entry.1 = r.confidence;
            }
        }

        // Find the text with the most votes; break ties by max confidence.
        let winner = counts
            .iter()
            .max_by(|(_, (ca, fa)), (_, (cb, fb))| {
                ca.cmp(cb)
                    .then(fa.partial_cmp(fb).unwrap_or(std::cmp::Ordering::Equal))
            })
            .map(|(text, _)| *text)
            .unwrap_or_default();

        if winner.is_empty() {
            Self::merge_highest(responses)
        } else {
            winner.to_string()
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(strategy: MergeStrategy) -> ConsensusEngine {
        ConsensusEngine::new(ConsensusConfig { k: 2, strategy })
    }

    fn resp(id: &str, text: &str, conf: f32) -> ExpertResponse {
        ExpertResponse::new(id, text, conf)
    }

    // ── Errors ────────────────────────────────────────────────────────────

    #[test]
    fn merge_rejects_single_response() {
        let e = engine(MergeStrategy::Highest);
        let r = e.merge(ExpertDomain::Code, vec![resp("e1", "hi", 0.9)]);
        assert!(matches!(
            r,
            Err(EnsembleError::InsufficientConsensusResponses(1))
        ));
    }

    #[test]
    fn merge_rejects_empty_response_text() {
        let e = engine(MergeStrategy::Highest);
        let responses = vec![resp("e1", "hello", 0.8), resp("e2", "  ", 0.5)];
        assert!(matches!(
            e.merge(ExpertDomain::Code, responses),
            Err(EnsembleError::ExpertResponseError(_))
        ));
    }

    // ── Highest strategy ──────────────────────────────────────────────────

    #[test]
    fn merge_highest_picks_highest_confidence() {
        let e = engine(MergeStrategy::Highest);
        let responses = vec![resp("e1", "A", 0.6), resp("e2", "B", 0.9)];
        let merged = e.merge(ExpertDomain::Code, responses).unwrap();
        assert_eq!(merged.text, "B");
        assert_eq!(merged.strategy_used, MergeStrategy::Highest);
    }

    // ── Concatenate strategy ──────────────────────────────────────────────

    #[test]
    fn merge_concatenate_joins_all_responses() {
        let e = engine(MergeStrategy::Concatenate);
        let responses = vec![resp("e1", "Part 1", 0.8), resp("e2", "Part 2", 0.7)];
        let merged = e.merge(ExpertDomain::Writing, responses).unwrap();
        assert!(merged.text.contains("Part 1"));
        assert!(merged.text.contains("Part 2"));
        assert!(merged.text.contains("---"));
    }

    // ── Majority strategy ─────────────────────────────────────────────────

    #[test]
    fn merge_majority_picks_most_common_answer() {
        let e = engine(MergeStrategy::Majority);
        let responses = vec![
            resp("e1", "Paris", 0.9),
            resp("e2", "Paris", 0.7),
            resp("e3", "London", 0.8),
        ];
        let merged = e.merge(ExpertDomain::Factual, responses).unwrap();
        assert_eq!(merged.text, "Paris");
    }

    #[test]
    fn merge_majority_falls_back_to_highest_confidence_on_tie() {
        let e = engine(MergeStrategy::Majority);
        let responses = vec![
            resp("e1", "A", 0.6),
            resp("e2", "B", 0.9), // higher confidence — wins the tie
        ];
        let merged = e.merge(ExpertDomain::Code, responses).unwrap();
        // One vote each (tie); majority max_by selects B due to higher confidence.
        assert_eq!(merged.text, "B");
    }

    // ── Contributing experts ──────────────────────────────────────────────

    #[test]
    fn merged_response_records_contributing_experts() {
        let e = engine(MergeStrategy::Highest);
        let responses = vec![resp("e1", "hello", 0.8), resp("e2", "world", 0.7)];
        let merged = e.merge(ExpertDomain::Dialogue, responses).unwrap();
        assert!(merged.contributing_experts.contains(&"e1".to_string()));
        assert!(merged.contributing_experts.contains(&"e2".to_string()));
    }
}
