//! Event spine — bridges pares-agens to PluresDB's `AgensRuntime`.
//!
//! The spine initialises `AgensRuntime` from the existing `CrdtStore` that
//! backs PluresDB memory and registers core lifecycle procedures for the
//! message-processing pipeline:
//!
//! ```text
//! InboundMessage → PluresDB → procedures fire → ModelRequest
//!   → model call → ModelResponse → PluresDB
//!   → DeliveryRequest → channel renderer → DeliverySuccess / DeliveryFailure
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use pares_agens_core::event_spine::EventSpine;
//!
//! let spine = EventSpine::new(store.crdt_store(), "pares-agens");
//! spine.seed_contracts();            // write channel contracts to PluresDB
//! spine.register_core_procedures();  // register lifecycle handlers
//!
//! // From a channel adapter:
//! spine.emit_inbound_message(chat_id, user, text);
//! ```

use std::sync::Arc;

use pluresdb::CrdtStore;
use pluresdb_procedures::agens::{AgensEvent, AgensRuntime, ProcedureHandler};
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::channel_contract::ChannelContract;

/// The event spine wraps `AgensRuntime` and provides the pares-agens
/// message lifecycle on top of PluresDB's reactive procedure system.
pub struct EventSpine<'a> {
    runtime: AgensRuntime<'a>,
    store: &'a CrdtStore,
}

impl<'a> EventSpine<'a> {
    /// Create a new event spine bound to the given CRDT store.
    pub fn new(store: &'a CrdtStore, actor: impl Into<String>) -> Self {
        let runtime = AgensRuntime::new(store, actor);
        Self { runtime, store }
    }

    /// Seed channel contracts into PluresDB.
    ///
    /// Currently seeds the Telegram contract.  Future channels (Discord, etc.)
    /// will be added here.
    pub fn seed_contracts(&self) {
        let telegram = ChannelContract::telegram();
        self.store.put(
            "contract:telegram",
            "event-spine",
            telegram.to_json(),
        );
        info!("seeded Telegram channel contract into PluresDB");
    }

    /// Look up a channel contract by name.
    pub fn get_contract(&self, channel: &str) -> Option<ChannelContract> {
        let node = self.store.get(format!("contract:{}", channel))?;
        ChannelContract::from_json(&node.data)
    }

    /// Register the core message-lifecycle procedures.
    ///
    /// Currently registers handlers for:
    /// - `"message"` — logs inbound messages (placeholder for routing to model)
    /// - `"model_response"` — converts model output into delivery requests
    pub fn register_core_procedures(&self) {
        // Inbound message handler — in a full implementation this would invoke
        // the model client.  For now it logs and emits a placeholder.
        self.runtime.register_procedure(
            "message",
            Arc::new(|event: &AgensEvent| {
                if let AgensEvent::Message { id, payload } = event {
                    debug!(event_id = %id, "event_spine: inbound message received");
                    // In the full pipeline, this handler would:
                    // 1. Run cerebellum preprocessing
                    // 2. Build the model request
                    // 3. Emit a ModelRequest event
                    // For now we just log.
                    let _ = payload;
                }
                Ok(())
            }),
        );

        // Model response handler — transforms model output into a delivery request.
        self.runtime.register_procedure(
            "model_response",
            Arc::new(|event: &AgensEvent| {
                if let AgensEvent::ModelResponse { id, payload } = event {
                    debug!(event_id = %id, "event_spine: model response received");
                    // In the full pipeline, this handler would:
                    // 1. Read the target channel from the payload
                    // 2. Format according to the channel contract
                    // 3. Emit a DeliveryRequest (stored as a Message event
                    //    with a delivery payload)
                    let _ = payload;
                }
                Ok(())
            }),
        );

        info!("registered core event-spine procedures (message, model_response)");
    }

    /// Register an additional procedure handler for a custom event type.
    pub fn register_procedure(
        &self,
        event_type: impl Into<String>,
        handler: Arc<dyn ProcedureHandler>,
    ) {
        self.runtime.register_procedure(event_type, handler);
    }

    // -----------------------------------------------------------------------
    // Convenience emitters
    // -----------------------------------------------------------------------

    /// Emit an inbound message event from a channel adapter.
    ///
    /// Returns the CRDT node ID of the stored event.
    pub fn emit_inbound_message(
        &self,
        chat_id: i64,
        user: &str,
        text: &str,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let event = AgensEvent::Message {
            id,
            payload: json!({
                "source": "telegram",
                "chat_id": chat_id,
                "user": user,
                "text": text,
            }),
        };
        let node_id = self.runtime.emit_event(&event);
        if let Err(e) = self.runtime.execute_procedure(&event) {
            warn!(error = %e, "event_spine: failed to execute message procedure");
        }
        node_id
    }

    /// Emit a model response event (called after the LLM responds).
    ///
    /// Returns the CRDT node ID of the stored event.
    pub fn emit_model_response(
        &self,
        chat_id: i64,
        channel: &str,
        content: &str,
    ) -> String {
        let id = Uuid::new_v4().to_string();
        let event = AgensEvent::ModelResponse {
            id,
            payload: json!({
                "channel": channel,
                "chat_id": chat_id,
                "content": content,
            }),
        };
        let node_id = self.runtime.emit_event(&event);
        if let Err(e) = self.runtime.execute_procedure(&event) {
            warn!(error = %e, "event_spine: failed to execute model_response procedure");
        }
        node_id
    }

    /// Emit a delivery success event.
    pub fn emit_delivery_success(&self, chat_id: i64, channel: &str, message_id: i64) {
        let id = Uuid::new_v4().to_string();
        let event = AgensEvent::Message {
            id,
            payload: json!({
                "_delivery": "success",
                "channel": channel,
                "chat_id": chat_id,
                "message_id": message_id,
            }),
        };
        self.runtime.emit_event(&event);
    }

    /// Emit a delivery failure event.
    pub fn emit_delivery_failure(&self, chat_id: i64, channel: &str, error: &str) {
        let id = Uuid::new_v4().to_string();
        let event = AgensEvent::Message {
            id,
            payload: json!({
                "_delivery": "failure",
                "channel": channel,
                "chat_id": chat_id,
                "error": error,
            }),
        };
        self.runtime.emit_event(&event);
    }

    /// Access the underlying `AgensRuntime` for advanced use.
    pub fn runtime(&self) -> &AgensRuntime<'a> {
        &self.runtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spine_emits_and_polls_message() {
        let store = CrdtStore::default();
        let spine = EventSpine::new(&store, "test-actor");
        spine.register_core_procedures();

        let before = chrono::Utc::now();
        spine.emit_inbound_message(12345, "alice", "hello");

        let events = spine.runtime().poll_events(before);
        assert!(!events.is_empty());
    }

    #[test]
    fn spine_seeds_telegram_contract() {
        let store = CrdtStore::default();
        let spine = EventSpine::new(&store, "test-actor");
        spine.seed_contracts();

        let contract = spine.get_contract("telegram").expect("contract should exist");
        assert_eq!(contract.max_message_len, 4096);
        assert_eq!(contract.preferred_format, "HTML");
    }
}
