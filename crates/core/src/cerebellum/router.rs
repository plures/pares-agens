//! Routing logic — decides where an event goes after autorecall.

use super::{CerebellumConfig, Route};
use crate::delegation::broker::SubTask;
use pares_radix_core::event::Event;

/// Complexity signals extracted from an event.
struct Signals {
    /// Approximate token count of the user message.
    token_estimate: usize,
    /// Whether the message asks for analysis, reasoning, or planning.
    analytical: bool,
    /// Whether the message is a simple command or acknowledgement.
    simple: bool,
    /// Whether the message includes code or implementation intent.
    code: bool,
    /// Whether the message suggests research or external lookup.
    research: bool,
    /// Whether the request should be decomposed into sub-tasks.
    decomposable: bool,
    /// Whether the message is pure noise (bare ack with no actionable content).
    noise: bool,
    /// Whether the message is conversational/social (greetings, check-ins).
    conversational: bool,
}

/// Decide routing for an event.
pub fn decide(event: &Event, learned_context: &str, config: &CerebellumConfig) -> Route {
    match event {
        Event::Timer { .. } => Route::Procedural,
        Event::ToolResult { .. } => Route::Conscious,
        Event::ModelResponse { .. } => Route::Drop,
        Event::StateChange { .. } => Route::Procedural,
        Event::PreActionConstraint { .. } => Route::Drop,
        Event::Message { content, .. } => decide_message(content, learned_context, config),
        Event::ConstraintViolation { .. } => Route::Drop,
        Event::TaskDecompositionRequired { .. } => Route::Drop,
    }
}

fn decide_message(content: &str, learned_context: &str, config: &CerebellumConfig) -> Route {
    let signals = analyze(content);

    // Pure noise gets dropped
    if signals.noise {
        return Route::Drop;
    }

    // Conversational/social messages go straight to conscious — never delegate
    if signals.conversational && !signals.code {
        return Route::Conscious;
    }

    // Short commands go to conscious
    if signals.simple && !signals.analytical {
        return Route::Conscious;
    }

    if signals.decomposable {
        let tasks = build_subtasks(content, learned_context, &signals);
        if !tasks.is_empty() {
            return Route::Delegate {
                reason: "decomposition signals detected (multi-part or cross-domain request)"
                    .into(),
                tasks,
            };
        }
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
        "plan",
    ];
    let analytical = analytical_keywords.iter().any(|kw| lower.contains(kw));

    let code_keywords = [
        "code",
        "implement",
        "build",
        "create",
        "refactor",
        "fix",
        "compile",
        "cargo",
        "rust",
        "typescript",
        "javascript",
        "python",
        "module",
        "crate",
        "function",
    ];
    let code = code_keywords.iter().any(|kw| lower.contains(kw));

    let research_keywords = [
        "research",
        "search",
        "find",
        "look up",
        "documentation",
        "docs",
        "source",
        "citation",
        "paper",
        "web",
    ];
    let research = research_keywords.iter().any(|kw| lower.contains(kw));

    let noise_patterns = [
        "ok", "sure", "thanks", "got it", "k", "cool", "np", "ty", "thx", "okay", "alright",
    ];
    let noise = noise_patterns.iter().any(|p| lower.trim() == *p);

    // Conversational / social messages that should NOT be delegated or decomposed.
    let conversational_keywords = [
        "hello",
        "hi ",
        "hey ",
        "hey,",
        "howdy",
        "good morning",
        "good afternoon",
        "good evening",
        "what's up",
        "whats up",
        "sup",
        "yo ",
        "greetings",
        "say hello",
        "loud and clear",
        "nice to meet",
        "can you hear me",
        "are you there",
        "are you online",
        "you around",
        "checking in",
    ];
    let conversational = conversational_keywords.iter().any(|kw| lower.contains(kw))
        || lower.trim() == "hi"
        || lower.trim() == "hey"
        || lower.trim() == "yo";

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

    let mentions_multiple_files = count_file_mentions(&lower) >= 2;
    let multi_part_keywords = [
        "multiple",
        "components",
        "modules",
        "parts",
        "files",
        "directories",
        "across",
        "integrate",
        "wire",
    ];
    let multi_part_request = multi_part_keywords.iter().any(|kw| lower.contains(kw));
    let multi_verb_request = ["create", "build", "implement", "wire", "integrate"]
        .iter()
        .any(|kw| lower.contains(kw))
        && (lower.contains(" and ") || multi_part_request);

    let decomposable = token_estimate > 200
        || mentions_multiple_files
        || multi_verb_request
        || (analytical && code);

    Signals {
        token_estimate,
        analytical,
        simple,
        code,
        research,
        decomposable,
        noise,
        conversational,
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

fn count_file_mentions(lower: &str) -> usize {
    let extensions = [
        ".rs", ".ts", ".js", ".py", ".go", ".java", ".toml", ".json", ".yml", ".yaml", ".md",
    ];
    extensions.iter().filter(|ext| lower.contains(*ext)).count()
        + ["/src/", "crates/", "modules/"]
            .iter()
            .filter(|segment| lower.contains(*segment))
            .count()
}

fn build_subtasks(content: &str, learned_context: &str, signals: &Signals) -> Vec<SubTask> {
    let mut tasks = Vec::new();
    let mut context = String::new();
    if !learned_context.trim().is_empty() {
        context = learned_context.trim().to_string();
    }

    if signals.analytical {
        let mut task = SubTask::new(
            "analyst",
            format!("Analyze and outline the plan for: {content}"),
        );
        if !context.is_empty() {
            task = task.with_parent_context(context.clone());
        }
        tasks.push(task);
    }

    if signals.code {
        let mut task = SubTask::new("coder", format!("Implement or patch: {content}"));
        if !context.is_empty() {
            task = task.with_parent_context(context.clone());
        }
        tasks.push(task);
    }

    if signals.research {
        let mut task = SubTask::new(
            "researcher",
            format!("Research supporting details for: {content}"),
        );
        if !context.is_empty() {
            task = task.with_parent_context(context.clone());
        }
        tasks.push(task);
    }

    if tasks.is_empty() && signals.decomposable {
        let mut task = SubTask::new(
            "analyst",
            format!("Decompose this request into actionable steps: {content}"),
        );
        if !context.is_empty() {
            task = task.with_parent_context(context);
        }
        tasks.push(task);
    }

    tasks
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
    fn multi_part_request_routes_delegate() {
        let event = Event::Message {
            id: "1".into(),
            channel: "c".into(),
            sender: "u".into(),
            content: "Implement updates in src/main.rs and src/lib.rs, then analyze the design trade-offs for the new module".into(),
        };
        let route = decide(&event, "", &config());
        assert!(matches!(route, Route::Delegate { .. }));
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
