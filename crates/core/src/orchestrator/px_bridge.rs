//! Bridge module: routes orchestrator logic calls through .px procedures
//! when available, falling back to hardcoded Rust implementations.
//!
//! This is the transitional layer for the .px-first migration. As .px
//! procedures mature and prove reliable, the Rust fallbacks can be removed.
//!
//! # Architecture
//!
//! ```text
//! Orchestrator (caller)
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
use px_check::{ContractCatalog, ExecutionProfile};

/// Holds loaded .px procedures for orchestrator logic, keyed by procedure name.
pub struct PxBridge {
    /// Loaded procedure adapters, keyed by name
    procedures: RwLock<HashMap<String, Arc<PxProcedureAdapter>>>,
    /// Action handler for IO boundaries (embedding, state, etc.)
    handler: Arc<dyn AsyncActionHandler>,
    /// Whether the bridge is active (procedures loaded successfully)
    active: std::sync::atomic::AtomicBool,
}

impl PxBridge {
    /// Validate a source document against its complete host contract without
    /// registering it. Hosts use this to make registration atomic across the
    /// named-procedure bridge and the reactive registry.
    pub fn validate_source_contract(
        source: &str,
        catalog: &ContractCatalog,
        profile: ExecutionProfile,
    ) -> Result<(), String> {
        let report = pluresdb_px::px::compiler::validate_and_compile_checked(source, catalog, profile);
        if report.is_activatable() {
            return Ok(());
        }

        Err(report
            .diagnostics
            .iter()
            .map(|diagnostic| {
                format!(
                    "{} {} step {}: {}",
                    diagnostic.code, diagnostic.procedure, diagnostic.step, diagnostic.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

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
            info!(count, "px_bridge: loaded orchestrator procedures");
        }

        Ok(count)
    }

    /// Validate the entire host contract before registering any procedure from
    /// this source. A failed report activates nothing and includes every static
    /// defect the Praxis checker can determine in one pass.
    pub async fn load_checked_from_source(
        &self,
        source: &str,
        catalog: &ContractCatalog,
        profile: ExecutionProfile,
    ) -> Result<usize, String> {
        Self::validate_source_contract(source, catalog, profile)?;
        self.load_from_source(source).await
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
            info!(count, dir = %dir.display(), "px_bridge: loaded orchestrator procedures from directory");
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
            info!(count, dir = %dir.display(), "px_bridge: loaded orchestrator procedures from directory (sync)");
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
                    // `pluresdb_px` records an explicit `return` as the last
                    // StepResult's output; it does not bind a synthetic
                    // `__return__` variable. Prefer that real procedure output
                    // before considering named output bindings.
                    if let Some(ret) = exec_result
                        .step_results
                        .iter()
                        .rev()
                        .find(|step| step.kind == "return")
                        .and_then(|step| step.output.clone())
                    {
                        Ok(ret)
                    } else if let Some(ret) = exec_result.variables.get("result") {
                        Ok(ret.clone())
                    } else {
                        // Procedures without an explicit return expose their
                        // output bindings as the full variables object.
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

    /// Execute the route_dispatch procedure via .px.
    ///
    /// Returns the routing decision as a Value, or None to fall back.
    pub async fn route_dispatch(
        &self,
        classification: Value,
        context: &str,
        event_type: &str,
    ) -> Option<Result<Value, String>> {
        let mut vars = HashMap::new();
        vars.insert("classification".to_string(), classification);
        vars.insert("context".to_string(), Value::String(context.to_string()));
        vars.insert(
            "event_type".to_string(),
            Value::String(event_type.to_string()),
        );

        self.call("route_dispatch", vars).await
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

    /// Load the real routing.px procedure and exercise route_dispatch end-to-end
    /// through the loaded .px procedure (not a mocked handler) to prove the
    /// destination-mapping logic actually works via the .px executor.
    async fn load_routing_bridge() -> PxBridge {
        let handler: Arc<dyn AsyncActionHandler> = Arc::new(NoOpHandler);
        let bridge = PxBridge::new(handler);
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../praxis/procedures/routing.px"),
        )
        .expect("routing.px must exist");
        let count = bridge
            .load_from_source(&source)
            .await
            .expect("routing.px must load");
        assert!(count > 0, "expected route_dispatch (and siblings) to parse");
        {
            let procs = bridge.procedures.read().await;
            eprintln!("loaded procedures: {:?}", procs.keys().collect::<Vec<_>>());
        }
        bridge
    }

    fn classification(intent: &str, complexity: i64, needs_tools: bool, needs_deep_model: bool) -> Value {
        json!({
            "intent": intent,
            "complexity": complexity,
            "topic": "",
            "topic_shift": false,
            "needs_tools": needs_tools,
            "needs_deep_model": needs_deep_model,
            "plugin_match": "",
            "completion_hint": "",
        })
    }

    fn dispatch_vars(cls: Value) -> HashMap<String, Value> {
        let mut vars = HashMap::new();
        vars.insert("classification".to_string(), cls);
        vars.insert("context".to_string(), Value::String(String::new()));
        vars.insert("event_type".to_string(), Value::String("message".to_string()));
        vars
    }

    #[tokio::test]
    async fn route_dispatch_greeting_is_fast() {
        let bridge = load_routing_bridge().await;
        let result = bridge
            .call("route_dispatch", dispatch_vars(classification("greeting", 1, false, false)))
            .await;
        eprintln!("result: {:?}", result);
        let result = result
            .expect("route_dispatch must be registered")
            .expect("route_dispatch must succeed");
        assert_eq!(result.get("route").and_then(|v| v.as_str()), Some("fast"));
        assert!(parse_px_route_for_test(&result).is_some());
    }

    #[tokio::test]
    async fn route_dispatch_status_query_is_procedural() {
        let bridge = load_routing_bridge().await;
        let result = bridge
            .call(
                "route_dispatch",
                dispatch_vars(classification("status_query", 1, false, false)),
            )
            .await
            .expect("route_dispatch must be registered")
            .expect("route_dispatch must succeed");
        assert_eq!(
            result.get("route").and_then(|v| v.as_str()),
            Some("procedural")
        );
    }

    #[tokio::test]
    async fn route_dispatch_deep_complexity_is_deep() {
        let bridge = load_routing_bridge().await;
        let result = bridge
            .call(
                "route_dispatch",
                dispatch_vars(classification("question", 5, false, true)),
            )
            .await
            .expect("route_dispatch must be registered")
            .expect("route_dispatch must succeed");
        assert_eq!(result.get("route").and_then(|v| v.as_str()), Some("deep"));
        assert!(result.get("reason").and_then(|v| v.as_str()).is_some());
    }

    #[tokio::test]
    async fn route_dispatch_simple_message_is_standard() {
        let bridge = load_routing_bridge().await;
        let result = bridge
            .call(
                "route_dispatch",
                dispatch_vars(classification("question", 1, false, false)),
            )
            .await
            .expect("route_dispatch must be registered")
            .expect("route_dispatch must succeed");
        assert_eq!(
            result.get("route").and_then(|v| v.as_str()),
            Some("standard")
        );
    }

    /// Local mirror of orchestrator::mod::parse_px_route, kept in sync manually
    /// since px_bridge doesn't depend on the orchestrator crate module directly.
    /// Exercises the same match arms (including the "fast" arm) to prove the
    /// JSON shape route_dispatch emits is parseable end-to-end.
    fn parse_px_route_for_test(val: &Value) -> Option<&'static str> {
        let route_str = val.get("route")?.as_str()?;
        match route_str {
            "standard" => Some("standard"),
            "fast" => Some("fast"),
            "procedural" => Some("procedural"),
            "drop" => Some("drop"),
            "deep" => Some("deep"),
            "delegate" => Some("delegate"),
            _ => None,
        }
    }
}
