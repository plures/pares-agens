use std::sync::{Arc, Mutex};

use chrono::Utc;
use uuid::Uuid;

pub use pares_agens_praxis::db::guidance::{
    AnalysisEvent, GuidanceCategory, GuidanceEntry, GuidanceStore, SourceSpan,
};

/// Service for managing Praxis coprocessor guidance.
///
/// Provides an interface for storing, retrieving, and updating guidance
/// entries derived from PluresLM memory analysis.  Internally all data is
/// persisted in a [`GuidanceStore`] (the PluresDB-backed storage layer)
/// wrapped in an `Arc<Mutex<…>>` for safe shared access.
#[derive(Clone)]
pub struct GuidanceService {
    store: Arc<Mutex<GuidanceStore>>,
}

impl Default for GuidanceService {
    fn default() -> Self {
        Self::new()
    }
}

impl GuidanceService {
    /// Create a new guidance service backed by an empty [`GuidanceStore`].
    pub fn new() -> Self {
        Self {
            store: Arc::new(Mutex::new(GuidanceStore::new())),
        }
    }

    /// Add a guidance entry to the service.
    pub fn add_guidance(&self, mut entry: GuidanceEntry) -> String {
        if entry.id.is_empty() {
            entry.id = Uuid::new_v4().to_string();
        }
        if entry.generated_at.is_empty() {
            entry.generated_at = Utc::now().to_rfc3339();
        }
        let id = entry.id.clone();
        self.store.lock().unwrap().upsert_entry(entry);
        id
    }

    /// Get all guidance entries for a specific category.
    pub fn get_guidance(&self, category: &GuidanceCategory) -> Vec<GuidanceEntry> {
        self.store
            .lock()
            .unwrap()
            .entries_by_category(category)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Get all guidance entries.
    pub fn get_all_guidance(&self) -> Vec<GuidanceEntry> {
        self.store
            .lock()
            .unwrap()
            .all_entries()
            .into_iter()
            .cloned()
            .collect()
    }

    /// Add a source span for traceability.
    ///
    /// If `span.id` is empty a new UUID is auto-assigned; otherwise the
    /// caller-supplied ID is used (upsert semantics — an existing span with
    /// the same ID will be replaced).  Returns the final span ID.
    pub fn add_span(&self, mut span: SourceSpan) -> String {
        if span.id.is_empty() {
            span.id = Uuid::new_v4().to_string();
        }
        let id = span.id.clone();
        self.store.lock().unwrap().upsert_span(span);
        id
    }

    /// Get source spans by their IDs.
    pub fn get_spans(&self, span_ids: &[String]) -> Vec<SourceSpan> {
        self.store
            .lock()
            .unwrap()
            .spans_by_ids(span_ids)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Record an analysis event.
    pub fn record_analysis_event(&self, event: AnalysisEvent) {
        self.store.lock().unwrap().push_event(event);
    }

    /// Get recent analysis events.
    pub fn get_recent_events(&self, limit: usize) -> Vec<AnalysisEvent> {
        self.store
            .lock()
            .unwrap()
            .recent_events(limit)
            .into_iter()
            .cloned()
            .collect()
    }

    /// Simulate generating guidance from memory content.
    ///
    /// This is a placeholder implementation. In production, this would:
    /// 1. Connect to PluresLM for memory analysis
    /// 2. Run AI analysis to extract facts, decisions, risks, etc.
    /// 3. Generate guidance entries with proper source traceability
    pub fn generate_guidance_from_memory(&self, memory_content: &str, memory_id: &str) {
        // Simple heuristic analysis for demonstration
        if memory_content.to_lowercase().contains("error") || memory_content.contains("bug") {
            let entry = GuidanceEntry {
                id: String::new(), // Will be auto-generated
                category: GuidanceCategory::Risks,
                content: "Potential error condition detected in recent conversation".to_string(),
                confidence: 0.7,
                source_spans: vec![memory_id.to_string()],
                generated_at: String::new(), // Will be auto-generated
                priority: 2,
            };
            self.add_guidance(entry);
        }

        if memory_content.to_lowercase().contains("decided") || memory_content.contains("because") {
            let entry = GuidanceEntry {
                id: String::new(),
                category: GuidanceCategory::Decisions,
                content: "New decision context recorded".to_string(),
                confidence: 0.8,
                source_spans: vec![memory_id.to_string()],
                generated_at: String::new(),
                priority: 1,
            };
            self.add_guidance(entry);
        }

        if memory_content.to_lowercase().contains("always") || memory_content.contains("never") {
            let entry = GuidanceEntry {
                id: String::new(),
                category: GuidanceCategory::Rules,
                content: "Policy constraint identified".to_string(),
                confidence: 0.9,
                source_spans: vec![memory_id.to_string()],
                generated_at: String::new(),
                priority: 1,
            };
            self.add_guidance(entry);
        }

        // Record the analysis event
        let event = AnalysisEvent {
            id: Uuid::new_v4().to_string(),
            event_type: "memory_analyzed".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            guidance_updated: 1,
            analyzed_memory_ids: vec![memory_id.to_string()],
        };
        self.record_analysis_event(event);
    }

    /// Clear all guidance entries (for testing/reset).
    pub fn clear(&self) {
        self.store.lock().unwrap().clear();
    }

    /// Inject a guidance entry derived from a user correction.
    ///
    /// The entry is created with the [`GuidanceCategory::Rules`] category,
    /// confidence 1.0, and priority 1 (highest) so it surfaces prominently in
    /// future contexts.  The `correction_id` is recorded as a source span for
    /// traceability.
    ///
    /// Returns the guidance entry ID.
    pub fn inject_correction_guidance(&self, rule_summary: &str, correction_id: &str) -> String {
        let entry = GuidanceEntry {
            id: String::new(), // auto-generated
            category: GuidanceCategory::Rules,
            content: rule_summary.to_string(),
            confidence: 1.0,
            source_spans: vec![correction_id.to_string()],
            generated_at: String::new(), // auto-generated
            priority: 1,
        };
        let id = self.add_guidance(entry);

        let event = AnalysisEvent {
            id: Uuid::new_v4().to_string(),
            event_type: "correction_applied".to_string(),
            timestamp: Utc::now().to_rfc3339(),
            guidance_updated: 1,
            analyzed_memory_ids: vec![correction_id.to_string()],
        };
        self.record_analysis_event(event);

        id
    }

    /// Remove a guidance entry by ID.  Returns `true` if the entry existed.
    pub fn remove_guidance(&self, guidance_id: &str) -> bool {
        self.store.lock().unwrap().remove_entry(guidance_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guidance_service_basic_operations() {
        let service = GuidanceService::new();

        let entry = GuidanceEntry {
            id: "test-1".to_string(),
            category: GuidanceCategory::Facts,
            content: "Test fact".to_string(),
            confidence: 0.9,
            source_spans: vec!["span-1".to_string()],
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            priority: 1,
        };

        let id = service.add_guidance(entry.clone());
        assert_eq!(id, "test-1");

        let facts = service.get_guidance(&GuidanceCategory::Facts);
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].content, "Test fact");

        let rules = service.get_guidance(&GuidanceCategory::Rules);
        assert_eq!(rules.len(), 0);
    }

    #[test]
    fn guidance_sorting_by_priority_and_confidence() {
        let service = GuidanceService::new();

        // Add entries with different priorities and confidence
        service.add_guidance(GuidanceEntry {
            id: "low-pri".to_string(),
            category: GuidanceCategory::Facts,
            content: "Low priority".to_string(),
            confidence: 0.9,
            source_spans: vec![],
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            priority: 3,
        });

        service.add_guidance(GuidanceEntry {
            id: "high-pri".to_string(),
            category: GuidanceCategory::Facts,
            content: "High priority".to_string(),
            confidence: 0.7,
            source_spans: vec![],
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            priority: 1,
        });

        let facts = service.get_guidance(&GuidanceCategory::Facts);
        assert_eq!(facts[0].content, "High priority"); // Priority 1 comes first
        assert_eq!(facts[1].content, "Low priority");
    }

    #[test]
    fn generate_guidance_from_memory_detects_patterns() {
        let service = GuidanceService::new();

        service.generate_guidance_from_memory(
            "We decided to use Rust because it's memory safe",
            "mem-1",
        );

        let decisions = service.get_guidance(&GuidanceCategory::Decisions);
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].content.contains("decision"));

        let events = service.get_recent_events(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "memory_analyzed");
    }

    #[test]
    fn inject_correction_guidance_creates_rule() {
        let service = GuidanceService::new();

        let id =
            service.inject_correction_guidance("avoid: use unwrap in production", "correction-123");

        let rules = service.get_guidance(&GuidanceCategory::Rules);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].content, "avoid: use unwrap in production");
        assert_eq!(rules[0].confidence, 1.0);
        assert_eq!(rules[0].priority, 1);
        assert_eq!(rules[0].source_spans, vec!["correction-123".to_string()]);

        let events = service.get_recent_events(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "correction_applied");
        assert!(!id.is_empty());
    }

    #[test]
    fn remove_guidance_removes_entry() {
        let service = GuidanceService::new();

        let id = service.inject_correction_guidance("some rule", "corr-1");
        assert_eq!(service.get_all_guidance().len(), 1);

        assert!(service.remove_guidance(&id));
        assert!(service.get_all_guidance().is_empty());
    }

    #[test]
    fn remove_guidance_nonexistent_returns_false() {
        let service = GuidanceService::new();
        assert!(!service.remove_guidance("nonexistent"));
    }
}
