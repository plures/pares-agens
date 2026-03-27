//! Agent registry — stores named [`AgentDefinition`]s.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::ToolDefinition;

// ── AgentCapabilities ────────────────────────────────────────────────────────

/// Capabilities and constraints of a named agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCapabilities {
    /// Preferred model identifier (e.g. `"qwen3:30b-a3b"`, `"gpt-4o"`).
    ///
    /// When `None` the broker uses whatever default is configured in the
    /// calling context.
    pub model: Option<String>,
    /// Names of tools this agent is permitted to call.  An empty list means
    /// no tools are available to the agent.
    pub allowed_tools: Vec<String>,
    /// Maximum number of tool-call / response turns before the agent gives up.
    pub max_turns: usize,
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        Self {
            model: None,
            allowed_tools: vec![],
            max_turns: 10,
        }
    }
}

// ── AgentDefinition ──────────────────────────────────────────────────────────

/// A named, configurable agent definition.
///
/// An `AgentDefinition` lives in the [`AgentRegistry`] and is looked up by
/// name when a sub-task is dispatched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDefinition {
    /// Unique name used to address this agent (e.g. `"researcher"`).
    pub name: String,
    /// Human-readable description shown in the UI and logs.
    pub description: String,
    /// The system prompt injected at the start of every conversation this
    /// agent participates in.
    pub system_prompt: String,
    /// Capabilities / constraints for this agent.
    pub capabilities: AgentCapabilities,
}

impl AgentDefinition {
    /// Create a new definition with sensible defaults.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        system_prompt: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            system_prompt: system_prompt.into(),
            capabilities: AgentCapabilities::default(),
        }
    }

    /// Builder — set the preferred model.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.capabilities.model = Some(model.into());
        self
    }

    /// Builder — set the allowed tools for this agent.
    pub fn with_tools(mut self, tools: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.capabilities.allowed_tools = tools.into_iter().map(Into::into).collect();
        self
    }

    /// Builder — set max turns.
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.capabilities.max_turns = max_turns;
        self
    }

    /// Filter a full list of [`ToolDefinition`]s down to the subset this
    /// agent is allowed to call.
    ///
    /// If `allowed_tools` is empty, returns an empty slice (no tools allowed).
    pub fn filter_tools<'a>(&self, all_tools: &'a [ToolDefinition]) -> Vec<&'a ToolDefinition> {
        if self.capabilities.allowed_tools.is_empty() {
            return vec![];
        }
        all_tools
            .iter()
            .filter(|t| self.capabilities.allowed_tools.contains(&t.name))
            .collect()
    }
}

// ── AgentRegistry ────────────────────────────────────────────────────────────

/// Registry of named [`AgentDefinition`]s.
///
/// Agents are looked up by name during delegation.  The registry also supports
/// user-defined agents registered at runtime (e.g. loaded from PluresDB
/// procedures).
#[derive(Default)]
pub struct AgentRegistry {
    agents: HashMap<String, AgentDefinition>,
}

impl AgentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) an agent definition.
    pub fn register(&mut self, definition: AgentDefinition) {
        self.agents.insert(definition.name.clone(), definition);
    }

    /// Register the three built-in agents (`researcher`, `coder`, `analyst`).
    pub fn register_builtins(&mut self) {
        use crate::delegation::builtin;
        self.register(builtin::researcher());
        self.register(builtin::coder());
        self.register(builtin::analyst());
    }

    /// Look up an agent by name.
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.get(name)
    }

    /// Remove an agent definition.  Returns the removed definition if it existed.
    pub fn remove(&mut self, name: &str) -> Option<AgentDefinition> {
        self.agents.remove(name)
    }

    /// Iterate over all registered agent definitions.
    pub fn iter(&self) -> impl Iterator<Item = &AgentDefinition> {
        self.agents.values()
    }

    /// Number of registered agents.
    pub fn len(&self) -> usize {
        self.agents.len()
    }

    /// Whether the registry has no agents.
    pub fn is_empty(&self) -> bool {
        self.agents.is_empty()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_get_agent() {
        let mut registry = AgentRegistry::new();
        let def = AgentDefinition::new("echo", "echoes back", "You echo everything.");
        registry.register(def);
        assert!(registry.get("echo").is_some());
        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn register_replaces_existing() {
        let mut registry = AgentRegistry::new();
        registry.register(AgentDefinition::new("a", "v1", "prompt v1"));
        registry.register(AgentDefinition::new("a", "v2", "prompt v2"));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get("a").unwrap().description, "v2");
    }

    #[test]
    fn remove_agent() {
        let mut registry = AgentRegistry::new();
        registry.register(AgentDefinition::new("a", "d", "p"));
        let removed = registry.remove("a");
        assert!(removed.is_some());
        assert!(registry.is_empty());
    }

    #[test]
    fn register_builtins_adds_three_agents() {
        let mut registry = AgentRegistry::new();
        registry.register_builtins();
        assert_eq!(registry.len(), 3);
        assert!(registry.get("researcher").is_some());
        assert!(registry.get("coder").is_some());
        assert!(registry.get("analyst").is_some());
    }

    #[test]
    fn filter_tools_restricts_to_allowed() {
        use serde_json::json;

        let def = AgentDefinition::new("t", "d", "p").with_tools(["search", "read_file"]);

        let all_tools = vec![
            ToolDefinition {
                name: "search".into(),
                description: "web search".into(),
                parameters: json!({}),
            },
            ToolDefinition {
                name: "write_file".into(),
                description: "write a file".into(),
                parameters: json!({}),
            },
            ToolDefinition {
                name: "read_file".into(),
                description: "read a file".into(),
                parameters: json!({}),
            },
        ];

        let filtered = def.filter_tools(&all_tools);
        let names: Vec<&str> = filtered.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"search"));
        assert!(names.contains(&"read_file"));
    }

    #[test]
    fn filter_tools_returns_empty_when_no_allowed() {
        use serde_json::json;

        let def = AgentDefinition::new("t", "d", "p"); // no tools

        let all_tools = vec![ToolDefinition {
            name: "search".into(),
            description: "web search".into(),
            parameters: json!({}),
        }];

        assert!(def.filter_tools(&all_tools).is_empty());
    }

    #[test]
    fn builder_methods_set_fields() {
        let def = AgentDefinition::new("t", "d", "p")
            .with_model("gpt-4o")
            .with_tools(["search"])
            .with_max_turns(5);

        assert_eq!(def.capabilities.model.as_deref(), Some("gpt-4o"));
        assert_eq!(def.capabilities.allowed_tools, vec!["search"]);
        assert_eq!(def.capabilities.max_turns, 5);
    }
}
