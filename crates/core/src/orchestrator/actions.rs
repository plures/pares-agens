//! Orchestrator action handler — IO boundaries for `.px` procedures.
//!
//! This module implements [`AsyncActionHandler`] to provide the side-effect
//! boundary between declarative `.px` procedures (which express orchestrator
//! logic like classification, routing, and context management) and the
//! underlying Rust infrastructure (embedding models, state stores, event bus).
//!
//! # Registered Actions
//!
//! | Action | Params | Returns |
//! |--------|--------|---------|
//! | `compute_embedding` | `{text: string}` | `{embedding: vec<f32>}` |
//! | `cosine_similarity` | `{a: vec<f32>, b: vec<f32>}` | `{similarity: f32}` |
//! | `read_state` | `{key: string}` | `{value: json}` |
//! | `write_state` | `{key: string, value: json}` | `{written: true}` |
//! | `get_current_time` | `{}` | `{timestamp_ms: i64}` |
//! | `emit_event` | `{type: string, payload: json}` | `{emitted: true}` |
//!
//! # Design
//!
//! This is the ONLY Rust code the orchestrator needs for IO — everything else
//! (classification rules, routing decisions, complexity scoring) lives in `.px`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, RwLock};
use std::sync::RwLock as StdRwLock;

use crate::memory::embed::EmbeddingProvider;
use super::px_bridge::PxBridge;
use pares_radix_core::model::StreamDelta;
use pares_radix_core::px_adapter::AsyncActionHandler;
use pares_radix_core::spine::event::SpineEvent;
use pares_radix_praxis::px::executor::ExecutionError;

/// A memory entry with pre-computed embedding for recall.
#[derive(Clone)]
struct MemoryEntry {
    content: String,
    embedding: Vec<f32>,
    metadata: Value,
}

// ── CerebellumActionHandler ──────────────────────────────────────────────────

/// Action handler providing IO boundaries for orchestrator `.px` procedures.
///
/// Each method maps a named action to an async Rust implementation that
/// performs the actual IO (embedding computation, state access, event emission).
/// The `.px` procedures call these by name; this handler is the only bridge.
pub struct CerebellumActionHandler {
    /// Embedding provider for `compute_embedding` action.
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    /// State store for `read_state` / `write_state` actions.
    /// Backed by an in-memory map for now; later migrates to PluresDB.
    state: Arc<RwLock<HashMap<String, Value>>>,
    /// Channel for emitting spine events into the pipeline.
    event_tx: Option<mpsc::Sender<SpineEvent>>,
    /// Model client for `model_complete` action.
    /// Wrapped in RwLock so it can be set after construction (late binding).
    model_client: Arc<StdRwLock<Option<Arc<dyn pares_radix_core::model::ModelClient>>>>,
    /// Tool dispatcher for `dispatch_tools` action.
    tool_dispatcher: Arc<StdRwLock<Option<Arc<dyn pares_radix_core::model::ToolDispatcher>>>>,
    /// Memory store for `recall_memories` action (key: "memories", value: vec of {content, embedding}).
    /// PluresDB replaces this — for now, memory entries live in the state map under "memory:*" keys.
    memory_entries: Arc<RwLock<Vec<MemoryEntry>>>,
    /// Broadcast channel for streaming model deltas to channel handlers (Telegram progressive editing).
    /// Channel handlers subscribe BEFORE triggering the dataflow pipeline.
    /// The model_complete action sends StreamDelta tokens here as they arrive.
    stream_tx: broadcast::Sender<StreamDelta>,
    /// Live-context feed for Chronos debug viewers.
    live_context_tx: broadcast::Sender<Value>,
    /// Session ids whose live feed is suspended while Chronos is inspected.
    paused_live_context_sessions: Arc<RwLock<HashSet<String>>>,
}

/// Routes the live Spine `.px` action surface to its owning implementation.
///
/// The platform composite owns durable state, conversation, task and tool IO.
/// Agens owns cognition-only actions such as classification and routing.  The
/// previous `serve-spine` wiring installed only the platform composite; unknown
/// cognition actions were consequently treated as model tools and failed in the
/// procedure registry.  Keeping this routing explicit preserves the boundary:
/// `.px` still chooses the flow, while each Rust handler performs only its
/// concrete IO or deterministic primitive.
pub struct SpineActionRouter {
    platform: Arc<dyn AsyncActionHandler>,
    cognition: Arc<dyn AsyncActionHandler>,
    /// Named `.px` procedures loaded for direct procedure-to-procedure calls.
    procedure_bridge: RwLock<Option<Arc<PxBridge>>>,
}

impl SpineActionRouter {
    /// Create a router for a live Spine runtime.
    pub fn new(
        platform: Arc<dyn AsyncActionHandler>,
        cognition: Arc<dyn AsyncActionHandler>,
    ) -> Self {
        Self {
            platform,
            cognition,
            procedure_bridge: RwLock::new(None),
        }
    }

    /// Attach the named procedure bridge after the live handler exists.
    ///
    /// The ordering breaks the natural cycle: the bridge needs this router as
    /// its action boundary, and this router needs the loaded bridge to resolve
    /// calls from one `.px` procedure to another.
    pub async fn set_procedure_bridge(&self, bridge: Arc<PxBridge>) {
        *self.procedure_bridge.write().await = Some(bridge);
    }

    fn get_field(params: &Value) -> Result<Value, ExecutionError> {
        let field = params
            .get("field")
            .and_then(Value::as_str)
            .ok_or_else(|| ExecutionError::ActionFailed {
                action: "get_field".to_string(),
                message: "missing field".to_string(),
            })?;
        Ok(params
            .get("object")
            .and_then(Value::as_object)
            .and_then(|object| object.get(field))
            .cloned()
            .or_else(|| params.get("default").cloned())
            .unwrap_or(Value::Null))
    }

    fn append_to_list(params: &Value) -> Result<Value, ExecutionError> {
        let mut list = params
            .get("list")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match params.get("items") {
            Some(Value::Array(items)) => list.extend(items.iter().cloned()),
            Some(item) => list.push(item.clone()),
            None => {
                return Err(ExecutionError::ActionFailed {
                    action: "append_to_list".to_string(),
                    message: "missing items".to_string(),
                });
            }
        }
        Ok(Value::Array(list))
    }

    fn get_last_item(params: &Value) -> Result<Value, ExecutionError> {
        let list = params
            .get("list")
            .and_then(Value::as_array)
            .ok_or_else(|| ExecutionError::ActionFailed {
                action: "get_last_item".to_string(),
                message: "missing list".to_string(),
            })?;
        Ok(list.last().cloned().unwrap_or(Value::Null))
    }

    fn compute_context_budget(params: &Value) -> Result<Value, ExecutionError> {
        let window = params
            .get("window")
            .and_then(Value::as_u64)
            .ok_or_else(|| ExecutionError::ActionFailed {
                action: "compute_context_budget".to_string(),
                message: "missing numeric window".to_string(),
            })?;
        let output = params
            .get("reserve_for_output")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let system = params
            .get("reserve_for_system")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Ok(Value::from(window.saturating_sub(output.saturating_add(system))))
    }

    fn determine_tier(params: &Value) -> Result<Value, ExecutionError> {
        let complexity = params
            .get("complexity")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let has_tools = params
            .get("has_tools")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        Ok(Value::from(if has_tools || complexity >= 5 {
            "premium"
        } else if complexity <= 1 {
            "fast"
        } else {
            "standard"
        }))
    }

    fn filter_relevant(params: &Value) -> Result<Value, ExecutionError> {
        // Constraints are safety guidance. In the absence of a declared PX
        // relevance predicate, preserve them all rather than silently dropping
        // a constraint at the action boundary.
        Ok(params
            .get("constraints")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())))
    }

    fn format_guidance_block(params: &Value) -> Result<Value, ExecutionError> {
        let session_type = params
            .get("session_type")
            .and_then(Value::as_str)
            .unwrap_or("general");
        let constraints = params
            .get("constraints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let entries = constraints
            .iter()
            .map(|constraint| {
                constraint
                    .get("message")
                    .or_else(|| constraint.get("name"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| constraint.to_string())
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            Ok(Value::String(String::new()))
        } else {
            Ok(Value::String(format!(
                "## Praxis guidance ({session_type})\n{}",
                entries
                    .iter()
                    .map(|entry| format!("- {entry}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )))
        }
    }

    fn append_guidance(params: &Value) -> Result<Value, ExecutionError> {
        let base = params.get("base").and_then(Value::as_str).unwrap_or_default();
        let guidance = params
            .get("guidance")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(Value::String(match (base.is_empty(), guidance.is_empty()) {
            (true, _) => guidance.to_string(),
            (_, true) => base.to_string(),
            (false, false) => format!("{base}\n\n{guidance}"),
        }))
    }

    fn style_response(params: &Value) -> Result<Value, ExecutionError> {
        // Personality is declarative policy. The action preserves the model
        // response until a configured renderer is available; it never invents
        // or discards user-facing content.
        Ok(params
            .get("response")
            .cloned()
            .unwrap_or_else(|| Value::String(String::new())))
    }
}

/// Cognition actions that must never be sent through the model-tool registry.
///
/// State and history verbs deliberately remain absent: the platform composite
/// is the sole owner of their shared, durable PluresDB backing store.
const COGNITION_ACTIONS: &[&str] = &[
    "compute_embedding",
    "cosine_similarity",
    "get_current_time",
    "emit_event",
    "pause_live_context_subscription",
    "resume_live_context_subscription",
    "publish_live_context_event",
    "normalize_text",
    "detect_intent",
    "score_complexity",
    "detect_tools_needed",
    "match_plugin",
    "extract_topic",
    "determine_model_tier",
    "classify",
    "model_complete",
    "classify_continuation",
    "classify_intent",
    "word_count",
    "match_patterns",
    "embed_text",
    "recall_memories",
    "recall_context",
    "store_memory",
    "extract_entities",
    "manage_context",
    "build_messages",
    "append_tail",
    "dispatch_tools",
    "build_tool_followup",
    "timestamp_now",
    "format_string",
    "find_most_recent",
    "generate_id",
];

#[async_trait]
impl AsyncActionHandler for SpineActionRouter {
    async fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError> {
        if COGNITION_ACTIONS.contains(&name) {
            return self.cognition.call(name, params).await;
        }

        match name {
            "get_field" => return Self::get_field(params),
            "append_to_list" => return Self::append_to_list(params),
            "get_last_item" => return Self::get_last_item(params),
            "compute_context_budget" => return Self::compute_context_budget(params),
            "determine_tier" => return Self::determine_tier(params),
            "filter_relevant" => return Self::filter_relevant(params),
            "format_guidance_block" => return Self::format_guidance_block(params),
            "append_guidance" => return Self::append_guidance(params),
            "style_response" => return Self::style_response(params),
            // These aliases intentionally share the platform's one durable
            // store; they are not a second, in-memory state implementation.
            "pluresdb_read" | "db_get" => return self.platform.call("read_state", params).await,
            "pluresdb_write" | "db_set" => return self.platform.call("write_state", params).await,
            "db_get_prefix" => return self.platform.call("read_state_prefix", params).await,
            _ => {}
        }

        // `.px` helpers (for example `route_dispatch`, `classify_message` and
        // `filter_leaf_tasks`) are procedure calls, not model tools.  Marshal
        // their named parameters into the bridge before consulting platform IO.
        let procedure_bridge = self.procedure_bridge.read().await.clone();
        if let Some(bridge) = procedure_bridge {
            let vars = params
                .as_object()
                .map(|values| {
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(result) = bridge.call(name, vars).await {
                return result.map_err(|message| ExecutionError::ActionFailed {
                    action: name.to_string(),
                    message,
                });
            }
        }

        self.platform.call(name, params).await
    }
}

impl CerebellumActionHandler {
    /// Create a new handler with all IO dependencies.
    pub fn new(
        embedder: Option<Arc<dyn EmbeddingProvider>>,
        event_tx: Option<mpsc::Sender<SpineEvent>>,
    ) -> Self {
        let (stream_tx, _) = broadcast::channel(256);
        let (live_context_tx, _) = broadcast::channel(256);
        Self {
            embedder,
            state: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            model_client: Arc::new(StdRwLock::new(None)),
            tool_dispatcher: Arc::new(StdRwLock::new(None)),
            memory_entries: Arc::new(RwLock::new(Vec::new())),
            stream_tx,
            live_context_tx,
            paused_live_context_sessions: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Create a minimal handler for testing (no embedder, no event channel).
    #[cfg(test)]
    pub fn for_testing() -> Self {
        let (stream_tx, _) = broadcast::channel(256);
        let (live_context_tx, _) = broadcast::channel(256);
        Self {
            embedder: None,
            state: Arc::new(RwLock::new(HashMap::new())),
            event_tx: None,
            model_client: Arc::new(StdRwLock::new(None)),
            tool_dispatcher: Arc::new(StdRwLock::new(None)),
            memory_entries: Arc::new(RwLock::new(Vec::new())),
            stream_tx,
            live_context_tx,
            paused_live_context_sessions: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Create a minimal handler with no embedder or event channel.
    pub fn new_minimal() -> Self {
        let (stream_tx, _) = broadcast::channel(256);
        let (live_context_tx, _) = broadcast::channel(256);
        Self {
            embedder: None,
            state: Arc::new(RwLock::new(HashMap::new())),
            event_tx: None,
            model_client: Arc::new(StdRwLock::new(None)),
            tool_dispatcher: Arc::new(StdRwLock::new(None)),
            memory_entries: Arc::new(RwLock::new(Vec::new())),
            stream_tx,
            live_context_tx,
            paused_live_context_sessions: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Attach a model client to enable `model_complete` action.
    /// Can be called after construction (late binding pattern).
    pub fn with_model_client(self, client: Arc<dyn pares_radix_core::model::ModelClient>) -> Self {
        *self.model_client.write().unwrap() = Some(client);
        self
    }

    /// Set the model client after construction (for late binding when
    /// the model client isn't available at orchestrator init time).
    pub fn set_model_client(&self, client: Arc<dyn pares_radix_core::model::ModelClient>) {
        *self.model_client.write().unwrap() = Some(client);
    }

    /// Attach a tool dispatcher to enable `dispatch_tools` action.
    pub fn with_tool_dispatcher(self, dispatcher: Arc<dyn pares_radix_core::model::ToolDispatcher>) -> Self {
        *self.tool_dispatcher.write().unwrap() = Some(dispatcher);
        self
    }

    /// Set the tool dispatcher after construction (late binding).
    pub fn set_tool_dispatcher(&self, dispatcher: Arc<dyn pares_radix_core::model::ToolDispatcher>) {
        *self.tool_dispatcher.write().unwrap() = Some(dispatcher);
    }

    /// Store a memory entry for later recall.
    pub async fn store_memory(&self, content: &str, embedding: Vec<f32>, metadata: Value) {
        let entry = MemoryEntry {
            content: content.to_string(),
            embedding,
            metadata,
        };
        self.memory_entries.write().await.push(entry);
    }

    /// Create a handler with a pre-populated state map (useful for testing).
    #[cfg(test)]
    pub fn with_state(state: HashMap<String, Value>) -> Self {
        let (stream_tx, _) = broadcast::channel(256);
        let (live_context_tx, _) = broadcast::channel(256);
        Self {
            embedder: None,
            state: Arc::new(RwLock::new(state)),
            event_tx: None,
            model_client: Arc::new(StdRwLock::new(None)),
            tool_dispatcher: Arc::new(StdRwLock::new(None)),
            memory_entries: Arc::new(RwLock::new(Vec::new())),
            stream_tx,
            live_context_tx,
            paused_live_context_sessions: Arc::new(RwLock::new(HashSet::new())),
        }
    }


    /// Subscribe to live context events (payload includes `session_id` for client-side filtering).
    pub fn subscribe_live_context(&self) -> broadcast::Receiver<Value> {
        self.live_context_tx.subscribe()
    }

    /// Publish an observed agent lifecycle event to attached Chronos viewers.
    /// The caller supplies facts only; viewer policy remains in `.px`.
    pub async fn publish_live_context(&self, session_id: &str, event: Value) -> Result<Value, ExecutionError> {
        self.publish_live_context_event(&json!({"session_id": session_id, "event": event})).await
    }

    /// Pause live context delivery for a session in this process.
    /// No `.px` policy/authorization is applied here; callers must gate access.
    pub async fn pause_live_context(&self, session_id: &str) -> Result<Value, ExecutionError> {
        self.pause_live_context_subscription(&json!({"session_id": session_id})).await
    }

    /// Resume live context for a debug session.
    pub async fn resume_live_context(&self, session_id: &str) -> Result<Value, ExecutionError> {
        self.resume_live_context_subscription(&json!({"session_id": session_id})).await
    }

    async fn pause_live_context_subscription(&self, params: &Value) -> Result<Value, ExecutionError> {
        let session_id = Self::live_context_session_id(params)?;
        let changed = self.paused_live_context_sessions.write().await.insert(session_id);
        Ok(json!({"paused": true, "changed": changed}))
    }

    async fn resume_live_context_subscription(&self, params: &Value) -> Result<Value, ExecutionError> {
        let session_id = Self::live_context_session_id(params)?;
        let changed = self.paused_live_context_sessions.write().await.remove(&session_id);
        Ok(json!({"paused": false, "changed": changed}))
    }

    async fn publish_live_context_event(&self, params: &Value) -> Result<Value, ExecutionError> {
        let session_id = Self::live_context_session_id(params)?;
        let event = params.get("event").cloned().ok_or_else(|| ExecutionError::ActionFailed {
            action: "publish_live_context_event".to_string(),
            message: "missing required param: event".to_string(),
        })?;
        if self.paused_live_context_sessions.read().await.contains(&session_id) {
            return Ok(json!({"delivered": false, "paused": true}));
        }
        let _ = self.live_context_tx.send(json!({"session_id": session_id, "event": event}));
        Ok(json!({"delivered": true, "paused": false}))
    }

    fn live_context_session_id(params: &Value) -> Result<String, ExecutionError> {
        params.get("session_id").and_then(Value::as_str).filter(|id| !id.is_empty())
            .map(str::to_owned).ok_or_else(|| ExecutionError::ActionFailed {
                action: "live_context_session_id".to_string(),
                message: "missing required param: session_id".to_string(),
            })
    }

    /// Subscribe to model streaming deltas.
    ///
    /// Channel handlers (e.g. Telegram) call this BEFORE triggering the dataflow
    /// pipeline. When `model_complete` fires internally, it sends [`StreamDelta`]
    /// tokens through this broadcast channel, enabling progressive message editing.
    ///
    /// The receiver is bounded (256 items). If the consumer is too slow, it will
    /// receive `RecvError::Lagged` and can skip to the latest state.
    pub fn subscribe_stream(&self) -> broadcast::Receiver<StreamDelta> {
        self.stream_tx.subscribe()
    }

    /// Get the broadcast sender for stream deltas.
    ///
    /// Used to pass the sender to channel adapters at construction time,
    /// enabling them to subscribe independently for progressive delivery.
    pub fn stream_sender(&self) -> broadcast::Sender<StreamDelta> {
        self.stream_tx.clone()
    }

    /// Replace the internal stream broadcast sender with an external one.
    ///
    /// Use this to share a single broadcast channel between the action handler
    /// and channel adapters (Telegram, etc.) that need to receive stream deltas.
    pub fn set_stream_sender(&mut self, tx: broadcast::Sender<StreamDelta>) {
        self.stream_tx = tx;
    }

    // ── Action implementations ───────────────────────────────────────────────

    async fn compute_embedding(&self, params: &Value) -> Result<Value, ExecutionError> {
        let text = params.get("text").and_then(|v| v.as_str()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "compute_embedding".to_string(),
                message: "missing required param: text (string)".to_string(),
            }
        })?;

        let embedder = self
            .embedder
            .as_ref()
            .ok_or_else(|| ExecutionError::ActionFailed {
                action: "compute_embedding".to_string(),
                message: "no embedding provider configured".to_string(),
            })?;

        let embedding = embedder
            .embed(text)
            .await
            .map_err(|e| ExecutionError::ActionFailed {
                action: "compute_embedding".to_string(),
                message: e.to_string(),
            })?;

        Ok(json!({ "embedding": embedding }))
    }

    fn cosine_similarity_impl(params: &Value) -> Result<Value, ExecutionError> {
        let a = params.get("a").and_then(|v| v.as_array()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "cosine_similarity".to_string(),
                message: "missing required param: a (array of floats)".to_string(),
            }
        })?;

        let b = params.get("b").and_then(|v| v.as_array()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "cosine_similarity".to_string(),
                message: "missing required param: b (array of floats)".to_string(),
            }
        })?;

        let a_vec: Vec<f32> = a.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
        let b_vec: Vec<f32> = b.iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();

        if a_vec.len() != b_vec.len() {
            return Err(ExecutionError::ActionFailed {
                action: "cosine_similarity".to_string(),
                message: format!(
                    "vector dimension mismatch: a={}, b={}",
                    a_vec.len(),
                    b_vec.len()
                ),
            });
        }

        let similarity = cosine_similarity(&a_vec, &b_vec);
        Ok(json!({ "similarity": similarity }))
    }

    async fn read_state(&self, params: &Value) -> Result<Value, ExecutionError> {
        let key = params.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "read_state".to_string(),
                message: "missing required param: key (string)".to_string(),
            }
        })?;

        let state = self.state.read().await;
        let value = state.get(key).cloned().unwrap_or(Value::Null);
        Ok(json!({ "value": value }))
    }

    /// Append a single message to a channel's persisted `chat_history:{chat_id}`
    /// state entry (read-modify-write against the shared state store).
    ///
    /// This is the fix for the amnesia bug: `append_history` used to be
    /// aliased directly to `write_state`, but `write_state` requires a
    /// literal `key`/`value` pair while every call site passes
    /// `{chat_id, role, content}` — so every append silently failed with
    /// `ActionFailed: missing required param: key`, and `chat_history:{id}`
    /// was never populated. `assemble_context`/`dispatch_steered_task`
    /// always read back an empty history, which is why the agent forgot
    /// tasks/context from the immediately preceding turn.
    async fn append_history(&self, params: &Value) -> Result<Value, ExecutionError> {
        let chat_id = params.get("chat_id").and_then(|v| v.as_str()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "append_history".to_string(),
                message: "missing required param: chat_id (string)".to_string(),
            }
        })?;
        let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or_default();

        let key = format!("chat_history:{chat_id}");
        let mut state = self.state.write().await;
        let mut history = state
            .get(&key)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        history.push(json!({"role": role, "content": content}));
        const MAX_HISTORY_ENTRIES: usize = 40;
        if history.len() > MAX_HISTORY_ENTRIES {
            history = history[history.len() - MAX_HISTORY_ENTRIES..].to_vec();
        }
        state.insert(key, json!(history));
        Ok(json!({ "written": true }))
    }

    async fn write_state(&self, params: &Value) -> Result<Value, ExecutionError> {
        let key = params.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "write_state".to_string(),
                message: "missing required param: key (string)".to_string(),
            }
        })?;

        let value = params.get("value").cloned().unwrap_or(Value::Null);

        let mut state = self.state.write().await;
        state.insert(key.to_string(), value);
        Ok(json!({ "written": true }))
    }

    fn get_current_time() -> Result<Value, ExecutionError> {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| ExecutionError::ActionFailed {
                action: "get_current_time".to_string(),
                message: e.to_string(),
            })?
            .as_millis() as i64;

        Ok(json!({ "timestamp_ms": timestamp_ms }))
    }

    async fn emit_event(&self, params: &Value) -> Result<Value, ExecutionError> {
        let event_type = params.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "emit_event".to_string(),
                message: "missing required param: type (string)".to_string(),
            }
        })?;

        let payload = params.get("payload").cloned().unwrap_or_else(|| json!({}));

        let tx = self
            .event_tx
            .as_ref()
            .ok_or_else(|| ExecutionError::ActionFailed {
                action: "emit_event".to_string(),
                message: "no event channel configured".to_string(),
            })?;

        // Construct a SpineEvent based on the requested type.
        // For now, all orchestrator-emitted events are modelled as ModelRequest
        // (the primary use case is requesting model invocation from .px logic).
        let spine_event = match event_type {
            "model_request" => SpineEvent::ModelRequest {
                id: SpineEvent::new_id(),
                source: "orchestrator".to_string(),
                chat_id: payload
                    .get("chat_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("orchestrator")
                    .to_string(),
                sender: "orchestrator".to_string(),
                content: payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                system_prompt: payload
                    .get("system_prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                metadata: payload,
            },
            _ => SpineEvent::Inbound {
                id: SpineEvent::new_id(),
                source: "orchestrator".to_string(),
                chat_id: payload
                    .get("chat_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("orchestrator")
                    .to_string(),
                sender: "orchestrator".to_string(),
                content: json!({ "type": event_type, "payload": payload }).to_string(),
                metadata: json!({ "emitted_by": "cerebellum_action_handler" }),
            },
        };

        tx.send(spine_event)
            .await
            .map_err(|e| ExecutionError::ActionFailed {
                action: "emit_event".to_string(),
                message: format!("failed to send event to pipeline: {e}"),
            })?;

        Ok(json!({ "emitted": true }))
    }

    // ── Dataflow classification actions ───────────────────────────────────────

    /// Normalize text: lowercase, trim whitespace.
    fn normalize_text(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        Ok(json!(text.to_lowercase().trim().to_string()))
    }

    /// Detect intent from text: question, command, statement, greeting, farewell.
    fn detect_intent(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        let intent = if text.ends_with('?') || text.starts_with("what ") || text.starts_with("how ")
            || text.starts_with("why ") || text.starts_with("when ") || text.starts_with("where ")
            || text.starts_with("who ") || text.starts_with("can you")
        {
            "question"
        } else if text.starts_with('/') || text.starts_with("do ") || text.starts_with("run ")
            || text.starts_with("execute ") || text.starts_with("create ")
            || text.starts_with("make ") || text.starts_with("build ")
            || text.starts_with("deploy ") || text.starts_with("fix ")
        {
            "command"
        } else if text.starts_with("hi") || text.starts_with("hey") || text.starts_with("hello") {
            "greeting"
        } else if text.starts_with("bye") || text.starts_with("goodbye") || text.starts_with("see you") {
            "farewell"
        } else {
            "statement"
        };
        Ok(json!(intent))
    }

    /// Score complexity 0-6 based on structural cues.
    fn score_complexity(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        let words: Vec<&str> = text.split_whitespace().collect();
        let word_count = words.len();
        let mut score: u32 = 0;

        // Length factor
        if word_count > 30 {
            score += 2;
        } else if word_count > 8 {
            score += 1;
        }

        // Reasoning words
        let reasoning = ["because", "therefore", "however", "although", "whereas",
            "analyze", "compare", "evaluate", "explain", "consider"];
        if words.iter().any(|w| reasoning.contains(&w.to_lowercase().as_str())) {
            score += 1;
        }

        // Multi-step markers
        let step_markers = ["first", "then", "next", "finally", "after", "before",
            "step", "1.", "2.", "3."];
        let step_count = words.iter().filter(|w| step_markers.contains(&w.to_lowercase().as_str())).count();
        if step_count >= 2 {
            score += 1;
        }

        // Code markers
        if text.contains('`') || text.contains("fn ") || text.contains("def ")
            || text.contains("->") || text.contains("::") || text.contains("impl ")
        {
            score += 1;
        }

        // Multi-clause
        let clauses = text.matches(',').count() + text.matches(';').count()
            + text.matches(" and ").count() + text.matches(" or ").count();
        if clauses >= 3 {
            score += 1;
        }

        Ok(json!(score.min(6)))
    }

    /// Detect if tools are needed based on text patterns.
    fn detect_tools_needed(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        let needs_tools = text.contains("search") || text.contains("browse")
            || text.contains("fetch") || text.contains("download")
            || text.contains("run ") || text.contains("execute")
            || text.contains("compile") || text.contains("build")
            || text.contains("deploy") || text.contains("commit")
            || text.contains("push") || text.contains("pull")
            || text.starts_with('/');
        Ok(json!(needs_tools))
    }

    /// Match against known plugin/tool patterns.
    fn match_plugin(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        let plugin = if text.contains("weather") {
            "weather"
        } else if text.contains("calendar") || text.contains("schedule") {
            "calendar"
        } else if text.contains("email") || text.contains("mail") {
            "email"
        } else if text.contains("git") || text.contains("repo") || text.contains("pr ") {
            "git"
        } else if text.contains("memory") || text.contains("remember") {
            "memory"
        } else {
            "none"
        };
        Ok(json!(plugin))
    }

    /// Extract topic from text (first noun phrase heuristic).
    fn extract_topic(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        // Simple: take the first 3-5 significant words
        let stop_words = ["the", "a", "an", "is", "are", "was", "were", "do", "does",
            "did", "to", "of", "in", "on", "at", "for", "with", "and", "or", "but",
            "can", "you", "i", "me", "my", "it", "this", "that"];
        let significant: Vec<&str> = text.split_whitespace()
            .filter(|w| !stop_words.contains(&w.to_lowercase().as_str()))
            .take(4)
            .collect();
        Ok(json!(significant.join(" ")))
    }

    // REMOVED: detect-topic-shift-action (dead code, ADR-0015). This was a
    // constant-`false` placeholder reachable only via the unwired
    // classify.px `classify_message` procedure, which has no production
    // caller. The real topic-shift implementation is
    // `Orchestrator::detect_topic_shift` in `orchestrator/mod.rs`.

    /// Determine model tier based on complexity score.
    fn determine_model_tier(params: &Value) -> Result<Value, ExecutionError> {
        let complexity = params["complexity"].as_u64().unwrap_or(0);
        let needs_deep = complexity > 3;
        Ok(json!(needs_deep))
    }

    /// Generic classify action (combines intent + complexity + tools).
    fn classify_action(params: &Value) -> Result<Value, ExecutionError> {
        let text = params["text"].as_str().unwrap_or_default();
        let intent = Self::detect_intent(&json!({"text": text}))?;
        let complexity = Self::score_complexity(&json!({"text": text}))?;
        let needs_tools = Self::detect_tools_needed(&json!({"text": text}))?;
        Ok(json!({
            "intent": intent,
            "complexity": complexity,
            "needs_tools": needs_tools,
        }))
    }

    // ── Unified Router & Task Steering actions ────────────────────────────────

    /// Classify whether a message is a continuation of an existing task.
    /// Mirrors task-steering.px logic as a Rust fallback until PxBridge wires fully.
    async fn classify_continuation_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let message = params.get("message").and_then(|v| v.as_str()).unwrap_or_default();
        let lower = message.to_lowercase();

        // Read promises from state
        let promises = self.read_state(&json!({"key": "agent_promises"})).await
            .ok().and_then(|v| v.get("value").cloned())
            .filter(|v| !v.is_null());

        let tasks = self.read_state(&json!({"key": "active_tasks"})).await
            .ok().and_then(|v| v.get("value").cloned())
            .filter(|v| !v.is_null());

        // No promises or tasks → always new request
        if promises.is_none() && tasks.is_none() {
            return Ok(json!({
                "is_continuation": false,
                "confidence": 1.0,
                "target_task_id": null,
                "intent": "new_request"
            }));
        }

        // Confirmation patterns
        let confirm_patterns = [
            "do it", "yes", "go ahead", "proceed", "fix it", "do that",
            "go for it", "make it happen", "execute", "run it", "ship it",
            "start", "begin", "let's go", "yep", "yeah", "confirmed",
            "approved", "do both", "do all", "continue", "keep going",
        ];
        if confirm_patterns.iter().any(|p| lower.contains(p)) {
            return Ok(json!({
                "is_continuation": true,
                "confidence": 0.95,
                "target_task_id": null,
                "intent": "confirm"
            }));
        }

        // Cancel patterns
        let cancel_patterns = ["never mind", "cancel", "stop", "don't", "abort", "forget it", "scratch that"];
        if cancel_patterns.iter().any(|p| lower.contains(p)) {
            return Ok(json!({
                "is_continuation": true,
                "confidence": 0.9,
                "target_task_id": null,
                "intent": "cancel"
            }));
        }

        // Redirect patterns
        let redirect_patterns = ["actually", "instead", "focus on", "prioritize", "switch to"];
        if redirect_patterns.iter().any(|p| lower.contains(p)) && message.len() > 15 {
            return Ok(json!({
                "is_continuation": true,
                "confidence": 0.8,
                "target_task_id": null,
                "intent": "redirect"
            }));
        }

        // Short messages after promises = likely continuation
        if message.split_whitespace().count() <= 5 && promises.is_some() {
            return Ok(json!({
                "is_continuation": true,
                "confidence": 0.7,
                "target_task_id": null,
                "intent": "confirm"
            }));
        }

        Ok(json!({
            "is_continuation": false,
            "confidence": 0.6,
            "target_task_id": null,
            "intent": "new_request"
        }))
    }

    /// Word count.
    fn word_count_action(params: &Value) -> Result<Value, ExecutionError> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or_default();
        Ok(json!(text.split_whitespace().count()))
    }

    /// Match text against a list of patterns (returns true if any match).
    fn match_patterns_action(params: &Value) -> Result<Value, ExecutionError> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or_default().to_lowercase();
        let patterns = params.get("patterns").and_then(|v| v.as_array());
        if let Some(pats) = patterns {
            let matched = pats.iter().any(|p| {
                p.as_str().is_some_and(|s| text.contains(s))
            });
            Ok(json!(matched))
        } else {
            Ok(json!(false))
        }
    }

    /// Recall memories via embedding similarity search.
    /// Embeds the query text, then finds closest memories by cosine similarity.
    async fn recall_memories_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let min_score = params.get("min_score").and_then(|v| v.as_f64()).unwrap_or(0.5);

        // Get the query embedding (either passed directly or computed from text)
        let query_embedding: Vec<f32> = if let Some(emb) = params.get("embedding").and_then(|v| v.as_array()) {
            emb.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect()
        } else if let Some(text) = params.get("text").and_then(|v| v.as_str()) {
            // Compute embedding from text
            let Some(embedder) = &self.embedder else {
                return Ok(json!({"memories": []}));
            };
            match embedder.embed(text).await {
                Ok(emb) => emb,
                Err(_) => return Ok(json!({"memories": []})),
            }
        } else {
            return Ok(json!({"memories": []}));
        };

        // Search memory entries by cosine similarity
        let entries = self.memory_entries.read().await;
        let mut scored: Vec<(f64, &MemoryEntry)> = entries.iter()
            .map(|entry| {
                let sim = cosine_similarity(&query_embedding, &entry.embedding) as f64;
                (sim, entry)
            })
            .filter(|(score, _)| *score >= min_score)
            .collect();

        // Sort by similarity descending
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        let memories: Vec<Value> = scored.iter()
            .map(|(score, entry)| json!({
                "content": entry.content,
                "score": score,
                "metadata": entry.metadata,
            }))
            .collect();

        Ok(json!({"memories": memories}))
    }

    /// Compatibility boundary for the legacy preprocessing procedure.
    ///
    /// The procedure provides extracted entities/classification rather than a
    /// ready embedding. Convert that durable context into a recall query, then
    /// reuse the sole memory-recall implementation and return its item list.
    async fn recall_context_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let mut terms = Vec::new();
        if let Some(entities) = params
            .get("entities")
            .and_then(|value| value.get("entities").or(Some(value)))
            .and_then(Value::as_array)
        {
            terms.extend(entities.iter().filter_map(|entity| {
                entity
                    .get("value")
                    .or_else(|| entity.get("name"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            }));
        }
        if let Some(intent) = params
            .get("classification")
            .and_then(|value| value.get("intent").or(Some(value)))
            .and_then(Value::as_str)
        {
            terms.push(intent.to_string());
        }

        let recalled = self
            .recall_memories_action(&json!({
                "text": terms.join(" "),
                "limit": params.get("limit").cloned().unwrap_or_else(|| json!(10)),
            }))
            .await?;
        Ok(recalled
            .get("memories")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())))
    }

    /// Store a memory entry with auto-computed embedding.
    async fn store_memory_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or_default();
        if content.is_empty() {
            return Err(ExecutionError::ActionFailed {
                action: "store_memory".to_string(),
                message: "content parameter is required".to_string(),
            });
        }

        let metadata = params.get("metadata").cloned().unwrap_or(json!({}));

        // Compute embedding
        let embedding = if let Some(embedder) = &self.embedder {
            embedder.embed(content).await.unwrap_or_default()
        } else {
            vec![]
        };

        self.memory_entries.write().await.push(MemoryEntry {
            content: content.to_string(),
            embedding,
            metadata,
        });

        Ok(json!({"stored": true}))
    }

    /// Dispatch tool calls to the tool dispatcher and return results.
    async fn dispatch_tools_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let calls = params.get("calls").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        let dispatcher = self.tool_dispatcher.read().unwrap().clone();
        let Some(dispatcher) = dispatcher else {
            return Err(ExecutionError::ActionFailed {
                action: "dispatch_tools".to_string(),
                message: "no tool dispatcher configured".to_string(),
            });
        };

        let mut results = Vec::new();
        for call in &calls {
            let name = call.get("name").or(call.get("function").and_then(|f| f.get("name")))
                .and_then(|v| v.as_str()).unwrap_or_default();
            let arguments = call.get("arguments").or(call.get("function").and_then(|f| f.get("arguments")))
                .cloned().unwrap_or(json!({}));
            let call_id = call.get("id").and_then(|v| v.as_str()).unwrap_or_default();

            let result = dispatcher.call_tool(name, arguments).await;
            results.push(json!({
                "tool_call_id": call_id,
                "name": name,
                "content": result,
            }));
        }

        Ok(json!({"results": results}))
    }

    /// Extract entities from text (lightweight NER).
    fn extract_entities_action(params: &Value) -> Result<Value, ExecutionError> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or_default();
        let mut entities = vec![];
        // Simple pattern extraction (file paths, URLs, @mentions, #tags)
        for word in text.split_whitespace() {
            if word.starts_with('/') || word.starts_with("C:\\") || word.starts_with("~/") {
                entities.push(json!({"kind": "path", "value": word}));
            } else if word.starts_with("http") {
                entities.push(json!({"kind": "url", "value": word}));
            } else if word.starts_with('@') {
                entities.push(json!({"kind": "mention", "value": word}));
            } else if word.starts_with('#') {
                entities.push(json!({"kind": "tag", "value": word}));
            }
        }
        Ok(json!({"entities": entities}))
    }

    /// Manage context window: trim to token budget.
    fn manage_context_action(params: &Value) -> Result<Value, ExecutionError> {
        let memories = params.get("memories").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let token_budget = params.get("token_budget").and_then(|v| v.as_u64()).unwrap_or(4096) as usize;
        // Simple: take memories up to ~token budget (estimate 4 chars per token)
        let mut context_str = String::new();
        let mut tokens_used = 0;
        for mem in &memories {
            let content = mem.get("content").and_then(|v| v.as_str()).unwrap_or_default();
            let est_tokens = content.len() / 4;
            if tokens_used + est_tokens > token_budget {
                break;
            }
            context_str.push_str(content);
            context_str.push('\n');
            tokens_used += est_tokens;
        }
        Ok(json!({"context": context_str, "tokens_used": tokens_used}))
    }

    /// Build message array for model invocation.
    fn build_messages_action(params: &Value) -> Result<Value, ExecutionError> {
        let mut messages = vec![];
        if let Some(system) = params.get("system").and_then(|v| v.as_str()) {
            if !system.is_empty() {
                messages.push(json!({"role": "system", "content": system}));
            }
        }
        if let Some(context) = params.get("context").and_then(|v| v.as_str()) {
            if !context.is_empty() {
                messages.push(json!({"role": "system", "content": format!("## Context\n{}", context)}));
            }
        }
        if let Some(history) = params.get("history").and_then(|v| v.as_array()) {
            for msg in history {
                messages.push(msg.clone());
            }
        }
        if let Some(user_msg) = params.get("user_message").and_then(|v| v.as_str()) {
            messages.push(json!({"role": "user", "content": user_msg}));
        }
        Ok(json!(messages))
    }

    /// Append to conversation tail (ring buffer of last N messages).
    fn append_tail_action(params: &Value) -> Result<Value, ExecutionError> {
        let mut tail = params.get("tail").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let role = params.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = params.get("content").and_then(|v| v.as_str()).unwrap_or_default();
        let max = params.get("max").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

        tail.push(json!({"role": role, "content": content}));
        if tail.len() > max {
            tail = tail[tail.len() - max..].to_vec();
        }
        Ok(json!(tail))
    }

    /// Build tool followup request.
    fn build_tool_followup_action(params: &Value) -> Result<Value, ExecutionError> {
        let tool_results = params.get("tool_results").cloned().unwrap_or(json!([]));
        Ok(json!({
            "messages": [{"role": "tool", "content": tool_results.to_string()}],
            "model_tier": "standard",
            "streaming": true,
            "source": "tool_followup",
            "task_id": null
        }))
    }

    /// Format a template string with variable substitution.
    fn format_string_action(params: &Value) -> Result<Value, ExecutionError> {
        let template = params.get("template").and_then(|v| v.as_str()).unwrap_or_default();
        let vars = params.get("vars").and_then(|v| v.as_object());
        let mut result = template.to_string();
        if let Some(vars) = vars {
            for (key, val) in vars {
                let replacement = val.as_str().map(|s| s.to_string())
                    .unwrap_or_else(|| val.to_string());
                result = result.replace(&format!("{{{}}}", key), &replacement);
            }
        }
        Ok(json!(result))
    }

    /// Find most recent task/promise ID.
    fn find_most_recent_action(params: &Value) -> Result<Value, ExecutionError> {
        let tasks = params.get("tasks").and_then(|v| v.as_array());
        let promises = params.get("promises").and_then(|v| v.as_array());

        // Try tasks first (sorted by created_at desc)
        if let Some(tasks) = tasks {
            if let Some(last) = tasks.last() {
                if let Some(id) = last.get("id").or(last.get("task_id")).and_then(|v| v.as_str()) {
                    return Ok(json!(id));
                }
            }
        }
        // Fall back to promises
        if let Some(promises) = promises {
            if let Some(last) = promises.last() {
                if let Some(id) = last.get("task_id").and_then(|v| v.as_str()) {
                    return Ok(json!(id));
                }
            }
        }
        Ok(json!(null))
    }

    /// Call the model client with messages and return the completion.
    async fn model_complete_action(&self, params: &Value) -> Result<Value, ExecutionError> {
        let client = self.model_client.read().unwrap().clone().ok_or_else(|| {
            ExecutionError::ActionFailed {
                action: "model_complete".to_string(),
                message: "no model client attached (call set_model_client first)".to_string(),
            }
        })?;

        // Extract messages from params
        let messages_raw = params.get("messages").cloned().unwrap_or(json!([]));
        let system_prompt = params.get("system_prompt")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let tier = params.get("tier")
            .and_then(|v| v.as_str())
            .unwrap_or("standard");

        // Build ChatMessage list
        use pares_radix_core::model::{ChatMessage, ChatOptions};
        let mut chat_messages: Vec<ChatMessage> = vec![];

        if !system_prompt.is_empty() {
            chat_messages.push(ChatMessage::system(system_prompt));
        }

        // Parse raw messages array
        if let Some(arr) = messages_raw.as_array() {
            for msg in arr {
                let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                let content = msg.get("content")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_string();
                match role {
                    "system" => chat_messages.push(ChatMessage::system(content)),
                    "assistant" => chat_messages.push(ChatMessage::assistant(content)),
                    _ => chat_messages.push(ChatMessage::user(content)),
                }
            }
        }

        let options = ChatOptions {
            temperature: match tier {
                "premium" => Some(0.7),
                "fast" => Some(0.3),
                _ => Some(0.5),
            },
            ..Default::default()
        };

        // Use streaming completion so channel handlers can progressively edit messages.
        // The broadcast sender emits StreamDelta tokens as they arrive; subscribers
        // (e.g. Telegram progressive editor) consume them concurrently.
        let stream_tx = self.stream_tx.clone();

        // Bridge: mpsc receiver forwards to broadcast sender
        let (delta_tx, mut delta_rx) = mpsc::unbounded_channel::<StreamDelta>();
        let broadcast_handle = tokio::spawn(async move {
            while let Some(delta) = delta_rx.recv().await {
                let is_done = matches!(delta, StreamDelta::Done);
                // Best-effort broadcast — no receivers is fine (nobody subscribed)
                let _ = stream_tx.send(delta);
                if is_done {
                    break;
                }
            }
        });

        match client.complete_stream(&chat_messages, &[], &options, delta_tx).await {
            Ok(completion) => {
                // Wait for broadcast forwarder to drain
                let _ = broadcast_handle.await;
                Ok(json!({
                    "content": completion.content,
                    "model": completion.model,
                    "tier": tier,
                }))
            }
            Err(e) => {
                // Signal Done on error so subscribers don't hang
                let _ = self.stream_tx.send(StreamDelta::Done);
                let _ = broadcast_handle.await;
                Err(ExecutionError::ActionFailed {
                    action: "model_complete".to_string(),
                    message: e.to_string(),
                })
            }
        }
    }
}

#[async_trait]
impl AsyncActionHandler for CerebellumActionHandler {
    async fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError> {
        match name {
            "compute_embedding" => self.compute_embedding(params).await,
            "cosine_similarity" => Self::cosine_similarity_impl(params),
            "read_state" => self.read_state(params).await,
            "write_state" => self.write_state(params).await,
            "get_current_time" => Self::get_current_time(),
            "emit_event" => self.emit_event(params).await,
            "pause_live_context_subscription" => self.pause_live_context_subscription(params).await,
            "resume_live_context_subscription" => self.resume_live_context_subscription(params).await,
            "publish_live_context_event" => self.publish_live_context_event(params).await,
            // Dataflow classification actions
            "normalize_text" => Self::normalize_text(params),
            "detect_intent" => Self::detect_intent(params),
            "score_complexity" => Self::score_complexity(params),
            "detect_tools_needed" => Self::detect_tools_needed(params),
            "match_plugin" => Self::match_plugin(params),
            "extract_topic" => Self::extract_topic(params),
            // REMOVED: "detect_topic_shift" (ADR-0015, dead code deletion).
            "determine_model_tier" => Self::determine_model_tier(params),
            "classify" => Self::classify_action(params),
            "model_complete" => self.model_complete_action(params).await,
            // Unified router actions (unified-router.px, task-steering.px)
            "classify_continuation" => self.classify_continuation_action(params).await,
            "classify_intent" => Self::detect_intent(params),
            "word_count" => Self::word_count_action(params),
            "match_patterns" => Self::match_patterns_action(params),
            "embed_text" => self.compute_embedding(params).await,
            "recall_memories" => self.recall_memories_action(params).await,
            "recall_context" => self.recall_context_action(params).await,
            "store_memory" => self.store_memory_action(params).await,
            "extract_entities" => Self::extract_entities_action(params),
            "manage_context" => Self::manage_context_action(params),
            "build_messages" => Self::build_messages_action(params),
            "append_history" => self.append_history(params).await,
            "append_tail" => Self::append_tail_action(params),
            "channel_send" => Ok(json!({"sent": true})), // Handled by graph output, not inline
            "dispatch_tools" => self.dispatch_tools_action(params).await,
            "build_tool_followup" => Self::build_tool_followup_action(params),
            "push_queue" => Ok(json!({"pushed": true})), // Graph handles queue routing
            "timestamp_now" => Self::get_current_time(),
            "format_string" => Self::format_string_action(params),
            "find_most_recent" => Self::find_most_recent_action(params),
            "generate_id" => Ok(json!(uuid::Uuid::new_v4().to_string())),
            _ => Err(ExecutionError::UnknownAction(name.to_string())),
        }
    }
}

// ── Pure math ────────────────────────────────────────────────────────────────

/// Compute cosine similarity between two vectors.
///
/// Returns 0.0 for empty or mismatched vectors, and handles zero-magnitude
/// vectors gracefully.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let (dot, norm_a_sq, norm_b_sq) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(dot, na, nb), (&x, &y)| {
            (dot + x * y, na + x * x, nb + y * y)
        });

    let norm_a = norm_a_sq.sqrt();
    let norm_b = norm_b_sq.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    struct PlatformMarker;

    #[async_trait]
    impl AsyncActionHandler for PlatformMarker {
        async fn call(&self, name: &str, _params: &Value) -> Result<Value, ExecutionError> {
            Ok(json!({"platform_action": name}))
        }
    }

    #[tokio::test]
    async fn live_spine_router_keeps_cognition_out_of_tool_dispatch() {
        let router = SpineActionRouter::new(
            Arc::new(PlatformMarker),
            Arc::new(CerebellumActionHandler::new_minimal()),
        );

        let classification = router
            .call("detect_intent", &json!({"text": "please inspect this"}))
            .await
            .expect("cognition action should be registered directly");
        assert!(classification.get("platform_action").is_none());

        let durable = router
            .call("write_state", &json!({"key": "x", "value": 1}))
            .await
            .expect("platform action should retain durable owner");
        assert_eq!(durable["platform_action"], "write_state");

        let tier = router
            .call("determine_tier", &json!({"complexity": 6, "has_tools": false}))
            .await
            .expect("tier action should be registered directly");
        assert_eq!(tier, "premium");

        let context = router
            .call("recall_context", &json!({"entities": {"entities": []}}))
            .await
            .expect("context recall action should be registered directly");
        assert!(context.is_array());

        let guidance = router
            .call(
                "format_guidance_block",
                &json!({"session_type": "chat", "constraints": [{"message": "Preserve durable state"}]}),
            )
            .await
            .expect("guidance formatter should be registered directly");
        assert!(guidance.as_str().unwrap().contains("Preserve durable state"));

        let sent = router
            .call("channel_send", &json!({"chat_id": "chat", "content": "hello"}))
            .await
            .expect("channel output action should be registered directly");
        assert_eq!(sent["sent"], true);

        let prefix = router
            .call("db_get_prefix", &json!({"prefix": "constraint:"}))
            .await
            .expect("prefix read should retain durable platform ownership");
        assert_eq!(prefix["platform_action"], "read_state_prefix");
    }

    #[test]
    fn live_spine_router_utility_actions_preserve_px_values() {
        assert_eq!(
            SpineActionRouter::get_field(&json!({
                "object": {"name": "spine"},
                "field": "name",
            }))
            .unwrap(),
            json!("spine")
        );
        assert_eq!(
            SpineActionRouter::append_to_list(&json!({
                "list": ["parent"],
                "items": ["child"],
            }))
            .unwrap(),
            json!(["parent", "child"])
        );
        assert_eq!(
            SpineActionRouter::compute_context_budget(&json!({
                "window": 32_000,
                "reserve_for_output": 4_096,
                "reserve_for_system": 2_048,
            }))
            .unwrap(),
            json!(25_856)
        );
    }

    // ── cosine_similarity tests ──────────────────────────────────────────────

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!(
            (sim - 1.0).abs() < 1e-6,
            "identical vectors should have similarity 1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            sim.abs() < 1e-6,
            "orthogonal vectors should have similarity 0.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_opposite_vectors() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let sim = cosine_similarity(&a, &b);
        assert!(
            (sim + 1.0).abs() < 1e-6,
            "opposite vectors should have similarity -1.0, got {sim}"
        );
    }

    #[test]
    fn cosine_similarity_known_value() {
        // a = [3, 4], b = [4, 3]
        // dot = 12+12 = 24, |a| = 5, |b| = 5
        // cos = 24/25 = 0.96
        let a = vec![3.0, 4.0];
        let b = vec![4.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.96).abs() < 1e-6, "expected 0.96, got {sim}");
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similarity_mismatched_dimensions() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert_eq!(sim, 0.0);
    }

    // ── action dispatch tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_unknown_action_returns_error() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler.call("nonexistent_action", &json!({})).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ExecutionError::UnknownAction(name) => assert_eq!(name, "nonexistent_action"),
            other => panic!("expected UnknownAction, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cosine_similarity_action_dispatch() {
        let handler = CerebellumActionHandler::for_testing();
        let params = json!({
            "a": [1.0, 0.0, 0.0],
            "b": [0.0, 1.0, 0.0]
        });
        let result = handler.call("cosine_similarity", &params).await.unwrap();
        let sim = result["similarity"].as_f64().unwrap();
        assert!(sim.abs() < 1e-6, "orthogonal vectors via action, got {sim}");
    }

    /// Regression test for the amnesia bug: `append_history` was aliased to
    /// raw `write_state`, which requires a `key`/`value` pair. Every real
    /// call site passes `{chat_id, role, content}` instead, so every append
    /// silently failed and `chat_history:{chat_id}` was never populated,
    /// meaning `assemble_context`/`dispatch_steered_task` always read back
    /// an empty history on the next turn.
    #[tokio::test]
    async fn append_history_persists_into_chat_history_key() {
        let handler = CerebellumActionHandler::for_testing();

        let result = handler
            .call(
                "append_history",
                &json!({"chat_id": "123", "role": "user", "content": "remember this task"}),
            )
            .await
            .expect("append_history must succeed with chat_id/role/content params");
        assert_eq!(result["written"], json!(true));

        let read_back = handler
            .call("read_state", &json!({"key": "chat_history:123"}))
            .await
            .unwrap();
        let history = read_back["value"].as_array().expect("chat_history must be an array");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0]["role"], json!("user"));
        assert_eq!(history[0]["content"], json!("remember this task"));

        // A second turn appends rather than overwrites.
        handler
            .call(
                "append_history",
                &json!({"chat_id": "123", "role": "assistant", "content": "got it"}),
            )
            .await
            .unwrap();
        let read_back2 = handler
            .call("read_state", &json!({"key": "chat_history:123"}))
            .await
            .unwrap();
        let history2 = read_back2["value"].as_array().unwrap();
        assert_eq!(history2.len(), 2);
        assert_eq!(history2[1]["role"], json!("assistant"));
    }

    #[tokio::test]
    async fn append_history_missing_chat_id_is_a_real_error() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler
            .call("append_history", &json!({"role": "user", "content": "x"}))
            .await;
        assert!(result.is_err(), "append_history without chat_id must fail loudly, not silently no-op");
    }

    #[tokio::test]
    async fn cosine_similarity_action_dimension_mismatch() {
        let handler = CerebellumActionHandler::for_testing();
        let params = json!({
            "a": [1.0, 2.0],
            "b": [1.0, 2.0, 3.0]
        });
        let result = handler.call("cosine_similarity", &params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_state_returns_null_for_missing_key() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler
            .call("read_state", &json!({"key": "missing"}))
            .await
            .unwrap();
        assert_eq!(result["value"], Value::Null);
    }

    #[tokio::test]
    async fn write_then_read_state() {
        let handler = CerebellumActionHandler::for_testing();

        // Write
        let write_result = handler
            .call("write_state", &json!({"key": "greeting", "value": "hello"}))
            .await
            .unwrap();
        assert_eq!(write_result["written"], true);

        // Read back
        let read_result = handler
            .call("read_state", &json!({"key": "greeting"}))
            .await
            .unwrap();
        assert_eq!(read_result["value"], "hello");
    }

    #[tokio::test]
    async fn write_state_complex_value() {
        let handler = CerebellumActionHandler::for_testing();
        let complex = json!({"nested": {"array": [1, 2, 3]}, "flag": true});

        handler
            .call(
                "write_state",
                &json!({"key": "config", "value": complex.clone()}),
            )
            .await
            .unwrap();

        let result = handler
            .call("read_state", &json!({"key": "config"}))
            .await
            .unwrap();
        assert_eq!(result["value"], complex);
    }

    #[tokio::test]
    async fn get_current_time_returns_reasonable_timestamp() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler.call("get_current_time", &json!({})).await.unwrap();
        let ts = result["timestamp_ms"].as_i64().unwrap();
        // Should be after 2024-01-01 (1704067200000 ms)
        assert!(
            ts > 1_704_067_200_000,
            "timestamp should be recent, got {ts}"
        );
        // Should be before 2030-01-01 (1893456000000 ms)
        assert!(
            ts < 1_893_456_000_000,
            "timestamp should not be in the far future, got {ts}"
        );
    }


    #[tokio::test]
    async fn live_context_pause_suppresses_events_and_resume_reenables_them() {
        use std::time::Duration;

        let handler = CerebellumActionHandler::for_testing();
        let mut subscriber = handler.subscribe_live_context();
        let session_id = "chronos-debug-session";

        assert_eq!(handler.call("publish_live_context_event", &json!({"session_id": session_id, "event": {"sequence": 1}})).await.unwrap()["delivered"], true);
        assert_eq!(subscriber.recv().await.unwrap()["event"]["sequence"], 1);

        assert_eq!(handler.call("pause_live_context_subscription", &json!({"session_id": session_id})).await.unwrap()["paused"], true);
        assert_eq!(handler.call("publish_live_context_event", &json!({"session_id": session_id, "event": {"sequence": 2}})).await.unwrap()["delivered"], false);
        assert!(tokio::time::timeout(Duration::from_millis(200), subscriber.recv()).await.is_err());

        assert_eq!(handler.call("resume_live_context_subscription", &json!({"session_id": session_id})).await.unwrap()["paused"], false);
        assert_eq!(handler.call("publish_live_context_event", &json!({"session_id": session_id, "event": {"sequence": 3}})).await.unwrap()["delivered"], true);
        assert_eq!(subscriber.recv().await.unwrap()["event"]["sequence"], 3);
    }

    #[tokio::test]
    async fn emit_event_without_channel_returns_error() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler
            .call(
                "emit_event",
                &json!({"type": "model_request", "payload": {}}),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn emit_event_sends_to_channel() {
        let (tx, mut rx) = mpsc::channel(16);
        let handler = CerebellumActionHandler::new(None, Some(tx));

        let result = handler
            .call(
                "emit_event",
                &json!({
                    "type": "model_request",
                    "payload": {"chat_id": "test-chat", "content": "hello"}
                }),
            )
            .await
            .unwrap();

        assert_eq!(result["emitted"], true);

        // Verify the event was received
        let event = rx.try_recv().unwrap();
        match event {
            SpineEvent::ModelRequest {
                source,
                chat_id,
                content,
                ..
            } => {
                assert_eq!(source, "orchestrator");
                assert_eq!(chat_id, "test-chat");
                assert_eq!(content, "hello");
            }
            other => panic!("expected ModelRequest, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn compute_embedding_without_provider_returns_error() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler
            .call("compute_embedding", &json!({"text": "hello world"}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compute_embedding_missing_text_param() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler.call("compute_embedding", &json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn compute_embedding_with_mock_provider() {
        use crate::memory::embed::MockEmbedder;

        let embedder: Arc<dyn EmbeddingProvider> = Arc::new(MockEmbedder);
        let handler = CerebellumActionHandler::new(Some(embedder), None);

        let result = handler
            .call("compute_embedding", &json!({"text": "hello world"}))
            .await
            .unwrap();

        let embedding = result["embedding"].as_array().unwrap();
        assert_eq!(embedding.len(), 384); // MockEmbedder uses EMBEDDING_DIM = 384
    }

    #[tokio::test]
    async fn read_state_missing_key_param() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler.call("read_state", &json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_state_missing_key_param() {
        let handler = CerebellumActionHandler::for_testing();
        let result = handler.call("write_state", &json!({"value": 42})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn emit_event_missing_type_param() {
        let (tx, _rx) = mpsc::channel(16);
        let handler = CerebellumActionHandler::new(None, Some(tx));
        let result = handler.call("emit_event", &json!({"payload": {}})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn emit_event_generic_type_creates_inbound() {
        let (tx, mut rx) = mpsc::channel(16);
        let handler = CerebellumActionHandler::new(None, Some(tx));

        let result = handler
            .call(
                "emit_event",
                &json!({
                    "type": "custom_event",
                    "payload": {"data": "test"}
                }),
            )
            .await
            .unwrap();

        assert_eq!(result["emitted"], true);

        let event = rx.try_recv().unwrap();
        matches!(event, SpineEvent::Inbound { .. });
    }
}
