//! Message classifier for the orchestrator preprocessing stage.
//!
//! Provides both heuristic (zero-cost, no model) and model-backed
//! classification of inbound messages. The [`ClassifierBackend`] trait
//! allows any LLM (BitNet, OpenAI, etc.) to be injected at the CLI level
//! without core depending on any inference crate.

use std::sync::{Arc, Mutex};

// ── classifier contract (single source of truth) ─────────────────────────────
//
// Stage S-B/S-C: the classification DTOs (`MessageClassification`,
// `MessageIntent`), the `ClassifierBackend` trait, and `CLASSIFIER_SYSTEM_PROMPT`
// now live in `pares-radix-core` as the platform seam. This cognition module
// owns only the higher-level `CerebellumClassifier` orchestrator and re-uses the
// single canonical contract definition (no local redefinition).
pub use pares_radix_core::classifier::{
    ClassifierBackend, MessageClassification, MessageIntent, CLASSIFIER_SYSTEM_PROMPT,
};

// ── classifier ───────────────────────────────────────────────────────────────

/// Message classifier combining optional model inference with heuristic
/// fallback.
pub struct CerebellumClassifier {
    backend: Option<Arc<dyn ClassifierBackend>>,
    last_topic: Mutex<Option<String>>,
    plugin_names: Vec<String>,
}

impl CerebellumClassifier {
    /// Create a classifier that uses only heuristic rules (no model).
    pub fn heuristic_only(plugin_names: Vec<String>) -> Self {
        Self {
            backend: None,
            last_topic: Mutex::new(None),
            plugin_names,
        }
    }

    /// Create a classifier with a model backend (falls back to heuristic on
    /// model failure).
    pub fn with_backend(backend: Arc<dyn ClassifierBackend>, plugin_names: Vec<String>) -> Self {
        Self {
            backend: Some(backend),
            last_topic: Mutex::new(None),
            plugin_names,
        }
    }

    /// Classify a user message.
    ///
    /// Tries the model backend first (if present), falling back to heuristic
    /// classification on any error or parse failure.
    pub fn classify(&self, message: &str) -> MessageClassification {
        if let Some(backend) = &self.backend {
            if let Ok(json_str) = backend.classify(CLASSIFIER_SYSTEM_PROMPT, message) {
                if let Ok(parsed) = serde_json::from_str::<MessageClassification>(&json_str) {
                    *self.last_topic.lock().unwrap() = Some(parsed.topic.clone());
                    return parsed;
                }
            }
        }
        self.classify_heuristic(message)
    }

    /// Pure keyword/rule-based classification — zero external dependencies.
    fn classify_heuristic(&self, message: &str) -> MessageClassification {
        let lower = message.to_lowercase();
        let lower = lower.trim();
        let words: Vec<&str> = message.split_whitespace().collect();
        let word_count = words.len();

        // ── Intent ───────────────────────────────────────────────────────
        let intent = if lower.starts_with('/') {
            MessageIntent::Command
        } else if lower.ends_with('?') || QUESTION_STARTERS.iter().any(|w| lower.starts_with(w)) {
            MessageIntent::Question
        } else if FEEDBACK_STARTERS.iter().any(|w| lower.starts_with(w)) {
            MessageIntent::Feedback
        } else if TASK_KEYWORDS.iter().any(|w| lower.contains(w)) {
            MessageIntent::Task
        } else {
            MessageIntent::Chat
        };

        // ── Complexity ───────────────────────────────────────────────────
        let complexity: u8 = match word_count {
            0..=5 => 1,
            6..=15 => 2,
            16..=30 => 3,
            31..=60 => 4,
            _ => 5,
        };

        // ── Tool detection ───────────────────────────────────────────────
        let needs_tools = TOOL_KEYWORDS.iter().any(|w| lower.contains(w));

        // ── Plugin matching ──────────────────────────────────────────────
        let plugin_match = self
            .plugin_names
            .iter()
            .find(|p| lower.contains(&p.replace('-', " ")))
            .cloned();

        // ── Topic extraction ─────────────────────────────────────────────
        let topic_words: Vec<&str> = words
            .iter()
            .filter(|w| !STOPWORDS.contains(&w.to_lowercase().as_str()) && w.len() > 2)
            .take(3)
            .copied()
            .collect();
        let topic = topic_words.join(" ");

        // ── Topic shift detection ────────────────────────────────────────
        let topic_shift = {
            let mut last = self.last_topic.lock().unwrap();
            let shifted = last
                .as_ref()
                .map(|lt| !topic.is_empty() && !lt.contains(&topic) && !topic.contains(lt.as_str()))
                .unwrap_or(false);
            *last = Some(topic.clone());
            shifted
        };

        MessageClassification {
            intent: intent.clone(),
            complexity,
            topic,
            topic_shift,
            entities: vec![],
            plugin_match,
            completion_hint: if intent == MessageIntent::Task {
                Some("task completed successfully".into())
            } else {
                None
            },
            needs_tools,
            needs_deep_model: complexity >= 4,
        }
    }
}

// ── word lists ───────────────────────────────────────────────────────────────

const QUESTION_STARTERS: &[&str] = &[
    "what", "how", "why", "when", "where", "who", "is", "are", "can", "do", "does", "will",
    "would", "should", "could",
];

const FEEDBACK_STARTERS: &[&str] = &[
    "ok", "yes", "no", "thanks", "thank", "good", "great", "perfect", "done", "approved", "lgtm",
    "nice", "cool", "sure", "agree", "correct", "right", "wrong", "nope",
];

const TASK_KEYWORDS: &[&str] = &[
    "build",
    "create",
    "fix",
    "implement",
    "deploy",
    "set up",
    "install",
    "configure",
    "write",
    "add",
    "remove",
    "delete",
    "update",
    "change",
    "move",
    "migrate",
    "refactor",
    "test",
    "review",
    "check",
    "scan",
    "index",
    "push",
    "pull",
    "commit",
    "merge",
];

const TOOL_KEYWORDS: &[&str] = &[
    "file",
    "run",
    "command",
    "search",
    "code",
    "git",
    "build",
    "deploy",
    "install",
    "read",
    "write",
    "edit",
    "list",
    "directory",
    "ls",
    "cat",
    "grep",
];

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "to", "of", "in", "for", "on",
    "with", "at", "by", "this", "that", "it", "my", "your", "i", "you", "we", "he", "she", "they",
    "me", "us", "and", "or", "but", "not", "can", "do", "does", "will", "would", "should", "could",
    "please", "just", "also",
];

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_classifier() -> CerebellumClassifier {
        CerebellumClassifier::heuristic_only(vec!["weather-plugin".into(), "code-search".into()])
    }

    #[test]
    fn question_detection() {
        let c = make_classifier();
        let r = c.classify("what is the meaning of life?");
        assert_eq!(r.intent, MessageIntent::Question);

        let r = c.classify("how do I deploy this?");
        assert_eq!(r.intent, MessageIntent::Question);
    }

    #[test]
    fn command_detection() {
        let c = make_classifier();
        let r = c.classify("/status");
        assert_eq!(r.intent, MessageIntent::Command);

        let r = c.classify("/deploy production");
        assert_eq!(r.intent, MessageIntent::Command);
    }

    #[test]
    fn task_detection() {
        let c = make_classifier();
        let r = c.classify("build the API endpoint for user profiles");
        assert_eq!(r.intent, MessageIntent::Task);
        assert!(r.completion_hint.is_some());
    }

    #[test]
    fn chat_detection() {
        let c = make_classifier();
        let r = c.classify("hello there");
        assert_eq!(r.intent, MessageIntent::Chat);
    }

    #[test]
    fn feedback_detection() {
        let c = make_classifier();
        let r = c.classify("ok thanks");
        assert_eq!(r.intent, MessageIntent::Feedback);

        let r = c.classify("lgtm");
        assert_eq!(r.intent, MessageIntent::Feedback);
    }

    #[test]
    fn plugin_name_matching() {
        let c = make_classifier();
        let r = c.classify("check the weather plugin status");
        assert_eq!(r.plugin_match, Some("weather-plugin".into()));
    }

    #[test]
    fn topic_shift_detection() {
        let c = make_classifier();
        let r1 = c.classify("tell me about rust programming");
        assert!(!r1.topic_shift); // first message, no prior topic

        let r2 = c.classify("what's for dinner tonight?");
        assert!(r2.topic_shift); // completely different topic
    }

    #[test]
    fn complexity_scoring() {
        let c = make_classifier();

        let r = c.classify("hello");
        assert_eq!(r.complexity, 1);

        let r = c.classify(
            "can you help me understand how the deployment pipeline works in our CI system",
        );
        assert!(r.complexity >= 2);

        // Very long message
        let long = (0..70)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let r = c.classify(&long);
        assert_eq!(r.complexity, 5);
    }

    #[test]
    fn tool_detection() {
        let c = make_classifier();
        let r = c.classify("read the file and search for errors");
        assert!(r.needs_tools);

        let r = c.classify("hello there");
        assert!(!r.needs_tools);
    }

    #[test]
    fn model_json_parsing() {
        // Simulate a model backend that returns valid JSON
        struct MockBackend;
        impl ClassifierBackend for MockBackend {
            fn classify(
                &self,
                _system_prompt: &str,
                _user_message: &str,
            ) -> Result<String, String> {
                Ok(r#"{"intent":"Task","complexity":3,"topic":"deploy API","topic_shift":false,"entities":["API","production"],"plugin_match":null,"completion_hint":"deployment complete","needs_tools":true,"needs_deep_model":false}"#.into())
            }
        }

        let c = CerebellumClassifier::with_backend(Arc::new(MockBackend), vec![]);
        let r = c.classify("deploy the API to production");
        assert_eq!(r.intent, MessageIntent::Task);
        assert_eq!(r.complexity, 3);
        assert_eq!(r.topic, "deploy API");
        assert_eq!(r.entities, vec!["API", "production"]);
        assert!(r.needs_tools);
    }

    #[test]
    fn model_fallback_on_invalid_json() {
        struct BadBackend;
        impl ClassifierBackend for BadBackend {
            fn classify(&self, _: &str, _: &str) -> Result<String, String> {
                Ok("not valid json".into())
            }
        }

        let c = CerebellumClassifier::with_backend(Arc::new(BadBackend), vec![]);
        let r = c.classify("what is rust?");
        // Should fall back to heuristic
        assert_eq!(r.intent, MessageIntent::Question);
    }

    #[test]
    fn model_fallback_on_error() {
        struct ErrorBackend;
        impl ClassifierBackend for ErrorBackend {
            fn classify(&self, _: &str, _: &str) -> Result<String, String> {
                Err("model unavailable".into())
            }
        }

        let c = CerebellumClassifier::with_backend(Arc::new(ErrorBackend), vec![]);
        let r = c.classify("/status");
        assert_eq!(r.intent, MessageIntent::Command);
    }
}
