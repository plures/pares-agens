use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use chrono::Utc;

/// Categories of Praxis coprocessor guidance displayed in the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuidanceCategory {
    /// Factual statements derived from memory.
    Facts,
    /// Operative rules the agent should follow.
    Rules,
    /// Hard constraints that must not be violated.
    Constraints,
    /// Recorded decisions and their rationale.
    Decisions,
    /// Identified risks and mitigations.
    Risks,
    /// General advisory guidance and recommendations.
    Guidance,
}

impl GuidanceCategory {
    /// Return a stable kebab-case string identifier for this category.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Rules => "rules", 
            Self::Constraints => "constraints",
            Self::Decisions => "decisions",
            Self::Risks => "risks", 
            Self::Guidance => "guidance",
        }
    }
}

/// A single guidance entry from the Praxis coprocessor analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuidanceEntry {
    /// Unique identifier for this guidance entry.
    pub id: String,
    /// Category of guidance (facts, rules, decisions, etc.).
    pub category: GuidanceCategory,
    /// Human-readable guidance content.
    pub content: String,
    /// Confidence score (0.0 to 1.0) for this guidance.
    pub confidence: f32,
    /// Source memory span IDs this guidance is derived from.
    pub source_spans: Vec<String>,
    /// Timestamp when this guidance was generated.
    pub generated_at: String,
    /// Priority level (1=highest, 5=lowest).
    pub priority: u8,
}

/// Source span information for traceability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceSpan {
    /// Memory entry ID containing this span.
    pub memory_id: String,
    /// Start character position in the memory content.
    pub start_pos: usize,
    /// End character position in the memory content.
    pub end_pos: usize,
    /// The actual text content of the span.
    pub text: String,
    /// Relevance score for this span to the guidance.
    pub relevance: f32,
}

/// Analysis event that triggers guidance updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisEvent {
    /// Event identifier.
    pub id: String,
    /// Event type (memory_updated, new_conversation, policy_change, etc.).
    pub event_type: String,
    /// Timestamp when the analysis completed.
    pub timestamp: String,
    /// Number of guidance entries updated by this analysis.
    pub guidance_updated: u32,
    /// Memory IDs that were analyzed.
    pub analyzed_memory_ids: Vec<String>,
}

/// Service for managing Praxis coprocessor guidance.
///
/// Provides an interface for storing, retrieving, and updating guidance
/// entries derived from PluresLM memory analysis.
#[derive(Clone)]
pub struct GuidanceService {
    /// All guidance entries indexed by ID.
    entries: Arc<Mutex<HashMap<String, GuidanceEntry>>>,
    /// Source span data indexed by span ID.
    spans: Arc<Mutex<HashMap<String, SourceSpan>>>,
    /// Recent analysis events.
    events: Arc<Mutex<Vec<AnalysisEvent>>>,
}

impl Default for GuidanceService {
    fn default() -> Self {
        Self::new()
    }
}

impl GuidanceService {
    /// Create a new guidance service.
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            spans: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(Vec::new())),
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
        self.entries.lock().unwrap().insert(id.clone(), entry);
        id
    }

    /// Get all guidance entries for a specific category.
    pub fn get_guidance(&self, category: &GuidanceCategory) -> Vec<GuidanceEntry> {
        let mut entries: Vec<_> = self
            .entries
            .lock()
            .unwrap()
            .values()
            .filter(|e| &e.category == category)
            .cloned()
            .collect();
        
        // Sort by priority (1=highest), then confidence (highest first)
        entries.sort_by(|a, b| {
            a.priority.cmp(&b.priority)
                .then_with(|| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal))
        });
        entries
    }

    /// Get all guidance entries.
    pub fn get_all_guidance(&self) -> Vec<GuidanceEntry> {
        self.entries.lock().unwrap().values().cloned().collect()
    }

    /// Add a source span for traceability.
    pub fn add_span(&self, span: SourceSpan) -> String {
        let id = Uuid::new_v4().to_string();
        self.spans.lock().unwrap().insert(id.clone(), span);
        id
    }

    /// Get source spans by their IDs.
    pub fn get_spans(&self, span_ids: &[String]) -> Vec<SourceSpan> {
        let spans = self.spans.lock().unwrap();
        span_ids
            .iter()
            .filter_map(|id| spans.get(id).cloned())
            .collect()
    }

    /// Record an analysis event.
    pub fn record_analysis_event(&self, event: AnalysisEvent) {
        let mut events = self.events.lock().unwrap();
        events.push(event);
        // Keep only last 50 events
        if events.len() > 50 {
            let len = events.len();
            events.drain(0..len - 50);
        }
    }

    /// Get recent analysis events.
    pub fn get_recent_events(&self, limit: usize) -> Vec<AnalysisEvent> {
        let events = self.events.lock().unwrap();
        events.iter().rev().take(limit).cloned().collect()
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
        self.entries.lock().unwrap().clear();
        self.spans.lock().unwrap().clear();
        self.events.lock().unwrap().clear();
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
        
        service.generate_guidance_from_memory("We decided to use Rust because it's memory safe", "mem-1");
        
        let decisions = service.get_guidance(&GuidanceCategory::Decisions);
        assert_eq!(decisions.len(), 1);
        assert!(decisions[0].content.contains("decision"));

        let events = service.get_recent_events(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "memory_analyzed");
    }
}