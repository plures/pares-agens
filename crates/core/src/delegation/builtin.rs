//! Built-in agent definitions: `researcher`, `coder`, `analyst`.

use crate::delegation::registry::AgentDefinition;

/// The **researcher** agent: specialised in information retrieval and web search.
///
/// Has access to `web_search` and `fetch_url` tools.
pub fn researcher() -> AgentDefinition {
    AgentDefinition::new(
        "researcher",
        "Information retrieval and web research specialist.",
        "You are an expert research assistant. Your job is to find accurate, \
         up-to-date information on topics using the available web search and \
         retrieval tools. Always cite sources and prefer recent authoritative \
         references. Be concise and factual. When unsure, say so.",
    )
    .with_tools(["web_search", "fetch_url"])
    .with_max_turns(8)
}

/// The **coder** agent: specialised in writing, reviewing, and debugging code.
///
/// Has access to `read_file`, `write_file`, `run_command`, and `search_code` tools.
pub fn coder() -> AgentDefinition {
    AgentDefinition::new(
        "coder",
        "Software engineering and code-focused specialist.",
        "You are an expert software engineer. Your job is to write correct, \
         idiomatic, and well-documented code. Prefer the language and framework \
         already in use in the repository. Always run lint/test tools after \
         writing or modifying code. Explain your changes clearly and flag any \
         potential security issues.",
    )
    .with_tools(["read_file", "write_file", "run_command", "search_code"])
    .with_max_turns(15)
}

/// The **analyst** agent: specialised in reasoning, evaluation, and synthesis.
///
/// Has no tool access — relies purely on reasoning over supplied context.
pub fn analyst() -> AgentDefinition {
    AgentDefinition::new(
        "analyst",
        "Deep reasoning, evaluation, and synthesis specialist.",
        "You are an expert analytical thinker. Your job is to reason carefully \
         over the information you are given, identify patterns, evaluate \
         trade-offs, and synthesise clear conclusions. Do not make things up; \
         if the context is insufficient, explicitly state what additional \
         information is needed. Structure your output with clear headings.",
    )
    .with_max_turns(6)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn researcher_has_search_tools() {
        let def = researcher();
        assert_eq!(def.name, "researcher");
        assert!(def
            .capabilities
            .allowed_tools
            .contains(&"web_search".to_string()));
        assert!(def
            .capabilities
            .allowed_tools
            .contains(&"fetch_url".to_string()));
    }

    #[test]
    fn coder_has_file_tools() {
        let def = coder();
        assert_eq!(def.name, "coder");
        assert!(def
            .capabilities
            .allowed_tools
            .contains(&"read_file".to_string()));
        assert!(def
            .capabilities
            .allowed_tools
            .contains(&"write_file".to_string()));
        assert!(def
            .capabilities
            .allowed_tools
            .contains(&"run_command".to_string()));
    }

    #[test]
    fn analyst_has_no_tools() {
        let def = analyst();
        assert_eq!(def.name, "analyst");
        assert!(def.capabilities.allowed_tools.is_empty());
    }

    #[test]
    fn all_builtins_have_non_empty_system_prompts() {
        for def in [researcher(), coder(), analyst()] {
            assert!(
                !def.system_prompt.trim().is_empty(),
                "agent '{}' has an empty system prompt",
                def.name
            );
        }
    }
}
