//! Delegation broker — fans out sub-tasks to specialised agents concurrently.
//!
//! # Overview
//!
//! [`DelegationBroker`] coordinates sub-task execution:
//!
//! 1. For each [`SubTask`], look up the agent definition in the registry.
//! 2. Create an isolated [`AgentContext`] seeded with the agent's system
//!    prompt and optional parent-context summary.
//! 3. Run the task input through the [`ModelClient`] using the allowed tools.
//! 4. Collect all [`SubTaskResult`]s and return them.
//!
//! All sub-tasks are executed **concurrently** using `tokio::spawn`.
//!
//! [`ModelClient`]: crate::model::ModelClient

use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{debug, instrument, warn};

use crate::delegation::{context::AgentContext, registry::AgentRegistry, DelegationError};
use crate::model::{ChatOptions, ModelClient, ToolDefinition, ToolDispatcher};

// ── SubTask ──────────────────────────────────────────────────────────────────

/// A single unit of work to be delegated to a named agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubTask {
    /// Name of the agent that should execute this task (must be registered in
    /// the [`AgentRegistry`]).
    pub agent_name: String,
    /// The task description / user query to send to the agent.
    pub input: String,
    /// Optional summary of the parent conversation, injected as grounding
    /// context into the agent's isolated history.
    pub parent_context: Option<String>,
}

impl SubTask {
    /// Convenience constructor.
    pub fn new(agent_name: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            agent_name: agent_name.into(),
            input: input.into(),
            parent_context: None,
        }
    }

    /// Builder — attach a parent-context summary for grounding.
    pub fn with_parent_context(mut self, ctx: impl Into<String>) -> Self {
        self.parent_context = Some(ctx.into());
        self
    }
}

// ── SubTaskResult ────────────────────────────────────────────────────────────

/// The outcome of one sub-task execution.
#[derive(Debug)]
pub struct SubTaskResult {
    /// Agent name that produced this result.
    pub agent_name: String,
    /// The text output from the agent, or an error description.
    pub output: Result<String, String>,
}

// ── DelegationBroker ─────────────────────────────────────────────────────────

/// Routes sub-tasks to specialised agents and runs them concurrently.
pub struct DelegationBroker {
    registry: Arc<AgentRegistry>,
    model: Arc<dyn ModelClient>,
    tools: Arc<dyn ToolDispatcher>,
}

impl DelegationBroker {
    /// Create a broker wired to `registry`, `model`, and `tools`.
    pub fn new(
        registry: Arc<AgentRegistry>,
        model: Arc<dyn ModelClient>,
        tools: Arc<dyn ToolDispatcher>,
    ) -> Self {
        Self {
            registry,
            model,
            tools,
        }
    }

    /// Fan out `tasks` to their respective agents and run them concurrently.
    ///
    /// Returns one [`SubTaskResult`] per input task, in submission order.
    /// A task that names an unknown agent produces an error result rather than
    /// aborting the whole batch.
    #[instrument(skip(self, tasks), fields(task_count = tasks.len()))]
    pub async fn delegate(&self, tasks: Vec<SubTask>) -> Vec<SubTaskResult> {
        if tasks.is_empty() {
            return vec![];
        }

        // We spawn one tokio task per sub-task and collect via JoinSet.
        // Each spawned task receives Arc-cloned handles so there is no
        // borrowing across await points.
        let mut join_set: JoinSet<SubTaskResult> = JoinSet::new();

        for task in tasks {
            let registry = Arc::clone(&self.registry);
            let model = Arc::clone(&self.model);
            let tools = Arc::clone(&self.tools);

            join_set.spawn(async move { run_sub_task(task, registry, model, tools).await });
        }

        // Collect results as they complete.  We want to preserve a
        // deterministic order for tests, so gather into a vec and sort by
        // agent_name after collection.
        let mut results: Vec<SubTaskResult> = Vec::new();
        while let Some(outcome) = join_set.join_next().await {
            match outcome {
                Ok(result) => results.push(result),
                Err(join_err) => {
                    warn!(error = %join_err, "sub-task join handle panicked");
                    results.push(SubTaskResult {
                        agent_name: "<unknown>".into(),
                        output: Err(format!("task panicked: {join_err}")),
                    });
                }
            }
        }

        results
    }
}

// ── internals ────────────────────────────────────────────────────────────────

/// Execute a single sub-task: look up agent definition, build context, run
/// the model loop, return result.
async fn run_sub_task(
    task: SubTask,
    registry: Arc<AgentRegistry>,
    model: Arc<dyn ModelClient>,
    tools: Arc<dyn ToolDispatcher>,
) -> SubTaskResult {
    let agent_name = task.agent_name.clone();

    // 1. Look up the agent definition.
    let definition = match registry.get(&agent_name) {
        Some(d) => d.clone(),
        None => {
            return SubTaskResult {
                agent_name,
                output: Err(DelegationError::UnknownAgent(task.agent_name).to_string()),
            };
        }
    };

    debug!(agent = %agent_name, "starting sub-task");

    // 2. Build isolated context.
    let mut context = match task.parent_context {
        Some(ref summary) => {
            AgentContext::with_parent_context(&agent_name, &definition.system_prompt, summary)
        }
        None => AgentContext::new(&agent_name, &definition.system_prompt),
    };

    context.push_user(&task.input);

    // 3. Retrieve and filter available tools.
    let all_tools: Vec<ToolDefinition> = tools.available_tools().await;
    let allowed: Vec<ToolDefinition> = definition
        .filter_tools(&all_tools)
        .into_iter()
        .cloned()
        .collect();

    // 4. Agentic model loop — up to max_turns iterations.
    let max_turns = definition.capabilities.max_turns;
    for turn in 0..max_turns {
        let completion = match model
            .complete(context.as_messages(), &allowed, &ChatOptions::default())
            .await
        {
            Ok(c) => c,
            Err(e) => {
                return SubTaskResult {
                    agent_name: agent_name.clone(),
                    output: Err(DelegationError::ModelError {
                        agent: agent_name.clone(),
                        message: e,
                    }
                    .to_string()),
                };
            }
        };

        // If the model made tool calls, dispatch them and feed results back.
        if !completion.tool_calls.is_empty() {
            debug!(
                agent = %agent_name,
                turn,
                tool_calls = completion.tool_calls.len(),
                "dispatching tool calls"
            );
            // Record the assistant's turn (with its tool call requests).
            use crate::model::ChatMessage;
            context.messages.push(ChatMessage {
                role: "assistant".into(),
                content: completion.content.unwrap_or_default(),
                tool_call_id: None,
                tool_calls: Some(completion.tool_calls.clone()),
            });

            for call in &completion.tool_calls {
                let result = tools.call_tool(&call.name, call.arguments.clone()).await;
                context
                    .messages
                    .push(ChatMessage::tool_result(call.id.clone(), result));
            }
            // Continue to the next turn so the model can process tool results.
            continue;
        }

        // Model produced a direct text response — we're done.
        let output = completion.content.unwrap_or_default();
        debug!(agent = %agent_name, turn, output_len = output.len(), "sub-task complete");
        context.push_assistant(&output);
        return SubTaskResult {
            agent_name,
            output: Ok(output),
        };
    }

    // Exhausted max_turns without a final answer.
    SubTaskResult {
        agent_name: agent_name.clone(),
        output: Err(format!(
            "agent '{agent_name}' exceeded max_turns ({max_turns}) without producing a final answer"
        )),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatMessage, ChatOptions, ModelCompletion, ToolDefinition};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Mock model that echoes the last user message ─────────────────────

    struct EchoModel;

    #[async_trait]
    impl ModelClient for EchoModel {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            let last_user = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(ModelCompletion {
                content: Some(format!("echo:{last_user}")),
                tool_calls: vec![],
                logprobs: None,
            })
        }
    }

    // ── Mock model that always errors ────────────────────────────────────

    struct ErrorModel;

    #[async_trait]
    impl ModelClient for ErrorModel {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Err("connection refused".into())
        }
    }

    // ── Mock tool dispatcher ─────────────────────────────────────────────

    struct NoopDispatcher;

    #[async_trait]
    impl ToolDispatcher for NoopDispatcher {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            vec![]
        }
        async fn call_tool(&self, _name: &str, _arguments: Value) -> String {
            String::new()
        }
    }

    // ── Mock model that calls a tool once then answers ───────────────────

    struct OnceToolModel {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ModelClient for OnceToolModel {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call — request a tool
                Ok(ModelCompletion {
                    content: None,
                    tool_calls: vec![crate::model::ToolCall {
                        id: "tc1".into(),
                        name: "read_file".into(),
                        arguments: serde_json::json!({"path": "foo.txt"}),
                    }],
                    logprobs: None,
                })
            } else {
                // Second call — produce the final answer
                let _ = messages; // ignore
                Ok(ModelCompletion {
                    content: Some("final answer after tool".into()),
                    tool_calls: vec![],
                    logprobs: None,
                })
            }
        }
    }

    struct ReturnToolDispatcher;

    #[async_trait]
    impl ToolDispatcher for ReturnToolDispatcher {
        async fn available_tools(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: "read_file".into(),
                description: "reads a file".into(),
                parameters: serde_json::json!({}),
            }]
        }
        async fn call_tool(&self, _name: &str, _arguments: Value) -> String {
            "file content".into()
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────────

    fn make_registry_with_builtins() -> Arc<AgentRegistry> {
        let mut reg = AgentRegistry::new();
        reg.register_builtins();
        Arc::new(reg)
    }

    fn make_registry_with_echo_agent() -> Arc<AgentRegistry> {
        use crate::delegation::registry::AgentDefinition;
        let mut reg = AgentRegistry::new();
        reg.register(AgentDefinition::new("echo", "echoes", "system prompt"));
        Arc::new(reg)
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn delegate_empty_returns_empty() {
        let broker = DelegationBroker::new(
            make_registry_with_builtins(),
            Arc::new(EchoModel),
            Arc::new(NoopDispatcher),
        );
        let results = broker.delegate(vec![]).await;
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn delegate_single_task_returns_echo() {
        let broker = DelegationBroker::new(
            make_registry_with_echo_agent(),
            Arc::new(EchoModel),
            Arc::new(NoopDispatcher),
        );
        let results = broker.delegate(vec![SubTask::new("echo", "hello")]).await;
        assert_eq!(results.len(), 1);
        let output = results[0].output.as_ref().unwrap();
        assert!(
            output.contains("hello"),
            "expected echo of 'hello', got: {output}"
        );
    }

    #[tokio::test]
    async fn delegate_unknown_agent_returns_error_result() {
        let broker = DelegationBroker::new(
            Arc::new(AgentRegistry::new()),
            Arc::new(EchoModel),
            Arc::new(NoopDispatcher),
        );
        let results = broker.delegate(vec![SubTask::new("ghost", "task")]).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].output.is_err());
        let err = results[0].output.as_ref().unwrap_err();
        assert!(err.contains("ghost"), "error must mention the agent name");
    }

    #[tokio::test]
    async fn delegate_model_error_returns_error_result() {
        let broker = DelegationBroker::new(
            make_registry_with_echo_agent(),
            Arc::new(ErrorModel),
            Arc::new(NoopDispatcher),
        );
        let results = broker.delegate(vec![SubTask::new("echo", "hello")]).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].output.is_err());
    }

    #[tokio::test]
    async fn delegate_concurrent_tasks() {
        // Two tasks assigned to the same echo agent — both should succeed.
        let broker = DelegationBroker::new(
            make_registry_with_echo_agent(),
            Arc::new(EchoModel),
            Arc::new(NoopDispatcher),
        );
        let tasks = vec![
            SubTask::new("echo", "task A"),
            SubTask::new("echo", "task B"),
        ];
        let results = broker.delegate(tasks).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.output.is_ok()));
    }

    #[tokio::test]
    async fn delegate_tool_call_loop() {
        use crate::delegation::registry::AgentDefinition;

        let calls = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(OnceToolModel {
            calls: Arc::clone(&calls),
        });

        let mut reg = AgentRegistry::new();
        reg.register(
            AgentDefinition::new("tooled", "uses tools", "system")
                .with_tools(["read_file"])
                .with_max_turns(5),
        );

        let broker = DelegationBroker::new(Arc::new(reg), model, Arc::new(ReturnToolDispatcher));

        let results = broker
            .delegate(vec![SubTask::new("tooled", "use read_file")])
            .await;
        assert_eq!(results.len(), 1);
        let output = results[0].output.as_ref().unwrap();
        assert_eq!(output, "final answer after tool");
        // Model was called twice: once for the tool call, once for the answer.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn delegate_with_parent_context_seeds_agent() {
        // We verify the parent context ends up in the messages by using a
        // model that echoes the full system prompt.
        struct SystemEchoModel;

        #[async_trait]
        impl ModelClient for SystemEchoModel {
            async fn complete(
                &self,
                messages: &[ChatMessage],
                _tools: &[ToolDefinition],
                _options: &ChatOptions,
            ) -> Result<ModelCompletion, String> {
                let system_content: String = messages
                    .iter()
                    .filter(|m| m.role == "system")
                    .map(|m| m.content.as_str())
                    .collect::<Vec<_>>()
                    .join(" | ");
                Ok(ModelCompletion {
                    content: Some(system_content),
                    tool_calls: vec![],
                    logprobs: None,
                })
            }
        }

        use crate::delegation::registry::AgentDefinition;
        let mut reg = AgentRegistry::new();
        reg.register(AgentDefinition::new("a", "d", "base prompt"));

        let broker = DelegationBroker::new(
            Arc::new(reg),
            Arc::new(SystemEchoModel),
            Arc::new(NoopDispatcher),
        );

        let task =
            SubTask::new("a", "query").with_parent_context("user is debugging a memory leak");

        let results = broker.delegate(vec![task]).await;
        let output = results[0].output.as_ref().unwrap();
        assert!(
            output.contains("memory leak"),
            "parent context must be passed to agent; got: {output}"
        );
    }
}
