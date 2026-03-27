//! Skill detection and auto-clustering for training data.
//!
//! Clusters [`TrainingExample`]s by domain using lightweight keyword-frequency
//! embeddings, identifies the dominant [`SkillCategory`] in each cluster, and
//! decides whether a cluster is incoherent enough to benefit from splitting.

use crate::{TrainerError, TrainingExample};
use serde::{Deserialize, Serialize};

// ── Public types ─────────────────────────────────────────────────────────────

/// A cluster of training examples that share a common domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCluster {
    /// Training examples assigned to this cluster.
    pub examples: Vec<TrainingExample>,

    /// Centroid of the cluster in embedding space.
    pub centroid: Vec<f32>,

    /// Coherence score measuring intra-cluster similarity (0.0–1.0).
    pub coherence_score: f32,
}

/// A category of skill identified in a training cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SkillCategory {
    /// Coding examples — inner value is the dominant language (e.g. `"rust"`).
    Coding(String),
    /// Writing examples — inner value is the dominant genre (e.g. `"essay"`).
    Writing(String),
    /// Analysis examples — inner value is the dominant domain (e.g. `"financial"`).
    Analysis(String),
    /// Domain-specific examples that don't fit the other categories.
    DomainSpecific(String),
}

/// Minimum coherence score below which a cluster should be split into
/// sub-clusters.
const SPLIT_THRESHOLD: f32 = 0.4;

/// Minimum number of examples required before considering a split.
const MIN_SPLIT_SIZE: usize = 4;

/// Default number of k-means clusters.
const DEFAULT_K: usize = 4;

/// Maximum k-means iterations.
const MAX_ITER: usize = 20;

/// Detects skills and clusters training data by domain.
#[derive(Debug, Default)]
pub struct SkillDetector;

impl SkillDetector {
    /// Create a new `SkillDetector`.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Cluster training examples by domain using lightweight text embeddings.
    ///
    /// Produces between 1 and [`DEFAULT_K`] clusters by running k-means over
    /// keyword-frequency vectors derived from each example's prompt.
    ///
    /// # Errors
    ///
    /// Returns [`TrainerError::InvalidData`] when `training_data` is empty.
    pub fn cluster_by_domain(
        &self,
        training_data: &[TrainingExample],
    ) -> Result<Vec<SkillCluster>, TrainerError> {
        if training_data.is_empty() {
            return Err(TrainerError::InvalidData(
                "training_data must not be empty".to_string(),
            ));
        }

        let embeddings: Vec<Vec<f32>> = training_data.iter().map(|ex| embed(&ex.prompt)).collect();

        let k = DEFAULT_K.min(training_data.len());
        let assignments = kmeans(&embeddings, k);

        let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); k];
        for (i, &cluster_idx) in assignments.iter().enumerate() {
            buckets[cluster_idx].push(i);
        }

        let clusters: Vec<SkillCluster> = buckets
            .into_iter()
            .filter(|b| !b.is_empty())
            .map(|indices| {
                let examples: Vec<TrainingExample> =
                    indices.iter().map(|&i| training_data[i].clone()).collect();
                let cluster_embeddings: Vec<&Vec<f32>> =
                    indices.iter().map(|&i| &embeddings[i]).collect();
                let centroid = compute_centroid(&cluster_embeddings);
                let coherence_score = compute_coherence(&cluster_embeddings, &centroid);
                SkillCluster {
                    examples,
                    centroid,
                    coherence_score,
                }
            })
            .collect();

        Ok(clusters)
    }

    /// Identify the skill categories present in a cluster.
    ///
    /// Returns one or more [`SkillCategory`] values based on the keyword
    /// distribution in the cluster's example prompts.  At least one category
    /// is always returned.
    #[must_use]
    pub fn identify_skills(&self, cluster: &SkillCluster) -> Vec<SkillCategory> {
        let mut coding = 0u32;
        let mut writing = 0u32;
        let mut analysis = 0u32;

        for ex in &cluster.examples {
            let lower = ex.prompt.to_lowercase();
            coding += count_keywords(&lower, CODING_KEYWORDS);
            writing += count_keywords(&lower, WRITING_KEYWORDS);
            analysis += count_keywords(&lower, ANALYSIS_KEYWORDS);
        }

        let total = coding + writing + analysis;
        if total == 0 {
            return vec![SkillCategory::DomainSpecific("unknown".to_string())];
        }

        // Include a category when it accounts for at least 25 % of keywords.
        let threshold = total / 4;
        let mut categories = Vec::new();

        if coding > threshold {
            categories.push(SkillCategory::Coding(dominant_coding_language(
                &cluster.examples,
            )));
        }
        if writing > threshold {
            categories.push(SkillCategory::Writing(dominant_writing_genre(
                &cluster.examples,
            )));
        }
        if analysis > threshold {
            categories.push(SkillCategory::Analysis(dominant_analysis_domain(
                &cluster.examples,
            )));
        }

        if categories.is_empty() {
            categories.push(SkillCategory::DomainSpecific("mixed".to_string()));
        }
        categories
    }

    /// Return `true` when `cluster` is large and incoherent enough to benefit
    /// from being split into sub-clusters.
    ///
    /// A cluster is a split candidate when it contains at least
    /// [`MIN_SPLIT_SIZE`] examples **and** its coherence score falls below
    /// [`SPLIT_THRESHOLD`].
    #[must_use]
    pub fn should_split_cluster(&self, cluster: &SkillCluster) -> bool {
        cluster.examples.len() >= MIN_SPLIT_SIZE && cluster.coherence_score < SPLIT_THRESHOLD
    }
}

// ── Embedding helpers ────────────────────────────────────────────────────────

/// Keyword lists that drive the lightweight domain embeddings.
const CODING_KEYWORDS: &[&str] = &[
    "function",
    "code",
    "implement",
    "class",
    "variable",
    "algorithm",
    "debug",
    "compile",
    "error",
    "python",
    "rust",
    "javascript",
    "typescript",
    "java",
    "api",
    "library",
    "syntax",
    "loop",
    "array",
    "struct",
];

const WRITING_KEYWORDS: &[&str] = &[
    "write",
    "essay",
    "paragraph",
    "story",
    "narrative",
    "describe",
    "summarize",
    "draft",
    "content",
    "blog",
    "article",
    "report",
    "document",
    "prose",
    "tone",
    "style",
    "audience",
    "introduction",
    "conclusion",
];

const ANALYSIS_KEYWORDS: &[&str] = &[
    "analyze",
    "compare",
    "evaluate",
    "assess",
    "review",
    "identify",
    "explain",
    "reason",
    "cause",
    "effect",
    "impact",
    "relationship",
    "contrast",
    "insight",
    "trend",
    "pattern",
    "data",
    "result",
    "finding",
    "conclusion",
];

/// Build a keyword-frequency embedding vector for `text`.
///
/// The vector has three components (coding, writing, analysis), normalised to
/// unit length when non-zero.
fn embed(text: &str) -> Vec<f32> {
    let lower = text.to_lowercase();
    let coding = count_keywords(&lower, CODING_KEYWORDS) as f32;
    let writing = count_keywords(&lower, WRITING_KEYWORDS) as f32;
    let analysis = count_keywords(&lower, ANALYSIS_KEYWORDS) as f32;
    let mut v = vec![coding, writing, analysis];
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Count how many of `keywords` appear as substrings of `text`.
fn count_keywords(text: &str, keywords: &[&str]) -> u32 {
    keywords
        .iter()
        .filter(|&&kw| text.contains(kw))
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

// ── k-means ──────────────────────────────────────────────────────────────────

/// Assign each embedding to one of `k` clusters via k-means.
///
/// Centroids are seeded with the first `k` embeddings (deterministic).
fn kmeans(embeddings: &[Vec<f32>], k: usize) -> Vec<usize> {
    let n = embeddings.len();
    if k >= n {
        return (0..n).collect();
    }

    let mut centroids: Vec<Vec<f32>> = embeddings[..k].to_vec();
    let mut assignments = vec![0usize; n];

    for _ in 0..MAX_ITER {
        let prev = assignments.clone();

        // Assignment step — move each point to its nearest centroid.
        for (i, emb) in embeddings.iter().enumerate() {
            assignments[i] = centroids
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    euclidean_sq(emb, a)
                        .partial_cmp(&euclidean_sq(emb, b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(idx, _)| idx)
                .unwrap_or(0);
        }

        if assignments == prev {
            break;
        }

        // Update step — recompute each centroid from its members.
        for (c, centroid) in centroids.iter_mut().enumerate().take(k) {
            let members: Vec<&Vec<f32>> = embeddings
                .iter()
                .enumerate()
                .filter(|(i, _)| assignments[*i] == c)
                .map(|(_, e)| e)
                .collect();
            if !members.is_empty() {
                *centroid = compute_centroid(&members);
            }
        }
    }

    assignments
}

/// Squared Euclidean distance between two equal-length vectors.
fn euclidean_sq(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

/// Compute the element-wise mean of a non-empty slice of vectors.
fn compute_centroid(vecs: &[&Vec<f32>]) -> Vec<f32> {
    if vecs.is_empty() {
        return Vec::new();
    }
    let dim = vecs[0].len();
    let mut sum = vec![0.0f32; dim];
    for v in vecs {
        for (s, x) in sum.iter_mut().zip(v.iter()) {
            *s += x;
        }
    }
    let n = vecs.len() as f32;
    sum.iter_mut().for_each(|s| *s /= n);
    sum
}

/// Compute the mean cosine similarity of each vector to the centroid.
///
/// Returns a value in `[0.0, 1.0]`.
fn compute_coherence(vecs: &[&Vec<f32>], centroid: &[f32]) -> f32 {
    if vecs.is_empty() {
        return 0.0;
    }
    let sum: f32 = vecs.iter().map(|v| cosine_similarity(v, centroid)).sum();
    sum / vecs.len() as f32
}

/// Cosine similarity between two vectors, clamped to `[0.0, 1.0]`.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)).clamp(0.0, 1.0)
}

// ── Category helpers ──────────────────────────────────────────────────────────

const CODING_LANGUAGES: &[&str] = &[
    "python",
    "rust",
    "javascript",
    "typescript",
    "java",
    "c++",
    "go",
    "sql",
];

const WRITING_GENRES: &[&str] = &[
    "essay",
    "story",
    "blog",
    "report",
    "article",
    "documentation",
];

const ANALYSIS_DOMAINS: &[&str] = &["financial", "scientific", "legal", "medical", "technical"];

fn dominant_coding_language(examples: &[TrainingExample]) -> String {
    best_match(examples, CODING_LANGUAGES, "unknown")
}

fn dominant_writing_genre(examples: &[TrainingExample]) -> String {
    best_match(examples, WRITING_GENRES, "general")
}

fn dominant_analysis_domain(examples: &[TrainingExample]) -> String {
    best_match(examples, ANALYSIS_DOMAINS, "general")
}

/// Return the item from `candidates` that appears most often across all
/// example prompts, falling back to `default` when none appear.
fn best_match(examples: &[TrainingExample], candidates: &[&str], default: &str) -> String {
    let combined: String = examples
        .iter()
        .map(|e| e.prompt.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    candidates
        .iter()
        .max_by_key(|&&term| combined.matches(term).count())
        .filter(|&&term| combined.contains(term))
        .map(|s| (*s).to_string())
        .unwrap_or_else(|| default.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_example(prompt: &str) -> TrainingExample {
        TrainingExample {
            prompt: prompt.to_string(),
            completion: "answer".to_string(),
        }
    }

    fn coding_examples() -> Vec<TrainingExample> {
        vec![
            make_example("implement a rust function to sort an array"),
            make_example("debug this python code with a loop error"),
            make_example("write a javascript api library class"),
            make_example("explain this rust struct and algorithm"),
        ]
    }

    fn writing_examples() -> Vec<TrainingExample> {
        vec![
            make_example("write an essay with a clear introduction and conclusion"),
            make_example("draft a blog article in a narrative style"),
            make_example("describe the tone and style of this prose document"),
            make_example("summarize this report and write a paragraph"),
        ]
    }

    fn analysis_examples() -> Vec<TrainingExample> {
        vec![
            make_example("analyze the trend and identify the pattern in this data"),
            make_example("compare and evaluate the impact and effect of this policy"),
            make_example("explain the cause and relationship between the findings"),
            make_example("assess the result and contrast the insight with prior research"),
        ]
    }

    // ── cluster_by_domain ────────────────────────────────────────────────────

    #[test]
    fn cluster_rejects_empty_input() {
        let detector = SkillDetector::new();
        assert!(matches!(
            detector.cluster_by_domain(&[]),
            Err(TrainerError::InvalidData(_))
        ));
    }

    #[test]
    fn cluster_single_example_returns_one_cluster() {
        let detector = SkillDetector::new();
        let data = vec![make_example("hello world")];
        let clusters = detector.cluster_by_domain(&data).unwrap();
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].examples.len(), 1);
    }

    #[test]
    fn cluster_assigns_all_examples() {
        let detector = SkillDetector::new();
        let mut data = coding_examples();
        data.extend(writing_examples());
        let total = data.len();
        let clusters = detector.cluster_by_domain(&data).unwrap();
        let assigned: usize = clusters.iter().map(|c| c.examples.len()).sum();
        assert_eq!(assigned, total);
    }

    #[test]
    fn cluster_produces_valid_coherence_scores() {
        let detector = SkillDetector::new();
        let data = coding_examples();
        let clusters = detector.cluster_by_domain(&data).unwrap();
        for c in &clusters {
            assert!(
                (0.0..=1.0).contains(&c.coherence_score),
                "coherence_score {} out of range",
                c.coherence_score
            );
        }
    }

    #[test]
    fn cluster_centroid_has_correct_dimension() {
        let detector = SkillDetector::new();
        let data = coding_examples();
        let clusters = detector.cluster_by_domain(&data).unwrap();
        for c in &clusters {
            // embed() always produces a 3-component vector
            assert_eq!(c.centroid.len(), 3);
        }
    }

    // ── identify_skills ──────────────────────────────────────────────────────

    #[test]
    fn identifies_coding_cluster() {
        let detector = SkillDetector::new();
        let clusters = detector.cluster_by_domain(&coding_examples()).unwrap();
        // At least one cluster should be identified as containing coding
        let any_coding = clusters.iter().any(|c| {
            detector
                .identify_skills(c)
                .iter()
                .any(|cat| matches!(cat, SkillCategory::Coding(_)))
        });
        assert!(any_coding, "expected a Coding category in coding clusters");
    }

    #[test]
    fn identifies_writing_cluster() {
        let detector = SkillDetector::new();
        let clusters = detector.cluster_by_domain(&writing_examples()).unwrap();
        let any_writing = clusters.iter().any(|c| {
            detector
                .identify_skills(c)
                .iter()
                .any(|cat| matches!(cat, SkillCategory::Writing(_)))
        });
        assert!(
            any_writing,
            "expected a Writing category in writing clusters"
        );
    }

    #[test]
    fn identifies_analysis_cluster() {
        let detector = SkillDetector::new();
        let clusters = detector.cluster_by_domain(&analysis_examples()).unwrap();
        let any_analysis = clusters.iter().any(|c| {
            detector
                .identify_skills(c)
                .iter()
                .any(|cat| matches!(cat, SkillCategory::Analysis(_)))
        });
        assert!(
            any_analysis,
            "expected an Analysis category in analysis clusters"
        );
    }

    #[test]
    fn identify_skills_returns_domain_specific_for_generic_text() {
        let detector = SkillDetector::new();
        let cluster = SkillCluster {
            examples: vec![
                make_example("hello"),
                make_example("world"),
                make_example("foo bar"),
            ],
            centroid: vec![0.0, 0.0, 0.0],
            coherence_score: 0.5,
        };
        let cats = detector.identify_skills(&cluster);
        assert!(!cats.is_empty());
        assert!(
            cats.iter()
                .any(|c| matches!(c, SkillCategory::DomainSpecific(_))),
            "expected DomainSpecific for generic text, got {cats:?}"
        );
    }

    #[test]
    fn identify_skills_always_returns_at_least_one_category() {
        let detector = SkillDetector::new();
        for example_set in [
            &coding_examples(),
            &writing_examples(),
            &analysis_examples(),
        ] {
            let clusters = detector.cluster_by_domain(example_set).unwrap();
            for c in &clusters {
                let cats = detector.identify_skills(c);
                assert!(!cats.is_empty(), "identify_skills returned empty vec");
            }
        }
    }

    // ── should_split_cluster ─────────────────────────────────────────────────

    #[test]
    fn split_required_for_large_incoherent_cluster() {
        let detector = SkillDetector::new();
        let cluster = SkillCluster {
            examples: vec![make_example("a"); MIN_SPLIT_SIZE],
            centroid: vec![0.0, 0.0, 0.0],
            coherence_score: SPLIT_THRESHOLD - 0.01,
        };
        assert!(detector.should_split_cluster(&cluster));
    }

    #[test]
    fn no_split_for_coherent_cluster() {
        let detector = SkillDetector::new();
        let cluster = SkillCluster {
            examples: vec![make_example("a"); MIN_SPLIT_SIZE],
            centroid: vec![0.0, 0.0, 0.0],
            coherence_score: SPLIT_THRESHOLD + 0.01,
        };
        assert!(!detector.should_split_cluster(&cluster));
    }

    #[test]
    fn no_split_for_small_cluster() {
        let detector = SkillDetector::new();
        let cluster = SkillCluster {
            examples: vec![make_example("a"); MIN_SPLIT_SIZE - 1],
            centroid: vec![0.0, 0.0, 0.0],
            coherence_score: 0.0,
        };
        assert!(!detector.should_split_cluster(&cluster));
    }

    #[test]
    fn no_split_for_small_coherent_cluster() {
        let detector = SkillDetector::new();
        let cluster = SkillCluster {
            examples: vec![make_example("a"); 2],
            centroid: vec![1.0, 0.0, 0.0],
            coherence_score: 0.9,
        };
        assert!(!detector.should_split_cluster(&cluster));
    }
}
