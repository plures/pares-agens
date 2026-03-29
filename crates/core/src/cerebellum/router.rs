//! Routing logic — decides where an event goes after autorecall.

use super::{CerebellumConfig, Route};
use crate::event::Event;

/// Complexity signals extracted from an event.
struct Signals {
    /// Approximate token count of the user message.
    token_estimate: usize,
    /// Whether the message asks for analysis, reasoning, or planning.
    analytical: bool,
    /// Whether the message is a simple command or acknowledgement.
    simple: bool,
}

/// Decide routing for an event.
pub fn decide(event: &Event, learned_context: &str, config: &CerebellumConfig) -> Route {
    match event {
        Event::Timer { .. } => Route::Procedural,
        Event::ToolResult { .. } => Route::Conscious,
        Event::ModelResponse { .. } => Route::Drop,
        Event::StateChange { .. } => Route::Procedural,
        Event::Message { content, .. } => decide_message(content, learned_context, config),
        Event::ConstraintViolation { .. } => Route::Drop,
    }
}

fn decide_message(content: &str, _learned_context: &str, config: &CerebellumConfig) -> Route {
    let signals = analyze(content);

    // Drop noise (exact single-word acks like "ok", "yes", "no")
    if signals.simple && signals.token_estimate == 1 {
        return Route::Drop;
    }

    // Short commands go to conscious
    if signals.simple && !signals.analytical {
        return Route::Conscious;
    }

    // Deep reasoning path
    if config.enable_subconscious && signals.analytical {
        let complexity = estimate_complexity(&signals);
        if complexity >= config.complexity_threshold {
            return Route::Deep {
                reason: "analytical query exceeds complexity threshold".into(),
            };
        }
    }

    Route::Conscious
}

fn analyze(content: &str) -> Signals {
    let lower = content.to_lowercase();
    let token_estimate = content.split_whitespace().count();

    let analytical_keywords = [
        "analyze",
        "explain",
        "compare",
        "design",
        "architect",
        "why",
        "how does",
        "trade-off",
        "tradeoff",
        "evaluate",
        "reason",
        "think through",
        "deep dive",
        "investigate",
    ];
    let analytical = analytical_keywords.iter().any(|kw| lower.contains(kw));

    let simple_patterns = [
        "yes",
        "no",
        "ok",
        "sure",
        "thanks",
        "got it",
        "do it",
        "push",
        "run",
        "status",
        "heartbeat",
    ];
    let simple =
        simple_patterns.iter().any(|p| lower.trim() == *p) || (token_estimate <= 3 && !analytical);

    Signals {
        token_estimate,
        analytical,
        simple,
    }
}

fn estimate_complexity(signals: &Signals) -> f32 {
    let mut score: f32 = 0.0;

    // Length contributes
    if signals.token_estimate > 50 {
        score += 0.3;
    } else if signals.token_estimate > 10 {
        score += 0.15;
    }

    // Analytical intent is a strong signal
    if signals.analytical {
        score += 0.6;
    }

    score.min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> CerebellumConfig {
        CerebellumConfig::default()
    }

    #[test]
    fn timer_routes_procedural() {
        let event = Event::Timer {
            id: "t".into(),
            name: "sweep".into(),
            recurring: true,
        };
        assert_eq!(decide(&event, "", &config()), Route::Procedural);
    }

    #[test]
    fn simple_message_routes_conscious() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "push now".into(),
        };
        assert_eq!(decide(&event, "", &config()), Route::Conscious);
    }

    #[test]
    fn analytical_message_routes_deep() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "Analyze the trade-offs between CRDT conflict resolution strategies and explain which is best for our use case".into(),
        };
        let route = decide(&event, "", &config());
        assert!(matches!(route, Route::Deep { .. }));
    }

    #[test]
    fn noise_message_drops() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "ok".into(),
        };
        assert_eq!(decide(&event, "", &config()), Route::Drop);
    }

    #[test]
    fn subconscious_disabled_forces_conscious() {
        let mut cfg = config();
        cfg.enable_subconscious = false;
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "Analyze the architecture deeply and explain why".into(),
        };
        assert_eq!(decide(&event, "", &cfg), Route::Conscious);
    }
}
