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
    /// Pre-rendered `<available_skills>` catalog block (runtime skill discovery).
    /// Built by the caller from the live-skills dir via
    /// `pares_agens_marketplace::skills_catalog`; the model reads a chosen
    /// `SKILL.md` on demand through the `read_file` tool. Empty when none.
    pub skills_catalog: Option<&'a str>,
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
pub fn build_system_prompt(
    personality: &PersonalityContract,
    context: &AgentContext<'_>,
) -> String {
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
    sorted_rules.sort_by_key(|r| std::cmp::Reverse(r.priority));

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
                sorted_overrides.sort_by_key(|o| std::cmp::Reverse(o.priority));
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

    // Response formatting — suppress internal monologue
    prompt.push_str("\n## Response Style\n");
    prompt
        .push_str("- Do NOT narrate your thinking process, reasoning steps, or decision-making.\n");
    prompt.push_str("- Do NOT explain what tools you are about to call or why.\n");
    prompt.push_str(
        "- Do NOT say 'Let me...', 'I\'ll...', 'First, I need to...', or similar preambles.\n",
    );
    prompt.push_str("- When using tools: just call them. Report the result, not the process.\n");
    prompt.push_str("- Keep responses concise and direct. Lead with the answer.\n");
    prompt.push_str(
        "- If a task requires multiple steps, do them silently and report the outcome.\n",
    );
    prompt.push_str(
        "- Only explain your process if the user explicitly asks how you did something.\n",
    );

    // Execution bias — the agent ACTS; it does not answer a work directive with a
    // plan-and-yield. This mirrors the behavior a capable agent harness enforces:
    // when tools can move the task forward, use them now instead of proposing steps
    // and asking for confirmation. Without this, models tend to reply to an
    // actionable request with a structured plan ("here's how I'll approach...",
    // "please specify...") and stop — the loop then terminates on that text-only
    // completion, so no work happens.
    prompt.push_str("\n## Execution\n");
    prompt.push_str(
        "- When given an actionable request, DO IT this turn using your tools. Do not reply with a plan and wait for approval.\n",
    );
    prompt.push_str(
        "- Never end a turn with a proposal, a promise to do it, or a question like 'shall I proceed?' / 'let me know if...' / 'please specify...' when a tool call can advance the work. Make a reasonable choice and execute.\n",
    );
    prompt.push_str(
        "- A plan is not a deliverable. If you can run a tool, run it. Only stop to ask when a genuine blocker requires a human decision (destructive/irreversible action, an external side-effect needing permission, or missing information no tool can obtain).\n",
    );
    prompt.push_str(
        "- Continue until the task is done or truly blocked. If a step fails, vary the approach and retry rather than giving up and reporting the plan.\n",
    );
    prompt.push_str(
        "- For multi-step work, decompose it and carry it out; report the outcome, not the intention.\n",
    );

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

    // Available skills catalog (runtime skill discovery) — advertises installed
    // SKILL.md files so the model can load one on demand via read_file.
    if let Some(skills) = context.skills_catalog {
        if !skills.trim().is_empty() {
            prompt.push_str("\n## Skills\n");
            prompt.push_str("Installed skills are listed below. When a task matches one, read its SKILL.md at the given <location> with the read_file tool, then follow it.\n\n");
            prompt.push_str(skills.trim());
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
        let home_prompt = std::path::PathBuf::from(&home).join(".pares-radix/SYSTEM-PROMPT.md");
        if home_prompt.exists() {
            tracing::info!("Loading system prompt from {}", home_prompt.display());
            return std::fs::read_to_string(&home_prompt)
                .map_err(|e| format!("failed to read {}: {e}", home_prompt.display()));
        }
    }

    Ok("You are Pares Radix, an AI agent built on the plures technology stack. Be direct, use tools proactively, and push commits without asking.".to_string())
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
            skills_catalog: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(prompt.contains("Pares Radix"));
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
            skills_catalog: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(prompt.starts_with("Think deeply"));
    }

    #[test]
    fn prompt_includes_response_style_section() {
        let contract = PersonalityContract::default_contract(None);
        let ctx = AgentContext {
            channel: None,
            learned_context: "",
            conversation_summary: None,
            deep: false,
            personality_documents: None,
            plugin_context: None,
            skills_catalog: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(
            prompt.contains("## Response Style"),
            "missing Response Style section"
        );
        assert!(
            prompt.contains("Do NOT narrate"),
            "missing monologue suppression"
        );
    }

    #[test]
    fn prompt_includes_execution_bias_section() {
        let contract = PersonalityContract::default_contract(None);
        let ctx = AgentContext {
            channel: None,
            learned_context: "",
            conversation_summary: None,
            deep: false,
            personality_documents: None,
            plugin_context: None,
            skills_catalog: None,
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(
            prompt.contains("## Execution"),
            "missing Execution section"
        );
        assert!(
            prompt.contains("DO IT this turn"),
            "missing act-now execution directive"
        );
        assert!(
            prompt.contains("A plan is not a deliverable"),
            "missing plan-is-not-a-deliverable directive"
        );
    }

    #[test]
    fn prompt_injects_available_skills_catalog() {
        let contract = PersonalityContract::default_contract(None);
        let catalog = "<available_skills>\n  <skill>\n    <name>weather</name>\n    <location>/skills/weather/SKILL.md</location>\n    <version>1.0.0</version>\n  </skill>\n</available_skills>";
        let ctx = AgentContext {
            channel: None,
            learned_context: "",
            conversation_summary: None,
            deep: false,
            personality_documents: None,
            plugin_context: None,
            skills_catalog: Some(catalog),
        };
        let prompt = build_system_prompt(&contract, &ctx);
        assert!(prompt.contains("## Skills"), "missing Skills section");
        assert!(
            prompt.contains("<available_skills>"),
            "catalog block not injected"
        );
        assert!(prompt.contains("<name>weather</name>"), "skill not in prompt");
        assert!(
            prompt.contains("read its SKILL.md"),
            "missing on-demand load instruction"
        );
    }
}
