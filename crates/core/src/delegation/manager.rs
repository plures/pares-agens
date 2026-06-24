//! Sub-agent session manager — tracks, lists, kills, and steers running sub-agents.
//!
//! Provides the session-management layer on top of [`DelegationBroker`]:
//!
//! * Unique session IDs for each spawned sub-agent.
//! * Timeout enforcement via `tokio::time::timeout`.
//! * Completion events pushed via `tokio::sync::mpsc`.
//! * List/kill/steer operations on running sessions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::delegation::broker::{DelegationBroker, SubTask, SubTaskResult};
use crate::delegation::steering::{self, SteeringTx};

// ── Types ────────────────────────────────────────────────────────────────────

/// Unique identifier for a running sub-agent session.
pub type SessionId = String;

/// Status of a sub-agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStatus {
    /// Currently executing.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with an error.
    Failed(String),
    /// Killed by the parent.
    Killed,
    /// Timed out.
    TimedOut,
}

/// Metadata about a running or completed sub-agent session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Unique session ID.
    pub id: SessionId,
    /// Agent name handling this session.
    pub agent_name: String,
    /// The task input given to this session.
    pub task_input: String,
    /// Optional label for display.
    pub label: Option<String>,
    /// When the session was spawned.
    pub started_at: DateTime<Utc>,
    /// When the session completed (if it has).
    pub completed_at: Option<DateTime<Utc>>,
    /// Current status.
    pub status: SessionStatus,
    /// Output text (if completed).
    pub output: Option<String>,
    /// Pending steering messages queued for injection at next turn boundary.
    pub pending_messages: Vec<String>,
}

/// A completion event pushed to the parent when a sub-agent finishes.
#[derive(Debug, Clone)]
pub struct CompletionEvent {
    /// Session ID that completed.
    pub session_id: SessionId,
    /// Agent name.
    pub agent_name: String,
    /// The result.
    pub result: Result<String, String>,
    /// Duration the task ran.
    pub duration: Duration,
    /// Steering messages that were queued but never consumed by the sub-agent.
    /// Non-empty means the parent sent directions that arrived too late.
    pub undelivered_steerings: Vec<String>,
}

/// Options for spawning a sub-agent session.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// Optional label for this session.
    pub label: Option<String>,
    /// Timeout for this session (None = no timeout).
    pub timeout: Option<Duration>,
    /// Optional parent context summary.
    pub parent_context: Option<String>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            label: None,
            timeout: Some(Duration::from_secs(30 * 60)), // 30 minutes default
            parent_context: None,
        }
    }
}

impl SpawnOptions {
    /// Create options with a specific timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Create options with no timeout.
    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Set the label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set parent context.
    pub fn with_parent_context(mut self, ctx: impl Into<String>) -> Self {
        self.parent_context = Some(ctx.into());
        self
    }
}

// ── SubAgentManager ──────────────────────────────────────────────────────────

/// Manages the lifecycle of spawned sub-agent sessions.
///
/// Wraps [`DelegationBroker`] with session tracking, timeouts, and
/// push-based completion events.
pub struct SubAgentManager {
    broker: Arc<DelegationBroker>,
    sessions: Arc<RwLock<HashMap<SessionId, SessionInfo>>>,
    handles: Arc<Mutex<HashMap<SessionId, JoinHandle<()>>>>,
    steering_txs: Arc<RwLock<HashMap<SessionId, SteeringTx>>>,
    completion_tx: mpsc::UnboundedSender<CompletionEvent>,
}

impl SubAgentManager {
    /// Create a new manager wrapping the given broker.
    ///
    /// Returns the manager and a receiver for completion events.
    pub fn new(broker: Arc<DelegationBroker>) -> (Self, mpsc::UnboundedReceiver<CompletionEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let manager = Self {
            broker,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            handles: Arc::new(Mutex::new(HashMap::new())),
            steering_txs: Arc::new(RwLock::new(HashMap::new())),
            completion_tx: tx,
        };
        (manager, rx)
    }

    /// Spawn a new sub-agent session.
    ///
    /// Returns the session ID immediately. The task runs in the background.
    /// A [`CompletionEvent`] is pushed to the receiver when it finishes.
    pub async fn spawn(
        &self,
        agent_name: impl Into<String>,
        input: impl Into<String>,
        options: SpawnOptions,
    ) -> SessionId {
        let id = Uuid::new_v4().to_string();
        let agent_name = agent_name.into();
        let input = input.into();

        let info = SessionInfo {
            id: id.clone(),
            agent_name: agent_name.clone(),
            task_input: input.clone(),
            label: options.label.clone(),
            started_at: Utc::now(),
            completed_at: None,
            status: SessionStatus::Running,
            output: None,
            pending_messages: Vec::new(),
        };

        // Register the session.
        self.sessions.write().await.insert(id.clone(), info);

        // Create steering channel for this session.
        let (steering_tx, steering_rx) = steering::channel();
        self.steering_txs
            .write()
            .await
            .insert(id.clone(), steering_tx);

        // Keep a clone of the rx so we can drain undelivered messages post-completion.
        let post_completion_rx = steering_rx.clone();

        // Build the sub-task with steering channel attached.
        let mut task = SubTask::new(agent_name.clone(), input);
        task = task.with_steering(steering_rx);
        if let Some(ctx) = options.parent_context {
            task = task.with_parent_context(ctx);
        }

        // Spawn the background task.
        let broker = Arc::clone(&self.broker);
        let sessions = Arc::clone(&self.sessions);
        let steering_txs = Arc::clone(&self.steering_txs);
        let tx = self.completion_tx.clone();
        let session_id = id.clone();
        let timeout = options.timeout;

        let handle = tokio::spawn(async move {
            let start = std::time::Instant::now();

            // Execute with optional timeout.
            let result = match timeout {
                Some(dur) => {
                    match tokio::time::timeout(dur, broker.delegate(vec![task])).await {
                        Ok(results) => extract_single_result(results),
                        Err(_elapsed) => {
                            // Timed out.
                            warn!(session_id = %session_id, "sub-agent timed out");
                            Err(format!("timed out after {:?}", dur))
                        }
                    }
                }
                None => {
                    let results = broker.delegate(vec![task]).await;
                    extract_single_result(results)
                }
            };

            let duration = start.elapsed();
            let now = Utc::now();

            // Update session info.
            let status = match &result {
                Ok(_) => SessionStatus::Completed,
                Err(e) if e.contains("timed out") => SessionStatus::TimedOut,
                Err(_) => SessionStatus::Failed(result.as_ref().unwrap_err().clone()),
            };

            {
                let mut sessions = sessions.write().await;
                if let Some(info) = sessions.get_mut(&session_id) {
                    info.status = status;
                    info.completed_at = Some(now);
                    info.output = result.as_ref().ok().cloned();
                }
            }

            // Clean up steering channel now that the session is done.
            steering_txs.write().await.remove(&session_id);

            // Drain any steering messages that were never consumed by the broker.
            let undelivered = post_completion_rx.drain().await;
            if !undelivered.is_empty() {
                debug!(
                    session_id = %session_id,
                    count = undelivered.len(),
                    "undelivered steering messages at completion"
                );
            }

            // Push completion event.
            let event = CompletionEvent {
                session_id: session_id.clone(),
                agent_name,
                result,
                duration,
                undelivered_steerings: undelivered,
            };

            if let Err(e) = tx.send(event) {
                debug!(session_id = %session_id, "completion event receiver dropped: {e}");
            }

            info!(session_id = %session_id, "sub-agent session complete");
        });

        self.handles.lock().await.insert(id.clone(), handle);
        id
    }

    /// List all sessions (running and completed).
    pub async fn list(&self) -> Vec<SessionInfo> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// List only running sessions.
    pub async fn list_running(&self) -> Vec<SessionInfo> {
        self.sessions
            .read()
            .await
            .values()
            .filter(|s| s.status == SessionStatus::Running)
            .cloned()
            .collect()
    }

    /// Get info about a specific session.
    pub async fn get(&self, session_id: &str) -> Option<SessionInfo> {
        self.sessions.read().await.get(session_id).cloned()
    }

    /// Kill a running sub-agent session.
    ///
    /// Returns `true` if the session was found and killed.
    pub async fn kill(&self, session_id: &str) -> bool {
        // Abort the tokio task.
        let handle = self.handles.lock().await.remove(session_id);
        if let Some(h) = handle {
            h.abort();

            // Update session status.
            let mut sessions = self.sessions.write().await;
            if let Some(info) = sessions.get_mut(session_id) {
                info.status = SessionStatus::Killed;
                info.completed_at = Some(Utc::now());
            }

            // Clean up steering channel.
            self.steering_txs.write().await.remove(session_id);

            info!(session_id = %session_id, "sub-agent killed");
            true
        } else {
            false
        }
    }

    /// Steer a running sub-agent by injecting an additional message.
    ///
    /// Sends the message through the steering channel so it gets picked up
    /// at the next turn boundary in the broker's model loop.
    /// Also records the message in `pending_messages` for observability.
    /// Returns `true` if the message was queued successfully.
    pub async fn steer(&self, session_id: &str, message: &str) -> bool {
        // First check session status.
        {
            let sessions = self.sessions.read().await;
            match sessions.get(session_id) {
                Some(info) if info.status == SessionStatus::Running => {}
                Some(_) => {
                    warn!(session_id = %session_id, "cannot steer: session not running");
                    return false;
                }
                None => return false,
            }
        }

        // Send through the steering channel.
        let sent = {
            let txs = self.steering_txs.read().await;
            if let Some(tx) = txs.get(session_id) {
                tx.send(message.to_string()).await;
                true
            } else {
                false
            }
        };

        if sent {
            // Record in pending_messages for observability.
            let mut sessions = self.sessions.write().await;
            if let Some(info) = sessions.get_mut(session_id) {
                info.pending_messages.push(message.to_string());
            }
            info!(session_id = %session_id, "steering message sent to sub-agent");
        }

        sent
    }

    /// Remove completed sessions older than `max_age` from the session store.
    pub async fn cleanup(&self, max_age: Duration) {
        let cutoff = Utc::now() - chrono::Duration::from_std(max_age).unwrap_or_default();
        let mut sessions = self.sessions.write().await;
        sessions.retain(|_, info| {
            if info.status == SessionStatus::Running {
                return true; // Never clean up running sessions.
            }
            match info.completed_at {
                Some(t) => t > cutoff,
                None => true,
            }
        });
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Extract the single result from a delegation of one task.
fn extract_single_result(mut results: Vec<SubTaskResult>) -> Result<String, String> {
    if results.is_empty() {
        return Err("no result from delegation".into());
    }
    results.remove(0).output
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delegation::registry::{AgentDefinition, AgentRegistry};
    use pares_radix_core::model::{
        ChatMessage, ChatOptions, ModelClient, ModelCompletion, ToolDefinition, ToolDispatcher,
    };
    use async_trait::async_trait;
    use serde_json::Value;
    use std::time::Duration;

    // ── Mocks ────────────────────────────────────────────────────────────

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
                model: None,
            })
        }
    }

    struct SlowModel {
        delay: Duration,
    }

    #[async_trait]
    impl ModelClient for SlowModel {
        async fn complete(
            &self,
            messages: &[ChatMessage],
            _tools: &[ToolDefinition],
            _options: &ChatOptions,
        ) -> Result<ModelCompletion, String> {
            tokio::time::sleep(self.delay).await;
            let last_user = messages
                .iter()
                .rev()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Ok(ModelCompletion {
                content: Some(format!("slow:{last_user}")),
                tool_calls: vec![],
                logprobs: None,
                model: None,
            })
        }
    }

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

    fn make_broker(model: Arc<dyn ModelClient>) -> Arc<DelegationBroker> {
        let mut reg = AgentRegistry::new();
        reg.register(AgentDefinition::new("echo", "echoes", "You echo."));
        Arc::new(DelegationBroker::new(
            Arc::new(reg),
            model,
            Arc::new(NoopDispatcher),
        ))
    }

    // ── Tests ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_and_receive_completion_event() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager
            .spawn("echo", "hello world", SpawnOptions::default())
            .await;

        // Wait for the completion event.
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout waiting for completion")
            .expect("channel closed");

        assert_eq!(event.session_id, id);
        assert_eq!(event.agent_name, "echo");
        assert!(event.result.unwrap().contains("hello world"));
    }

    #[tokio::test]
    async fn list_shows_running_then_completed() {
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_millis(200),
        }));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "test", SpawnOptions::default()).await;

        // Should be running immediately.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let running = manager.list_running().await;
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, id);
        assert_eq!(running[0].status, SessionStatus::Running);

        // Wait for completion.
        let _ = rx.recv().await;

        // Should now be completed.
        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.status, SessionStatus::Completed);
        assert!(info.output.is_some());
    }

    #[tokio::test]
    async fn kill_aborts_running_session() {
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_secs(60),
        }));
        let (manager, _rx) = SubAgentManager::new(broker);

        let id = manager
            .spawn("echo", "long task", SpawnOptions::default())
            .await;

        tokio::time::sleep(Duration::from_millis(10)).await;
        let killed = manager.kill(&id).await;
        assert!(killed);

        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.status, SessionStatus::Killed);
    }

    #[tokio::test]
    async fn timeout_produces_timed_out_status() {
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_secs(60),
        }));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let opts = SpawnOptions::default().with_timeout(Duration::from_millis(50));
        let id = manager.spawn("echo", "will timeout", opts).await;

        // Wait for the completion event.
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout waiting for event")
            .expect("channel closed");

        assert_eq!(event.session_id, id);
        assert!(event.result.is_err());
        assert!(event.result.unwrap_err().contains("timed out"));

        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.status, SessionStatus::TimedOut);
    }

    #[tokio::test]
    async fn spawn_with_label_is_stored() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let opts = SpawnOptions::default().with_label("my-task");
        let id = manager.spawn("echo", "hi", opts).await;

        let _ = rx.recv().await;

        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.label.as_deref(), Some("my-task"));
    }

    #[tokio::test]
    async fn cleanup_removes_old_completed_sessions() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "done", SpawnOptions::default()).await;

        let _ = rx.recv().await;

        // Cleanup with 0 max_age should remove it.
        manager.cleanup(Duration::from_secs(0)).await;
        assert!(manager.get(&id).await.is_none());
    }

    #[tokio::test]
    async fn multiple_concurrent_spawns() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id1 = manager
            .spawn("echo", "task1", SpawnOptions::default())
            .await;
        let id2 = manager
            .spawn("echo", "task2", SpawnOptions::default())
            .await;
        let id3 = manager
            .spawn("echo", "task3", SpawnOptions::default())
            .await;

        // Collect all 3 events.
        let mut events = Vec::new();
        for _ in 0..3 {
            let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("timeout")
                .expect("closed");
            events.push(event);
        }

        assert_eq!(events.len(), 3);
        let ids: Vec<&str> = events.iter().map(|e| e.session_id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
        assert!(ids.contains(&id3.as_str()));
    }

    #[tokio::test]
    async fn steer_queues_message_for_running_session() {
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_secs(5),
        }));
        let (manager, _rx) = SubAgentManager::new(broker);

        let id = manager
            .spawn("echo", "task1", SpawnOptions::default())
            .await;

        // Give the session time to start executing.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Steer should succeed since session is still running.
        let steered = manager.steer(&id, "change course").await;
        assert!(steered);

        // Verify message is queued.
        let sessions = manager.sessions.read().await;
        let info = sessions.get(&id).unwrap();
        assert_eq!(info.pending_messages, vec!["change course".to_string()]);
    }

    #[tokio::test]
    async fn steer_fails_for_nonexistent_session() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, _rx) = SubAgentManager::new(broker);

        let steered = manager.steer("nonexistent", "hello").await;
        assert!(!steered);
    }

    #[tokio::test]
    async fn completion_event_reports_undelivered_steerings() {
        // Use SlowModel so there's a window to inject a steering message
        // that won't be consumed (model returns text on first turn, no loop iteration).
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_millis(200),
        }));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "task", SpawnOptions::default()).await;

        // Wait for the model to start executing, then steer.
        // The steering message arrives mid-model-call, so it won't be drained
        // by the broker (drain only happens at the top of each loop iteration).
        tokio::time::sleep(Duration::from_millis(50)).await;
        let steered = manager.steer(&id, "too late").await;
        assert!(steered);

        // Wait for completion.
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(event.session_id, id);
        assert_eq!(event.undelivered_steerings, vec!["too late".to_string()]);
    }

    #[tokio::test]
    async fn completion_event_empty_undelivered_when_no_steering() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let _id = manager
            .spawn("echo", "hello", SpawnOptions::default())
            .await;

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert!(event.undelivered_steerings.is_empty());
    }

    // ── Mutation-coverage tests ───────────────────────────────────────────

    #[tokio::test]
    async fn with_parent_context_sets_value() {
        let opts = SpawnOptions::default().with_parent_context("ctx-data");
        assert_eq!(opts.parent_context.as_deref(), Some("ctx-data"));
    }

    #[tokio::test]
    async fn without_timeout_clears_value() {
        let opts = SpawnOptions::default()
            .with_timeout(Duration::from_secs(10))
            .without_timeout();
        assert!(opts.timeout.is_none());
    }

    #[tokio::test]
    async fn list_returns_all_sessions() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id1 = manager.spawn("echo", "a", SpawnOptions::default()).await;
        let id2 = manager.spawn("echo", "b", SpawnOptions::default()).await;

        // Wait for both to complete.
        let _ = rx.recv().await;
        let _ = rx.recv().await;

        let all = manager.list().await;
        assert_eq!(all.len(), 2);
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&id1.as_str()));
        assert!(ids.contains(&id2.as_str()));
    }

    #[tokio::test]
    async fn non_timeout_error_produces_failed_status() {
        // A model that returns an error that doesn't contain "timed out"
        struct FailingModel;

        #[async_trait]
        impl ModelClient for FailingModel {
            async fn complete(
                &self,
                _messages: &[ChatMessage],
                _tools: &[ToolDefinition],
                _options: &ChatOptions,
            ) -> Result<ModelCompletion, String> {
                Err("connection refused".into())
            }
        }

        let broker = make_broker(Arc::new(FailingModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "fail", SpawnOptions::default()).await;

        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(event.session_id, id);
        assert!(event.result.is_err());

        let info = manager.get(&id).await.unwrap();
        // Must be Failed, NOT TimedOut.
        match &info.status {
            SessionStatus::Failed(msg) => assert!(msg.contains("connection refused")),
            other => panic!("expected Failed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn steer_fails_for_completed_session() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "done", SpawnOptions::default()).await;

        // Wait for completion.
        let _ = rx.recv().await;

        // Verify session is completed.
        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.status, SessionStatus::Completed);

        // Steering a completed session should fail.
        let steered = manager.steer(&id, "too late").await;
        assert!(!steered);
    }

    #[tokio::test]
    async fn cleanup_retains_recently_completed_sessions() {
        let broker = make_broker(Arc::new(EchoModel));
        let (manager, mut rx) = SubAgentManager::new(broker);

        let id = manager.spawn("echo", "keep", SpawnOptions::default()).await;
        let _ = rx.recv().await;

        // Cleanup with a large max_age should keep it.
        manager.cleanup(Duration::from_secs(3600)).await;
        assert!(manager.get(&id).await.is_some());
    }

    #[tokio::test]
    async fn cleanup_does_not_remove_running_sessions() {
        let broker = make_broker(Arc::new(SlowModel {
            delay: Duration::from_secs(60),
        }));
        let (manager, _rx) = SubAgentManager::new(broker);

        let id = manager
            .spawn("echo", "running", SpawnOptions::default())
            .await;

        tokio::time::sleep(Duration::from_millis(10)).await;

        // Even with 0 max_age, running sessions are retained.
        manager.cleanup(Duration::from_secs(0)).await;
        let info = manager.get(&id).await.unwrap();
        assert_eq!(info.status, SessionStatus::Running);
    }
}
