//! Isolated conversation context for a single sub-agent run.

use pares_radix_core::model::ChatMessage;

/// Holds the conversation history for a single sub-agent invocation.
///
/// Each sub-agent receives its own `AgentContext` so that its message history
/// does not bleed into other agents or the primary conversation thread.
///
/// The context is pre-seeded with the agent's system prompt and, optionally,
/// with a parent-context summary for grounding.
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Agent name (for logging and diagnostics).
    pub agent_name: String,
    /// The accumulated message history, in chronological order.
    pub messages: Vec<ChatMessage>,
}

impl AgentContext {
    /// Create a new context seeded with `system_prompt`.
    pub fn new(agent_name: impl Into<String>, system_prompt: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            messages: vec![ChatMessage::system(system_prompt)],
        }
    }

    /// Create a new context seeded with `system_prompt` and an optional
    /// parent-context summary injected as a second system message.
    ///
    /// The parent summary grounds the sub-agent in the broader conversation
    /// without passing the full history across context boundaries.
    pub fn with_parent_context(
        agent_name: impl Into<String>,
        system_prompt: impl Into<String>,
        parent_summary: impl Into<String>,
    ) -> Self {
        let summary = parent_summary.into();
        let mut ctx = Self::new(agent_name, system_prompt);
        if !summary.trim().is_empty() {
            ctx.messages.push(ChatMessage::system(format!(
                "## Parent context (summary)\n{summary}"
            )));
        }
        ctx
    }

    /// Push a user message onto the history.
    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::user(content));
    }

    /// Push an assistant message onto the history.
    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMessage::assistant(content));
    }

    /// Return the full message slice for passing to a [`ModelClient`].
    ///
    /// [`ModelClient`]: pares_radix_core::model::ModelClient
    pub fn as_messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Number of messages in the history (including the system prompt).
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the context has no messages at all (should never be true in
    /// normal usage because the constructor always inserts a system prompt).
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_context_starts_with_system_prompt() {
        let ctx = AgentContext::new("coder", "You are a coding assistant.");
        assert_eq!(ctx.len(), 1);
        assert_eq!(ctx.messages[0].role, "system");
        assert!(ctx.messages[0].content.contains("coding assistant"));
    }

    #[test]
    fn with_parent_context_adds_second_system_message() {
        let ctx = AgentContext::with_parent_context(
            "analyst",
            "You analyze data.",
            "User is debugging a memory leak.",
        );
        assert_eq!(ctx.len(), 2);
        assert_eq!(ctx.messages[1].role, "system");
        assert!(ctx.messages[1].content.contains("memory leak"));
    }

    #[test]
    fn with_parent_context_empty_summary_no_extra_message() {
        let ctx = AgentContext::with_parent_context("analyst", "prompt", "   ");
        assert_eq!(ctx.len(), 1);
    }

    #[test]
    fn push_user_and_assistant_grow_history() {
        let mut ctx = AgentContext::new("r", "p");
        ctx.push_user("question");
        ctx.push_assistant("answer");
        assert_eq!(ctx.len(), 3);
        assert_eq!(ctx.messages[1].role, "user");
        assert_eq!(ctx.messages[2].role, "assistant");
    }

    #[test]
    fn as_messages_returns_all_messages() {
        let mut ctx = AgentContext::new("r", "p");
        ctx.push_user("q");
        assert_eq!(ctx.as_messages().len(), 2);
    }

    #[test]
    fn contexts_are_isolated() {
        let mut ctx_a = AgentContext::new("a", "prompt A");
        let ctx_b = AgentContext::new("b", "prompt B");
        ctx_a.push_user("message for A");
        assert_eq!(ctx_b.len(), 1, "context B must not be affected by A's push");
    }
}
