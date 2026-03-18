//! Agent-invoke step — allows procedures to call an LLM model during execution.
//!
//! The `AgentInvoke` step lives in the application layer (pares-agens), NOT in
//! PluresDB.  PluresDB procedures are pure data operations; LLM calls are
//! app-layer concerns.
//!
//! # Safety
//!
//! Every [`AgentInvoke`] instance enforces three safety limits, configured via
//! [`InvokeConfig`]:
//!
//! - **`max_invocations`**: after this many calls `invoke` returns
//!   [`InvokeError::BudgetExceeded`] immediately.
//! - **`max_tokens`**: passed as intent to the caller; the model client is
//!   responsible for honouring a token limit (e.g. by injecting it into the
//!   request).  `AgentInvoke` records the limit so callers can read it.
//! - **`timeout_ms`**: every `invoke` call is wrapped in a
//!   [`tokio::time::timeout`]; a timed-out call returns
//!   [`InvokeError::Timeout`] without panicking.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use tokio::time::{timeout, Duration};

use crate::model::{ChatMessage, ModelClient};
use crate::procedure::Procedure;

// ── InvokeConfig ─────────────────────────────────────────────────────────────

/// Safety limits for [`AgentInvoke`].
#[derive(Debug, Clone)]
pub struct InvokeConfig {
    /// Maximum tokens the caller expects the model to produce.
    ///
    /// `AgentInvoke` stores this value; callers that wire up the model client
    /// should forward it to the underlying completion request.
    pub max_tokens: usize,
    /// Maximum number of [`AgentInvoke::invoke`] calls allowed before
    /// [`InvokeError::BudgetExceeded`] is returned.
    pub max_invocations: usize,
    /// Per-call wall-clock timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for InvokeConfig {
    fn default() -> Self {
        Self {
            max_tokens: 1024,
            max_invocations: 3,
            timeout_ms: 30_000,
        }
    }
}

// ── InvokeError ───────────────────────────────────────────────────────────────

/// Errors that can occur during an LLM invocation step.
#[derive(Debug, thiserror::Error)]
pub enum InvokeError {
    /// The underlying model client returned an error.
    #[error("model call failed: {0}")]
    ModelError(String),
    /// The model's response could not be interpreted as expected.
    #[error("response parsing failed: {0}")]
    ParseError(String),
    /// The [`InvokeConfig::max_invocations`] budget was exhausted.
    #[error("token budget exceeded")]
    BudgetExceeded,
    /// The model call did not complete within [`InvokeConfig::timeout_ms`].
    #[error("invocation timed out after {ms}ms")]
    Timeout {
        /// The configured timeout in milliseconds.
        ms: u64,
    },
}

// ── AgentInvoke ──────────────────────────────────────────────────────────────

/// A procedure step that invokes an LLM model and feeds the response back
/// into the procedure pipeline.
///
/// # Usage
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use pares_agens_core::cerebellum::invoke::{AgentInvoke, InvokeConfig};
///
/// # async fn example(client: Arc<dyn pares_agens_core::model::ModelClient>) {
/// let invoker = AgentInvoke::with_config(
///     client,
///     InvokeConfig { max_tokens: 256, max_invocations: 2, timeout_ms: 5_000 },
/// );
///
/// let result = invoker
///     .invoke("You are a classifier.", "Is this spam?", None)
///     .await;
/// # }
/// ```
pub struct AgentInvoke {
    model_client: Arc<dyn ModelClient>,
    config: InvokeConfig,
    invocation_count: AtomicUsize,
}

impl AgentInvoke {
    /// Create a new `AgentInvoke` with default [`InvokeConfig`] limits.
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        Self {
            model_client,
            config: InvokeConfig::default(),
            invocation_count: AtomicUsize::new(0),
        }
    }

    /// Create a new `AgentInvoke` with custom safety limits.
    pub fn with_config(model_client: Arc<dyn ModelClient>, config: InvokeConfig) -> Self {
        Self {
            model_client,
            config,
            invocation_count: AtomicUsize::new(0),
        }
    }

    /// The configuration for this invoke step.
    pub fn config(&self) -> &InvokeConfig {
        &self.config
    }

    /// How many times [`invoke`][Self::invoke] has been called so far on this
    /// instance.
    pub fn invocation_count(&self) -> usize {
        self.invocation_count.load(Ordering::Relaxed)
    }

    /// Invoke the model with a prompt constructed from the current procedure
    /// state.
    ///
    /// # Parameters
    ///
    /// - `system_prompt` — Role/instructions for the model.
    /// - `user_content` — The content to process (usually the output of a
    ///   previous pipeline step).
    /// - `response_format` — Optional JSON schema string.  When provided it is
    ///   appended as a second system message instructing the model to follow
    ///   the schema.
    ///
    /// # Errors
    ///
    /// Returns [`InvokeError::BudgetExceeded`] when the invocation limit is
    /// reached, [`InvokeError::Timeout`] when the call exceeds
    /// `config.timeout_ms`, and [`InvokeError::ModelError`] for model-client
    /// failures.
    pub async fn invoke(
        &self,
        system_prompt: &str,
        user_content: &str,
        response_format: Option<&str>,
    ) -> Result<String, InvokeError> {
        // ── Budget check ──────────────────────────────────────────────────────
        // Use fetch_add so concurrent callers each get a unique count value.
        // The check happens *before* the network call so we never burn tokens
        // on a call that should have been rejected.
        let prior = self.invocation_count.fetch_add(1, Ordering::Relaxed);
        if prior >= self.config.max_invocations {
            return Err(InvokeError::BudgetExceeded);
        }

        // ── Build message list ────────────────────────────────────────────────
        let mut messages = vec![ChatMessage::system(system_prompt)];

        if let Some(fmt) = response_format {
            messages.push(ChatMessage::system(format!(
                "Respond using the following JSON schema:\n{fmt}"
            )));
        }

        messages.push(ChatMessage::user(user_content));

        // ── Call the model with timeout ───────────────────────────────────────
        let duration = Duration::from_millis(self.config.timeout_ms);
        let call = self.model_client.complete(&messages, &[]);

        let completion = timeout(duration, call)
            .await
            .map_err(|_| InvokeError::Timeout { ms: self.config.timeout_ms })?
            .map_err(InvokeError::ModelError)?;

        // ── Extract text response ─────────────────────────────────────────────
        completion
            .content
            .ok_or_else(|| InvokeError::ParseError("model returned no text content".into()))
    }
}

// ── InvokableProcedure ────────────────────────────────────────────────────────

/// Extension of [`Procedure`] for procedures that need LLM access.
///
/// Procedures that require model invocations should implement this trait and
/// hold an internal `Arc<AgentInvoke>`.  The trait lets callers inject or swap
/// the model client before procedure execution begins.
#[async_trait]
pub trait InvokableProcedure: Procedure {
    /// Inject the model client used for LLM invocations within this procedure.
    fn set_model_client(&mut self, client: Arc<dyn ModelClient>);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelCompletion, ToolDefinition};
    use std::time::Duration as StdDuration;
    use tokio::time::sleep;

    // ── Mock model client ─────────────────────────────────────────────────────

    /// A mock `ModelClient` that immediately returns a fixed response.
    struct MockModelClient {
        response: String,
    }

    impl MockModelClient {
        fn new(response: impl Into<String>) -> Self {
            Self { response: response.into() }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some(self.response.clone()),
                tool_calls: vec![],
            })
        }
    }

    /// A mock `ModelClient` that sleeps for `delay` before responding.
    struct SlowModelClient {
        delay: StdDuration,
    }

    #[async_trait]
    impl ModelClient for SlowModelClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> Result<ModelCompletion, String> {
            sleep(self.delay.into()).await;
            Ok(ModelCompletion {
                content: Some("late response".into()),
                tool_calls: vec![],
            })
        }
    }

    /// A mock `ModelClient` that always returns an error.
    struct FailingModelClient;

    #[async_trait]
    impl ModelClient for FailingModelClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> Result<ModelCompletion, String> {
            Err("upstream unavailable".into())
        }
    }

    /// A mock `ModelClient` that returns a completion with no text content
    /// (tool-call-only response).
    struct ToolOnlyModelClient;

    #[async_trait]
    impl ModelClient for ToolOnlyModelClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: None,
                tool_calls: vec![],
            })
        }
    }

    // ── Helper ────────────────────────────────────────────────────────────────

    fn invoker_with_mock(response: &str) -> AgentInvoke {
        AgentInvoke::new(Arc::new(MockModelClient::new(response)))
    }

    // ── Basic invoke ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn basic_invoke_returns_model_response() {
        let invoker = invoker_with_mock("positive");
        let result = invoker
            .invoke("You are a classifier.", "Is this spam?", None)
            .await
            .unwrap();
        assert_eq!(result, "positive");
    }

    #[tokio::test]
    async fn invoke_increments_invocation_count() {
        let invoker = invoker_with_mock("ok");
        assert_eq!(invoker.invocation_count(), 0);
        invoker.invoke("sys", "user", None).await.unwrap();
        assert_eq!(invoker.invocation_count(), 1);
        invoker.invoke("sys", "user", None).await.unwrap();
        assert_eq!(invoker.invocation_count(), 2);
    }

    #[tokio::test]
    async fn invoke_with_response_format_succeeds() {
        let invoker = invoker_with_mock(r#"{"label":"spam"}"#);
        let schema = r#"{"type":"object","properties":{"label":{"type":"string"}}}"#;
        let result = invoker
            .invoke("Classify.", "Free money!", Some(schema))
            .await
            .unwrap();
        assert_eq!(result, r#"{"label":"spam"}"#);
    }

    // ── Budget enforcement ────────────────────────────────────────────────────

    #[tokio::test]
    async fn budget_exceeded_after_max_invocations() {
        let config = InvokeConfig {
            max_tokens: 64,
            max_invocations: 2,
            timeout_ms: 5_000,
        };
        let invoker = AgentInvoke::with_config(
            Arc::new(MockModelClient::new("ok")),
            config,
        );

        // First two calls succeed.
        invoker.invoke("sys", "msg", None).await.unwrap();
        invoker.invoke("sys", "msg", None).await.unwrap();

        // Third call should fail with BudgetExceeded.
        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(matches!(err, InvokeError::BudgetExceeded), "expected BudgetExceeded, got {err}");
    }

    #[tokio::test]
    async fn budget_exceeded_reported_correctly_at_limit() {
        let config = InvokeConfig {
            max_tokens: 64,
            max_invocations: 1,
            timeout_ms: 5_000,
        };
        let invoker = AgentInvoke::with_config(
            Arc::new(MockModelClient::new("ok")),
            config,
        );

        invoker.invoke("sys", "msg", None).await.unwrap();
        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(matches!(err, InvokeError::BudgetExceeded));
    }

    // ── Timeout ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn invoke_times_out_when_model_is_slow() {
        let config = InvokeConfig {
            max_tokens: 64,
            max_invocations: 3,
            timeout_ms: 50, // very short timeout
        };
        let invoker = AgentInvoke::with_config(
            Arc::new(SlowModelClient {
                delay: StdDuration::from_millis(500),
            }),
            config,
        );

        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(
            matches!(err, InvokeError::Timeout { ms: 50 }),
            "expected Timeout, got {err}"
        );
    }

    // ── Model errors ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn model_error_propagates() {
        let invoker = AgentInvoke::new(Arc::new(FailingModelClient));
        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(
            matches!(err, InvokeError::ModelError(_)),
            "expected ModelError, got {err}"
        );
    }

    #[tokio::test]
    async fn parse_error_when_model_returns_no_content() {
        let invoker = AgentInvoke::new(Arc::new(ToolOnlyModelClient));
        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(
            matches!(err, InvokeError::ParseError(_)),
            "expected ParseError, got {err}"
        );
    }

    // ── InvokeConfig defaults ─────────────────────────────────────────────────

    #[test]
    fn invoke_config_defaults() {
        let cfg = InvokeConfig::default();
        assert_eq!(cfg.max_tokens, 1024);
        assert_eq!(cfg.max_invocations, 3);
        assert_eq!(cfg.timeout_ms, 30_000);
    }

    // ── AgentInvoke accessors ─────────────────────────────────────────────────

    #[test]
    fn agent_invoke_config_accessor() {
        let invoker = AgentInvoke::with_config(
            Arc::new(MockModelClient::new("x")),
            InvokeConfig { max_tokens: 512, max_invocations: 5, timeout_ms: 1_000 },
        );
        assert_eq!(invoker.config().max_tokens, 512);
        assert_eq!(invoker.config().max_invocations, 5);
        assert_eq!(invoker.config().timeout_ms, 1_000);
        assert_eq!(invoker.invocation_count(), 0);
    }

    // ── Integration: procedure that classifies text via invoke ────────────────

    /// A minimal `InvokableProcedure` that uses `AgentInvoke` to classify a
    /// message event as "spam" or "not-spam" and emits a `StateChange` event.
    struct ClassifyProcedure {
        invoker: Arc<AgentInvoke>,
    }

    #[async_trait]
    impl crate::procedure::Procedure for ClassifyProcedure {
        fn name(&self) -> &str {
            "classify"
        }

        fn handles(&self) -> &str {
            "message"
        }

        async fn execute(&self, event: &crate::event::Event) -> Vec<crate::event::Event> {
            if let crate::event::Event::Message { content, .. } = event {
                let result = self
                    .invoker
                    .invoke(
                        "Classify the following message as 'spam' or 'not-spam'.",
                        content,
                        None,
                    )
                    .await;

                match result {
                    Ok(label) => vec![crate::event::Event::StateChange {
                        key: "spam_label".into(),
                        old_value: None,
                        new_value: serde_json::Value::String(label),
                    }],
                    Err(_) => vec![],
                }
            } else {
                vec![]
            }
        }
    }

    #[async_trait]
    impl InvokableProcedure for ClassifyProcedure {
        fn set_model_client(&mut self, client: Arc<dyn ModelClient>) {
            self.invoker = Arc::new(AgentInvoke::new(client));
        }
    }

    #[tokio::test]
    async fn invokable_procedure_classifies_message() {
        let procedure = ClassifyProcedure {
            invoker: Arc::new(AgentInvoke::new(Arc::new(MockModelClient::new("not-spam")))),
        };

        let event = crate::event::Event::Message {
            id: "1".into(),
            channel: "general".into(),
            sender: "user".into(),
            content: "Hello, how are you?".into(),
        };

        let output = procedure.execute(&event).await;
        assert_eq!(output.len(), 1);
        if let crate::event::Event::StateChange { key, new_value, .. } = &output[0] {
            assert_eq!(key, "spam_label");
            assert_eq!(new_value, &serde_json::Value::String("not-spam".into()));
        } else {
            panic!("expected StateChange event");
        }
    }

    // ── Error display ─────────────────────────────────────────────────────────

    #[test]
    fn invoke_error_display_messages() {
        assert_eq!(
            InvokeError::ModelError("oops".into()).to_string(),
            "model call failed: oops"
        );
        assert_eq!(
            InvokeError::ParseError("bad json".into()).to_string(),
            "response parsing failed: bad json"
        );
        assert_eq!(InvokeError::BudgetExceeded.to_string(), "token budget exceeded");
        assert_eq!(
            InvokeError::Timeout { ms: 500 }.to_string(),
            "invocation timed out after 500ms"
        );
    }
}
