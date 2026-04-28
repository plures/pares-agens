//! Dynamic system prompt builder.
//!
//! Assembles the system prompt at runtime from a [`PersonalityContract`],
//! channel context, recalled memory, and tool descriptions — replacing the
//! flat system prompt file as the primary source.

use crate::personality::{BehaviorRule, PersonalityContract};

/// Context passed to the prompt builder for dynamic assembly.
pub struct AgentContext<'a> {
    /// Current channel name (e.g. "telegram", "discord").
    pub channel: Option<&'a str>,
    /// Recalled memory context from PluresLM / cerebellum.
    pub learned_context: &'a str,
    /// Recent conversation summary (optional).
    pub conversation_summary: Option<&'a str>,
    /// Whether this is a deep/escalated reasoning call.
    pub deep: bool,
    /// Personality documents (SOUL.md, IDENTITY.md, etc.) loaded from PluresDB.
    pub personality_documents: Option<&'a str>,
    /// Plugin schema context (installed plugins, entities, tools).
    pub plugin_context: Option<&'a str>,
}

/// Build a complete system prompt from a personality contract and context.
///
/// Sections (in order):
/// 1. Deep-thinking preamble (if `context.deep`)
/// 2. Base identity (name, description, tone)
/// 3. Core behavioral rules sorted by priority (highest first)
/// 4. Channel-specific overrides
/// 5. Recalled memory context
/// 6. Conversation summary
pub fn build_system_prompt(personality: &PersonalityContract, context: &AgentContext<'_>) -> String {
    let mut prompt = String::with_capacity(2048);

    // Deep preamble
    if context.deep {
        prompt.push_str("Think deeply about this. Analyze thoroughly.\n\n");
    }

    // Personality documents (SOUL.md, IDENTITY.md, etc.) — injected first
    if let Some(docs) = context.personality_documents {
        if !docs.trim().is_empty() {
            prompt.push_str(docs.trim());
            prompt.push_str("\n\n");
        }
    }

    // Identity
    prompt.push_str(&format!(
        "You are {}, {}.\nTone: {}.\n",
        personality.name, personality.description, personality.tone
    ));

    // Core rules
    let mut sorted_rules: Vec<&BehaviorRule> = personality.rules.iter().collect();
    sorted_rules.sort_by(|a, b| b.priority.cmp(&a.priority));

    if !sorted_rules.is_empty() {
        prompt.push_str("\n## Behavioral Rules\n");
        for rule in &sorted_rules {
            let prefix = if rule.enforced { "MUST" } else { "SHOULD" };
            prompt.push_str(&format!("- {prefix}: {}\n", rule.rule));
        }
    }

    // Channel overrides
    if let Some(channel) = context.channel {
        if let Some(overrides) = personality.channel_overrides.get(channel) {
            if !overrides.is_empty() {
                let mut sorted_overrides: Vec<&BehaviorRule> = overrides.iter().collect();
                sorted_overrides.sort_by(|a, b| b.priority.cmp(&a.priority));
                prompt.push_str(&format!("\n## Channel Rules ({})\n", channel));
                for rule in &sorted_overrides {
                    let prefix = if rule.enforced { "MUST" } else { "SHOULD" };
                    prompt.push_str(&format!("- {prefix}: {}\n", rule.rule));
                }
            }
        }
    }

    // Recalled context
    if !context.learned_context.trim().is_empty() {
        prompt.push_str("\n## Recalled Context\n");
        prompt.push_str(context.learned_context.trim());
        prompt.push('\n');
    }

    // Conversation summary
    if let Some(summary) = context.conversation_summary {
        if !summary.trim().is_empty() {
            prompt.push_str("\n## Conversation Context\n");
            prompt.push_str(summary.trim());
            prompt.push('\n');
        }
    }

    // Plugin schema context
    if let Some(plugin_ctx) = context.plugin_context {
        if !plugin_ctx.trim().is_empty() {
            prompt.push_str(plugin_ctx.trim());
            prompt.push('\n');
        }
    }

    prompt
}

/// Build a system prompt from a flat file fallback (legacy path).
/// Returns the file contents or a built-in default.
pub fn build_system_prompt_from_file(path: Option<&std::path::Path>) -> Result<String, String> {
    if let Some(path) = path {
        return std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read system prompt {}: {e}", path.display()));
    }

    if let Ok(home) = std::env::var("HOME") {
        let home_prompt = std::path::PathBuf::from(&home).join(".pares-agens/SYSTEM-PROMPT.md");
        if home_prompt.exists() {
            tracing::info!("Loading system prompt from {}", home_prompt.display());
            return std::fs::read_to_string(&home_prompt)
                .map_err(|e| format!("failed to read {}: {e}", home_prompt.display()));
        }
    }

    Ok("You are Pares Agens, an AI agent built on the plures technology stack. Be direct, use tools proactively, and push commits without asking.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_prompt_with_identity_and_rules() {
        let contract = PersonalityContract::default_contract(None);
        let ctx = AgentContext {
            channel: Some("telegram"),
            learned_context: "User prefers concise answers.",
            conversation_summary: None,
            deep: false,
            personality_documents: None,
            plugin_context: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(prompt.contains("Pares Agens"));
        assert!(prompt.contains("MUST: Never share private data"));
        assert!(prompt.contains("Channel Rules (telegram)"));
        assert!(prompt.contains("Recalled Context"));
    }

    #[test]
    fn deep_adds_preamble() {
        let contract = PersonalityContract::default_contract(None);
        let ctx = AgentContext {
            channel: None,
            learned_context: "",
            conversation_summary: None,
            deep: true,
            personality_documents: None,
            plugin_context: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(prompt.starts_with("Think deeply"));
    }
}
