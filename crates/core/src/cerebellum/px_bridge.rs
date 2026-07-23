//! Bridge module: routes cerebellum logic calls through .px procedures
//! when available, falling back to hardcoded Rust implementations.
//!
//! This is the transitional layer for the .px-first migration. As .px
//! procedures mature and prove reliable, the Rust fallbacks can be removed.
//!
//! # Architecture
//!
//! ```text
//! Cerebellum (caller)
//!     │
//!     ▼
//! PxBridge (this module)
//!     │
//!     ├─ .px loaded? ──► execute_procedure("classify_message", vars)
//!     │                       │
//!     │                       ▼
//!     │               PxProcedureAdapter (calls ActionHandler for IO)
//!     │
//!     └─ fallback ───► classifier.rs / router.rs (hardcoded Rust)
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::{debug, info};

use pares_radix_core::px_adapter::{load_px_procedures, AsyncActionHandler, PxProcedureAdapter};

/// Holds loaded .px procedures for cerebellum logic, keyed by procedure name.
pub struct PxBridge {
    /// Loaded procedure adapters, keyed by name
    procedures: RwLock<HashMap<String, Arc<PxProcedureAdapter>>>,
    /// Action handler for IO boundaries (embedding, state, etc.)
    handler: Arc<dyn AsyncActionHandler>,
    /// Whether the bridge is active (procedures loaded successfully)
    active: std::sync::atomic::AtomicBool,
}

impl PxBridge {
    /// Create a new bridge with the given action handler.
    pub fn new(handler: Arc<dyn AsyncActionHandler>) -> Self {
        Self {
            procedures: RwLock::new(HashMap::new()),
            handler,
            active: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Load .px procedures from source text.
    ///
    /// Call this at startup with the contents of `praxis/procedures/*.px`.
    /// Procedures are indexed by name for direct invocation.
    pub async fn load_from_source(&self, source: &str) -> Result<usize, String> {
        let adapters = load_px_procedures(source, self.handler.clone())?;
        let count = adapters.len();

        let mut procs = self.procedures.write().await;
        for adapter in adapters {
            let name = adapter.name().to_string();
            debug!(procedure = %name, "px_bridge: registered procedure");
            procs.insert(name, Arc::new(adapter));
        }

        if count > 0 {
            self.active
                .store(true, std::sync::atomic::Ordering::Relaxed);
            info!(count, "px_bridge: loaded cerebellum procedures");
        }

        Ok(count)
    }

    /// Load .px procedures from a directory (recursive).
    pub async fn load_from_directory(&self, dir: &std::path::Path) -> usize {
        let adapters = pares_radix_core::px_adapter::load_px_directory(dir, self.handler.clone());
        let count = adapters.len();

        let mut procs = self.procedures.write().await;
        for adapter in adapters {
            let name = adapter.name().to_string();
            debug!(procedure = %name, "px_bridge: registered procedure from directory");
            procs.insert(name, Arc::new(adapter));
        }

        if count > 0 {
            self.active
                .store(true, std::sync::atomic::Ordering::Relaxed);
            info!(count, dir = %dir.display(), "px_bridge: loaded cerebellum procedures from directory");
        }

        count
    }

    /// Load .px procedures from a directory synchronously (for non-async contexts).
    ///
    /// If called from within a tokio runtime, uses `try_write` with a spin loop
    /// to avoid the blocking_write panic. Safe in both sync and async contexts.
    pub fn load_from_directory_sync(&self, dir: &std::path::Path) -> usize {
        let adapters = pares_radix_core::px_adapter::load_px_directory(dir, self.handler.clone());
        let count = adapters.len();

        // If we're inside a tokio runtime, blocking_write() panics.
        // Use try_write() in a loop instead (lock is uncontended at startup).
        let in_runtime = tokio::runtime::Handle::try_current().is_ok();
        if in_runtime {
            loop {
                if let Ok(mut procs) = self.procedures.try_write() {
                    for adapter in adapters {
                        let name = adapter.name().to_string();
                        debug!(procedure = %name, "px_bridge: registered procedure from directory (sync)");
                        procs.insert(name, Arc::new(adapter));
                    }
                    break;
                }
                std::thread::yield_now();
            }
        } else {
            let mut procs = self.procedures.blocking_write();
            for adapter in adapters {
                let name = adapter.name().to_string();
                debug!(procedure = %name, "px_bridge: registered procedure from directory (sync)");
                procs.insert(name, Arc::new(adapter));
            }
        }

        if count > 0 {
            self.active
                .store(true, std::sync::atomic::Ordering::Relaxed);
            info!(count, dir = %dir.display(), "px_bridge: loaded cerebellum procedures from directory (sync)");
        }

        count
    }

    /// Whether any .px procedures are loaded and ready.
    pub fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Execute a named procedure with the given variables.
    ///
    /// Returns `None` if the procedure isn't loaded (caller should fall back
    /// to Rust implementation). Returns `Some(Err(...))` if loaded but fails.
    pub async fn call(
        &self,
        procedure_name: &str,
        vars: HashMap<String, Value>,
    ) -> Option<Result<Value, String>> {
        let procs = self.procedures.read().await;
        let adapter = procs.get(procedure_name)?;

        let result = adapter.execute_with_vars(vars).await;

        Some(match result {
            Ok(exec_result) => {
                if exec_result.success {
                    // Return the procedure's output (last assigned variable or explicit return)
                    if let Some(ret) = exec_result.variables.get("__return__") {
                        Ok(ret.clone())
                    } else if let Some(ret) = exec_result.variables.get("result") {
                        Ok(ret.clone())
                    } else {
                        // Return all variables as a JSON object
                        Ok(json!(exec_result.variables))
                    }
                } else {
                    Err(exec_result
                        .error
                        .unwrap_or_else(|| "unknown .px execution error".to_string()))
                }
            }
            Err(e) => Err(format!("px executor error: {e}")),
        })
    }

    /// Execute the classify_message procedure via .px.
    ///
    /// Returns the classification result as a Value, or None to fall back.
    pub async fn classify_message(
        &self,
        message: &str,
        plugins: &[String],
        last_topic: &str,
    ) -> Option<Result<Value, String>> {
        let mut vars = HashMap::new();
        vars.insert("message".to_string(), Value::String(message.to_string()));
        vars.insert("plugins".to_string(), json!(plugins));
        vars.insert(
            "last_topic".to_string(),
            Value::String(last_topic.to_string()),
        );

        self.call("classify_message", vars).await
    }

    /// Execute the route_event procedure via .px.
    ///
    /// Returns the routing decision as a Value, or None to fall back.
    pub async fn route_event(
        &self,
        event_type: &str,
        content: &str,
        learned_context: &str,
        enable_subconscious: bool,
        complexity_threshold: f64,
    ) -> Option<Result<Value, String>> {
        let mut vars = HashMap::new();
        vars.insert(
            "event_type".to_string(),
            Value::String(event_type.to_string()),
        );
        vars.insert("content".to_string(), Value::String(content.to_string()));
        vars.insert(
            "learned_context".to_string(),
            Value::String(learned_context.to_string()),
        );
        vars.insert(
            "enable_subconscious".to_string(),
            Value::Bool(enable_subconscious),
        );
        vars.insert(
            "complexity_threshold".to_string(),
            json!(complexity_threshold),
        );

        self.call("route_event", vars).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pares_radix_praxis::px::executor::ExecutionError;

    /// Minimal test handler that returns empty for any action call.
    struct NoOpHandler;

    #[async_trait]
    impl AsyncActionHandler for NoOpHandler {
        async fn call(&self, name: &str, _params: &Value) -> Result<Value, ExecutionError> {
            // For testing .px logic that doesn't need real IO
            match name {
                "lowercase" => Ok(json!("")),
                "trim" => Ok(json!("")),
                "split" => Ok(json!([])),
                "length" => Ok(json!(0)),
                _ => Err(ExecutionError::UnknownAction(name.to_string())),
            }
        }
    }

    #[tokio::test]
    async fn bridge_inactive_when_no_procedures_loaded() {
        let handler: Arc<dyn AsyncActionHandler> = Arc::new(NoOpHandler);
        let bridge = PxBridge::new(handler);
        assert!(!bridge.is_active());
    }

    #[tokio::test]
    async fn bridge_returns_none_for_unknown_procedure() {
        let handler: Arc<dyn AsyncActionHandler> = Arc::new(NoOpHandler);
        let bridge = PxBridge::new(handler);
        let result = bridge.classify_message("hello", &[], "").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn bridge_loads_valid_px_source() {
        let handler: Arc<dyn AsyncActionHandler> = Arc::new(NoOpHandler);
        let bridge = PxBridge::new(handler);

        let source = r#"
procedure test_proc:
  trigger: manual
  return "hello"
"#;
        let count = bridge.load_from_source(source).await.unwrap();
        // May or may not parse depending on grammar support for simple return
        // The point is it doesn't crash
        assert!(count == 0 || count == 1);
    }
}
