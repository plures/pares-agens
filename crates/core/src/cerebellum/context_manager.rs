//! Context Manager — the cerebellum's core responsibility.
//!
//! Instead of classifying messages, the context manager decides:
//! - What context to KEEP (still relevant)
//! - What context to ADD (newly relevant from memory)
//! - What context to REMOVE (stale, irrelevant, crowding the window)
//!
//! This is trained from outcomes: when the conscious model succeeds,
//! the context was right. When it fails, it was wrong. Over time,
//! the relevance weights improve.

use std::collections::HashMap;

/// A managed context window with relevance scoring.
#[derive(Debug, Clone)]
pub struct ManagedContext {
    /// Active context items, scored by relevance.
    pub items: Vec<ContextItem>,
    /// Total token budget for the context window.
    pub token_budget: usize,
    /// Tokens currently used.
    pub tokens_used: usize,
    /// Whether the topic shifted (clear history signal).
    pub topic_shifted: bool,
}

/// A single piece of context with metadata.
#[derive(Debug, Clone)]
pub struct ContextItem {
    /// Unique ID (for tracking outcomes).
    pub id: String,
    /// The actual content.
    pub content: String,
    /// Estimated token count.
    pub tokens: usize,
    /// Relevance score (0.0 - 1.0).
    pub relevance: f32,
    /// Source of this context.
    pub source: ContextSource,
    /// How many turns this has been in context.
    pub age_turns: u32,
    /// Was this item part of a successful interaction?
    pub success_count: u32,
    /// Was this item part of a failed interaction?
    pub failure_count: u32,
}

/// Where a context item came from.
#[derive(Debug, Clone, PartialEq)]
pub enum ContextSource {
    /// From PluresDB memory recall.
    Memory,
    /// From conversation history.
    History,
    /// From a system/personality file.
    System,
    /// From a procedure result.
    Procedure,
    /// Entity extracted from the current message.
    Entity,
}

/// Pattern-based entity extractor (no model needed).
pub struct EntityExtractor;

impl EntityExtractor {
    /// Extract structured entities from a message using patterns.
    pub fn extract(message: &str) -> Vec<ExtractedEntity> {
        let mut entities = Vec::new();

        // ADO work item IDs
        for cap in regex_lite::Regex::new(r"#(\d{5,7})")
            .unwrap()
            .captures_iter(message)
        {
            entities.push(ExtractedEntity {
                kind: EntityKind::AdoWorkItem,
                value: cap[1].to_string(),
                context_key: format!("ado:workitem:{}", &cap[1]),
            });
        }

        // Git SHAs
        for cap in regex_lite::Regex::new(r"\b([0-9a-f]{7,40})\b")
            .unwrap()
            .captures_iter(message)
        {
            entities.push(ExtractedEntity {
                kind: EntityKind::GitSha,
                value: cap[1].to_string(),
                context_key: format!("git:sha:{}", &cap[1]),
            });
        }

        // File paths
        for cap in regex_lite::Regex::new(r"(?:^|\s)([~/][\w./\-]+\.\w+)")
            .unwrap()
            .captures_iter(message)
        {
            entities.push(ExtractedEntity {
                kind: EntityKind::FilePath,
                value: cap[1].to_string(),
                context_key: format!("file:{}", &cap[1]),
            });
        }

        // GitHub repos (org/name)
        for cap in regex_lite::Regex::new(r"\b(\w+/[\w\-]+)\b")
            .unwrap()
            .captures_iter(message)
        {
            let val = &cap[1];
            if val.contains('/') && !val.starts_with('/') && !val.contains('.') {
                entities.push(ExtractedEntity {
                    kind: EntityKind::GitHubRepo,
                    value: val.to_string(),
                    context_key: format!("repo:{val}"),
                });
            }
        }

        // URLs
        for cap in regex_lite::Regex::new(r"https?://\S+")
            .unwrap()
            .captures_iter(message)
        {
            entities.push(ExtractedEntity {
                kind: EntityKind::Url,
                value: cap[0].to_string(),
                context_key: format!("url:{}", &cap[0]),
            });
        }

        entities
    }
}

#[derive(Debug, Clone)]
pub struct ExtractedEntity {
    pub kind: EntityKind,
    pub value: String,
    /// Key for looking up related context in PluresDB.
    pub context_key: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntityKind {
    AdoWorkItem,
    GitSha,
    FilePath,
    GitHubRepo,
    Url,
}

/// Context relevance scoring — trained from outcomes.
pub struct RelevanceScorer {
    /// Learned weights: context_key → relevance boost.
    weights: HashMap<String, f32>,
    /// Decay factor per turn of age.
    age_decay: f32,
}

impl Default for RelevanceScorer {
    fn default() -> Self {
        Self {
            weights: HashMap::new(),
            age_decay: 0.85, // 15% decay per turn
        }
    }
}

impl RelevanceScorer {
    /// Score a context item's relevance given the current message.
    pub fn score(
        &self,
        item: &ContextItem,
        query_similarity: f32,
        entities: &[ExtractedEntity],
    ) -> f32 {
        let mut score = query_similarity;

        // Boost if the item mentions any extracted entity
        for entity in entities {
            if item.content.contains(&entity.value) {
                score += 0.3;
            }
        }

        // Apply learned weight if we have one
        if let Some(&weight) = self.weights.get(&item.id) {
            score *= weight;
        }

        // Age decay
        score *= self.age_decay.powi(item.age_turns as i32);

        // Success/failure signal
        let total = (item.success_count + item.failure_count) as f32;
        if total > 0.0 {
            let success_rate = item.success_count as f32 / total;
            score *= 0.5 + 0.5 * success_rate; // 0.5x to 1.0x multiplier
        }

        score.clamp(0.0, 1.0)
    }

    /// Update weights based on outcome (called after conscious model responds).
    pub fn record_outcome(&mut self, context_items: &[ContextItem], success: bool) {
        for item in context_items {
            let weight = self.weights.entry(item.id.clone()).or_insert(1.0);
            if success {
                *weight = (*weight * 1.1).min(2.0); // boost on success
            } else {
                *weight = (*weight * 0.9).max(0.1); // decay on failure
            }
        }
    }
}

/// Manage the context window: decide what stays, what goes, what's added.
pub fn manage_context(
    current: &mut Vec<ContextItem>,
    recalled: Vec<ContextItem>,
    entities: &[ExtractedEntity],
    scorer: &RelevanceScorer,
    query_similarities: &HashMap<String, f32>,
    budget: usize,
) -> ManagedContext {
    // Age all existing items
    for item in current.iter_mut() {
        item.age_turns += 1;
    }

    // Add recalled items (dedup by id)
    let existing_ids: std::collections::HashSet<String> =
        current.iter().map(|i| i.id.clone()).collect();
    for item in recalled {
        if !existing_ids.contains(&item.id) {
            current.push(item);
        }
    }

    // Score everything
    for item in current.iter_mut() {
        let sim = query_similarities.get(&item.id).copied().unwrap_or(0.0);
        item.relevance = scorer.score(item, sim, entities);
    }

    // Sort by relevance (highest first)
    current.sort_by(|a, b| {
        b.relevance
            .partial_cmp(&a.relevance)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Fit within budget — remove lowest relevance items
    let mut tokens_used = 0;
    let mut kept = Vec::new();
    for item in current.drain(..) {
        if tokens_used + item.tokens <= budget {
            tokens_used += item.tokens;
            kept.push(item);
        }
        // Items that don't fit are dropped (can be re-recalled later)
    }

    *current = kept.clone();

    ManagedContext {
        items: kept,
        token_budget: budget,
        tokens_used,
        topic_shifted: false, // caller sets this from embedding comparison
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_extraction_ado_items() {
        let entities = EntityExtractor::extract("Close #2608832 and check #2605477");
        let ado: Vec<_> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::AdoWorkItem)
            .collect();
        assert_eq!(ado.len(), 2);
        assert_eq!(ado[0].value, "2608832");
        assert_eq!(ado[1].value, "2605477");
    }

    #[test]
    fn entity_extraction_git_shas() {
        let entities = EntityExtractor::extract("Check commit f6fcf65 on main");
        let shas: Vec<_> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::GitSha)
            .collect();
        assert!(shas.iter().any(|e| e.value == "f6fcf65"));
    }

    #[test]
    fn entity_extraction_file_paths() {
        let entities = EntityExtractor::extract("Edit ~/projects/pares-radix/src/main.rs");
        let paths: Vec<_> = entities
            .iter()
            .filter(|e| e.kind == EntityKind::FilePath)
            .collect();
        assert!(!paths.is_empty());
    }

    #[test]
    fn relevance_scorer_age_decay() {
        let scorer = RelevanceScorer::default();
        let fresh = ContextItem {
            id: "1".into(),
            content: "test".into(),
            tokens: 10,
            relevance: 0.0,
            source: ContextSource::Memory,
            age_turns: 0,
            success_count: 0,
            failure_count: 0,
        };
        let stale = ContextItem {
            id: "2".into(),
            content: "test".into(),
            tokens: 10,
            relevance: 0.0,
            source: ContextSource::Memory,
            age_turns: 5,
            success_count: 0,
            failure_count: 0,
        };
        let fresh_score = scorer.score(&fresh, 0.8, &[]);
        let stale_score = scorer.score(&stale, 0.8, &[]);
        assert!(
            fresh_score > stale_score,
            "fresh should score higher than stale"
        );
    }

    #[test]
    fn relevance_scorer_success_boost() {
        let scorer = RelevanceScorer::default();
        let successful = ContextItem {
            id: "1".into(),
            content: "test".into(),
            tokens: 10,
            relevance: 0.0,
            source: ContextSource::Memory,
            age_turns: 0,
            success_count: 5,
            failure_count: 0,
        };
        let failed = ContextItem {
            id: "2".into(),
            content: "test".into(),
            tokens: 10,
            relevance: 0.0,
            source: ContextSource::Memory,
            age_turns: 0,
            success_count: 0,
            failure_count: 5,
        };
        let s_score = scorer.score(&successful, 0.8, &[]);
        let f_score = scorer.score(&failed, 0.8, &[]);
        assert!(s_score > f_score, "successful context should score higher");
    }

    #[test]
    fn context_management_fits_budget() {
        let scorer = RelevanceScorer::default();
        let mut current = vec![
            ContextItem {
                id: "1".into(),
                content: "important".into(),
                tokens: 50,
                relevance: 0.0,
                source: ContextSource::Memory,
                age_turns: 0,
                success_count: 3,
                failure_count: 0,
            },
            ContextItem {
                id: "2".into(),
                content: "less important".into(),
                tokens: 50,
                relevance: 0.0,
                source: ContextSource::Memory,
                age_turns: 3,
                success_count: 0,
                failure_count: 2,
            },
        ];
        let sims: HashMap<String, f32> = [("1".into(), 0.9), ("2".into(), 0.3)].into();
        let result = manage_context(&mut current, vec![], &[], &scorer, &sims, 60);
        assert_eq!(result.items.len(), 1); // only room for one
        assert_eq!(result.items[0].id, "1"); // the relevant one
    }

    #[test]
    fn entity_boost_in_scoring() {
        let scorer = RelevanceScorer::default();
        let item = ContextItem {
            id: "1".into(),
            content: "Work item #2608832 status: Active".into(),
            tokens: 10,
            relevance: 0.0,
            source: ContextSource::Memory,
            age_turns: 0,
            success_count: 0,
            failure_count: 0,
        };
        let entities = vec![ExtractedEntity {
            kind: EntityKind::AdoWorkItem,
            value: "2608832".into(),
            context_key: "ado:workitem:2608832".into(),
        }];
        let with_entity = scorer.score(&item, 0.5, &entities);
        let without_entity = scorer.score(&item, 0.5, &[]);
        assert!(
            with_entity > without_entity,
            "entity match should boost score"
        );
    }
}
