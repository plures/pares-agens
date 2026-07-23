//! Dataflow bridge: the COMPLETE message pipeline as a PluresDB dataflow graph.
//!
//! This replaces the imperative cerebellum orchestration. The flow is:
//!   1. Channel adapter writes InboundMessage to "inbound" queue
//!   2. Graph runs to quiescence (all procedures fire via data availability)
//!   3. "delivery" queue contains the response to send
//!   4. Channel adapter reads from "delivery" and sends
//!
//! The graph IS the router, the context assembler, the model invoker, and
//! the tool loop. Rust only exists at IO boundaries (model API, tool exec,
//! channel send) — implemented as actions in CerebellumActionHandler.
//!
//! See: praxis/procedures/unified-router.px for the complete topology.
//! See: praxis/spine/spine.px for the queue registry and architecture.

use pares_radix_praxis::dataflow::{
    AsyncActionHandler, AsyncDataflowGraph, DataflowConfig, Datum, ProcedureNode,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, info};

/// The dataflow-driven message pipeline.
///
/// Replaces: cerebellum/mod.rs orchestration, router.rs, classifier.rs,
/// context_manager.rs, invoke.rs, pipeline.rs.
///
/// Keeps: CerebellumActionHandler (IO boundary dispatch).
pub struct DataflowBridge {
    graph: AsyncDataflowGraph,
    /// Whether the graph has any procedures loaded.
    active: bool,
    /// Action handler for side-effect dispatch (model calls, tool exec, channel send).
    handler: Arc<dyn AsyncActionHandler>,
}

impl DataflowBridge {
    /// Create a new bridge with default config and an action handler.
    pub fn new(handler: Arc<dyn AsyncActionHandler>) -> Self {
        Self {
            graph: AsyncDataflowGraph::new(),
            active: false,
            handler,
        }
    }

    /// Create with custom depth/queue limits.
    pub fn with_config(config: DataflowConfig, handler: Arc<dyn AsyncActionHandler>) -> Self {
        Self {
            graph: AsyncDataflowGraph::with_config(config),
            active: false,
            handler,
        }
    }

    /// Load a procedure node into the graph.
    pub async fn register(&mut self, node: ProcedureNode) -> Result<(), String> {
        self.graph
            .register(node)
            .await
            .map_err(|e| format!("{e}"))?;
        self.active = true;
        Ok(())
    }

    /// Whether any procedures are loaded.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Run the FULL pipeline for an inbound message.
    ///
    /// This is the top-level entry point. Channel adapters call this.
    /// The graph handles everything: routing, context, model invocation,
    /// tool loops, commitment detection, and delivery.
    ///
    /// Returns the delivery payload (content to send to user), or None
    /// if the graph produced no deliverable output (e.g., message was dropped).
    pub async fn process_message(
        &self,
        chat_id: i64,
        sender: &str,
        content: &str,
        message_id: Option<&str>,
        history: Option<&[pares_radix_core::model::ChatMessage]>,
    ) -> Result<Option<DeliveryResult>, String> {
        if !self.active {
            return Ok(None);
        }

        // Format conversation history for context injection
        let history_text = if let Some(messages) = history {
            if messages.is_empty() {
                String::new()
            } else {
                // Take last N messages to fit reasonable context
                let recent: Vec<_> = messages.iter().rev().take(20).collect::<Vec<_>>().into_iter().rev().collect();
                recent.iter().map(|m| {
                    format!("{}: {}", m.role, m.content)
                }).collect::<Vec<_>>().join("\n")
            }
        } else {
            String::new()
        };

        let event_datum = Datum::root(json!({
            "content": content,
            "chat_id": chat_id.to_string(),
            "sender": sender,
            "timestamp": chrono::Utc::now().timestamp_millis(),
            "message_id": message_id,
            "conversation_history": history_text,
        }));

        // Push to inbound — graph fires from here
        self.graph
            .push("inbound", event_datum)
            .await
            .map_err(|e| format!("push to inbound failed: {e}"))?;

        // Run to quiescence — everything happens here
        let fired = self
            .graph
            .run_to_completion(self.handler.clone())
            .await
            .map_err(|e| format!("graph execution failed: {e}"))?;

        info!(procedures_fired = fired, chat_id, "dataflow pipeline quiescent");

        if fired == 0 {
            debug!("no procedures fired — graph may not have matching consumers");
            return Ok(None);
        }

        // Read delivery output
        if let Some(datum) = self.graph.pop("delivery").await {
            let content = datum.value.get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let streaming = datum.value.get("streaming")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);

            if content.is_empty() {
                return Ok(None);
            }

            return Ok(Some(DeliveryResult {
                content,
                chat_id: chat_id.to_string(),
                streaming,
                reply_to: datum.value.get("reply_to")
                    .and_then(|v| v.as_str())
                    .map(String::from),
            }));
        }

        // Check if delivery happened inline (side-effect in deliver_response procedure)
        if let Some(datum) = self.graph.pop("delivered").await {
            debug!(delivered = ?datum.value, "delivery completed inline by procedure");
            return Ok(None); // Already sent — nothing for caller to do
        }

        // Procedures fired but no delivery output — might be a dropped message
        // or a procedural response that was handled internally
        debug!(fired, "procedures fired but no delivery output");
        Ok(None)
    }

    /// Legacy compatibility: process_event for the cerebellum's current routing path.
    /// TODO: Remove once cerebellum is fully replaced by process_message.
    pub async fn process_event(
        &self,
        event_type: &str,
        content: &str,
        context: &str,
    ) -> Result<Option<Value>, String> {
        let event_datum = Datum::root(json!({
            "type": event_type,
            "content": content,
            "context": context,
        }));

        self.graph
            .push("inbound", event_datum)
            .await
            .map_err(|e| format!("push failed: {e}"))?;

        let fired = self
            .graph
            .run_to_completion(self.handler.clone())
            .await
            .map_err(|e| format!("execution failed: {e}"))?;

        info!(procedures_fired = fired, "dataflow graph quiescent (legacy path)");

        if fired == 0 {
            return Ok(None);
        }

        // Read from output queues in priority order (legacy compatibility)
        for queue_name in &["route", "route_decision", "classification", "model_response", "delivery"] {
            if let Some(datum) = self.graph.pop(queue_name).await {
                return Ok(Some(datum.value));
            }
        }

        Ok(None)
    }
}

/// Result of a successful pipeline execution — ready for channel delivery.
#[derive(Debug, Clone)]
pub struct DeliveryResult {
    /// The response content to send to the user.
    pub content: String,
    /// Target chat ID.
    pub chat_id: String,
    /// Whether to stream the response (progressive editing).
    pub streaming: bool,
    /// Message to reply to (quote), if any.
    pub reply_to: Option<String>,
}

/// Re-exports the existing CerebellumActionHandler so the dataflow bridge
/// can use the same IO boundary as the trigger-based px_bridge.
pub use super::actions::CerebellumActionHandler;

/// Adapter that wraps our local AsyncActionHandler to satisfy the pluresdb-px trait.
/// Both traits have `async fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError>`
/// but they're from different crates, so we need a thin wrapper.
pub struct DataflowActionAdapter {
    inner: Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>,
}

impl DataflowActionAdapter {
    pub fn new(handler: Arc<dyn pares_radix_core::px_adapter::AsyncActionHandler>) -> Self {
        Self { inner: handler }
    }
}

#[async_trait::async_trait]
impl AsyncActionHandler for DataflowActionAdapter {
    async fn call(
        &self,
        name: &str,
        params: &Value,
    ) -> Result<Value, pares_radix_praxis::dataflow::ExecutionError> {
        self.inner.call(name, params).await
    }
}
