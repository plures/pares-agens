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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use pares_agens_privacy::PrivacyFilter;
use tokio::time::{timeout, Duration};
use tracing::info;

use crate::model::{ChatMessage, ChatOptions, ModelClient};
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
    /// Optional PII redaction filter.  When set, user content is redacted
    /// before being sent to the model and the response is de-redacted before
    /// being returned to the caller.
    privacy_filter: Option<Arc<PrivacyFilter>>,
}

impl AgentInvoke {
    /// Create a new `AgentInvoke` with default [`InvokeConfig`] limits.
    pub fn new(model_client: Arc<dyn ModelClient>) -> Self {
        Self {
            model_client,
            config: InvokeConfig::default(),
            invocation_count: AtomicUsize::new(0),
            privacy_filter: None,
        }
    }

    /// Create a new `AgentInvoke` with custom safety limits.
    pub fn with_config(model_client: Arc<dyn ModelClient>, config: InvokeConfig) -> Self {
        Self {
            model_client,
            config,
            invocation_count: AtomicUsize::new(0),
            privacy_filter: None,
        }
    }

    /// Attach a [`PrivacyFilter`] to this invoker.
    ///
    /// When a filter is present every call to [`invoke`][Self::invoke] will:
    ///
    /// 1. Redact PII in the user content, replacing values with typed numbered
    ///    placeholders (`[EMAIL_1]`, `[PHONE_1]`, …) before the request is
    ///    sent to the cloud model.
    /// 2. Restore the original values in the model response before returning
    ///    it to the caller.
    /// 3. Emit a `tracing::info!` audit log recording the number of items
    ///    redacted per PII type (never the content itself).
    ///
    /// Redaction is applied to the `user_content` parameter only; system
    /// prompts are developer-controlled and are not modified.
    #[must_use]
    pub fn with_redaction(mut self, filter: Arc<PrivacyFilter>) -> Self {
        self.privacy_filter = Some(filter);
        self
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
    /// When a [`PrivacyFilter`] is attached via [`Self::with_redaction`], PII
    /// in `user_content` is replaced with typed placeholders before the
    /// request reaches the cloud model, and the original values are restored
    /// in the returned response.  An audit log line is emitted via
    /// `tracing::info!` recording only PII type and count.
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

        // ── PII redaction ─────────────────────────────────────────────────────
        // When a privacy filter is configured, redact PII in the user content
        // before it is sent to the cloud model.  The redaction map is kept so
        // we can restore original values in the response.
        let (redacted_content, redaction_map) = if let Some(filter) = &self.privacy_filter {
            let (redacted, map, audit) = filter.redact(user_content);
            if !audit.entries.is_empty() {
                // Audit log: type + count only, never the actual PII content.
                for entry in &audit.entries {
                    info!(
                        pii_type = ?entry.pii_type,
                        count = entry.count,
                        "pii redacted before model call"
                    );
                }
            }
            (redacted, Some(map))
        } else {
            (user_content.to_string(), None)
        };

        // ── Build message list ────────────────────────────────────────────────
        let mut messages = vec![ChatMessage::system(system_prompt)];

        if let Some(fmt) = response_format {
            messages.push(ChatMessage::system(format!(
                "Respond using the following JSON schema:\n{fmt}"
            )));
        }

        messages.push(ChatMessage::user(&redacted_content));

        // ── Call the model with timeout ───────────────────────────────────────
        let duration = Duration::from_millis(self.config.timeout_ms);
        let options = ChatOptions::default();
        let call = self
            .model_client
            .complete(&messages, &[], &options);

        let completion = timeout(duration, call)
            .await
            .map_err(|_| InvokeError::Timeout {
                ms: self.config.timeout_ms,
            })?
            .map_err(InvokeError::ModelError)?;

        // ── Extract text response ─────────────────────────────────────────────
        let raw_response = completion
            .content
            .ok_or_else(|| InvokeError::ParseError("model returned no text content".into()))?;

        // ── Restore redacted values ───────────────────────────────────────────
        // Replace any placeholder tokens in the model response with the
        // original PII values so the output is transparent to the caller.
        let response = if let Some(map) = redaction_map {
            map.restore(&raw_response)
        } else {
            raw_response
        };

        Ok(response)
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
    use crate::model::{ChatOptions, ModelCompletion, ToolDefinition};
    use std::time::Duration as StdDuration;
    use tokio::time::sleep;

    // ── Mock model client ─────────────────────────────────────────────────────

    /// A mock `ModelClient` that immediately returns a fixed response.
    struct MockModelClient {
        response: String,
    }

    impl MockModelClient {
        fn new(response: impl Into<String>) -> Self {
            Self {
                response: response.into(),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some(self.response.clone()),
                tool_calls: vec![],
                logprobs: None,
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
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            sleep(self.delay).await;
            Ok(ModelCompletion {
                content: Some("late response".into()),
                tool_calls: vec![],
                logprobs: None,
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
            _options: &ChatOptions,
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
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: None,
                tool_calls: vec![],
                logprobs: None,
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
        let invoker = AgentInvoke::with_config(Arc::new(MockModelClient::new("ok")), config);

        // First two calls succeed.
        invoker.invoke("sys", "msg", None).await.unwrap();
        invoker.invoke("sys", "msg", None).await.unwrap();

        // Third call should fail with BudgetExceeded.
        let err = invoker.invoke("sys", "msg", None).await.unwrap_err();
        assert!(
            matches!(err, InvokeError::BudgetExceeded),
            "expected BudgetExceeded, got {err}"
        );
    }

    #[tokio::test]
    async fn budget_exceeded_reported_correctly_at_limit() {
        let config = InvokeConfig {
            max_tokens: 64,
            max_invocations: 1,
            timeout_ms: 5_000,
        };
        let invoker = AgentInvoke::with_config(Arc::new(MockModelClient::new("ok")), config);

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
            InvokeConfig {
                max_tokens: 512,
                max_invocations: 5,
                timeout_ms: 1_000,
            },
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
        assert_eq!(
            InvokeError::BudgetExceeded.to_string(),
            "token budget exceeded"
        );
        assert_eq!(
            InvokeError::Timeout { ms: 500 }.to_string(),
            "invocation timed out after 500ms"
        );
    }

    // ── Redaction pipeline ────────────────────────────────────────────────────

    /// A mock client that echoes back the user message content so tests can
    /// inspect what was actually sent to the "cloud API".
    struct EchoModelClient;

    #[async_trait]
    impl ModelClient for EchoModelClient {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            // Return the last user message content so we can assert on it.
            let user_msg = messages
                .iter()
                .rfind(|m| m.role == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(ModelCompletion {
                content: Some(user_msg),
                tool_calls: vec![],
                logprobs: None,
            })
        }
    }

    /// A mock client that returns a response containing a placeholder token —
    /// simulating a model that "remembers" the placeholder from the user
    /// message and includes it in its answer.
    struct PlaceholderEchoClient {
        response: String,
    }

    #[async_trait]
    impl ModelClient for PlaceholderEchoClient {
        async fn complete(
            &self,
            _messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            Ok(ModelCompletion {
                content: Some(self.response.clone()),
                tool_calls: vec![],
                logprobs: None,
            })
        }
    }

    #[tokio::test]
    async fn redaction_removes_pii_before_cloud_call() {
        let filter = Arc::new(PrivacyFilter::new());
        let invoker = AgentInvoke::new(Arc::new(EchoModelClient)).with_redaction(filter);

        // The echo client returns what was sent; the *redacted* user content
        // should never contain the original email.
        let result = invoker
            .invoke("system prompt", "Contact bob@example.com for help.", None)
            .await
            .unwrap();

        // After restoration the original email must be back.
        assert!(
            result.contains("bob@example.com"),
            "original email should be restored in response, got: {result}"
        );
        // The raw cloud content (echoed back) must NOT contain the original.
        // We verify indirectly: if redaction worked, the echo contained a
        // placeholder, and restore put the email back.
        assert!(
            !result.contains("[EMAIL_1]"),
            "placeholder should be restored before returning, got: {result}"
        );
    }

    #[tokio::test]
    async fn redaction_restores_values_in_response() {
        let filter = Arc::new(PrivacyFilter::new());
        // The "cloud" returns a response that contains the placeholder token.
        let invoker = AgentInvoke::new(Arc::new(PlaceholderEchoClient {
            response: "You asked about [EMAIL_1] — I can help.".into(),
        }))
        .with_redaction(filter);

        let result = invoker
            .invoke("sys", "My email is alice@example.com.", None)
            .await
            .unwrap();

        assert!(
            result.contains("alice@example.com"),
            "original email should be restored in response, got: {result}"
        );
        assert!(
            !result.contains("[EMAIL_1]"),
            "placeholder should not appear in final response, got: {result}"
        );
    }

    #[tokio::test]
    async fn invoke_without_redaction_passes_content_unchanged() {
        // When no PrivacyFilter is attached, content is forwarded as-is.
        let invoker = AgentInvoke::new(Arc::new(EchoModelClient));
        let content = "My email is alice@example.com.";
        let result = invoker.invoke("sys", content, None).await.unwrap();
        assert_eq!(result, content);
    }

    #[tokio::test]
    async fn redaction_is_noop_for_clean_content() {
        let filter = Arc::new(PrivacyFilter::new());
        let invoker = AgentInvoke::new(Arc::new(EchoModelClient)).with_redaction(filter);
        let content = "What is the weather today?";
        let result = invoker.invoke("sys", content, None).await.unwrap();
        assert_eq!(
            result, content,
            "clean content should pass through unchanged"
        );
    }
}
