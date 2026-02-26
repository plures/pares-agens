//! `pares-agens-core` — reactive event loop, procedure executor, and praxis decision ledger.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use pares_agens_core::{
//!     executor::Executor,
//!     handlers::{OnMessage, OnTimer},
//!     procedure::ProcedureRegistry,
//! };
//!
//! # #[tokio::main]
//! # async fn main() {
//! let mut registry = ProcedureRegistry::new();
//! registry.register(Box::new(OnMessage));
//! registry.register(Box::new(OnTimer));
//!
//! let executor = Executor::new(registry);
//! // executor.run(&source, 0).await;  // pass a real EventSource
//! # }
//! ```

pub mod event;
pub mod executor;
pub mod handlers;
pub mod praxis;
pub mod procedure;
pub mod source;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum Event {
    Message { id: Uuid, content: String, from: String },
    ModelResponse { id: Uuid, content: String },
    TimerFired { name: String },
    StateChanged { key: String, value: String },
    ToolResult { call_id: Uuid, content: String },
}

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("Store error: {0}")]
    Store(String),
    #[error("Recall error: {0}")]
    Recall(String),
}

#[async_trait]
pub trait Memory {
    async fn capture(&self, event: &Event) -> Result<(), MemoryError>;
    async fn recall(&self, query: &str) -> Result<Vec<String>, MemoryError>;
}

#[derive(Default)]
pub struct InMemory {
    store: Arc<Mutex<Vec<String>>>,
}

impl InMemory {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Memory for InMemory {
    async fn capture(&self, event: &Event) -> Result<(), MemoryError> {
        let content = match event {
            Event::Message { content, .. } => content.clone(),
            Event::ModelResponse { content, .. } => content.clone(),
            Event::TimerFired { name } => name.clone(),
            Event::StateChanged { key, value } => format!("{key}={value}"),
            Event::ToolResult { content, .. } => content.clone(),
        };
        self.store
            .lock()
            .map_err(|e| MemoryError::Store(e.to_string()))?
            .push(content);
        Ok(())
    }

    async fn recall(&self, query: &str) -> Result<Vec<String>, MemoryError> {
        let store = self
            .store
            .lock()
            .map_err(|e| MemoryError::Recall(e.to_string()))?;
        let results = store
            .iter()
            .filter(|s| s.contains(query))
            .cloned()
            .collect();
        Ok(results)
    }
}

pub mod agent {
    use super::{Event, Memory};
    use std::sync::Arc;

    pub struct Agent {
        pub memory: Arc<dyn Memory + Send + Sync>,
    }

    impl Agent {
        pub fn new(memory: Arc<dyn Memory + Send + Sync>) -> Self {
            Self { memory }
        }

        pub async fn handle_event(&self, event: Event) -> Option<Event> {
            self.memory.capture(&event).await.ok();
            match event {
                Event::Message { id, content, .. } => Some(Event::ModelResponse {
                    id,
                    content: format!("Echo: {}", content),
                }),
                _ => None,
            }
        }
    }
}

pub use agent::Agent;
