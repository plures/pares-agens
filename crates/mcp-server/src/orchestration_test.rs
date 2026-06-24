//! End-to-end integration test for .px procedure orchestration.
//!
//! Tests the full pipeline:
//!   .px source → compiler → executor → ActionHandler bridge → MCP tools → PluresDB → verify

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use praxis_native::px::executor::{execute, ActionHandler, ExecutionError};
    use praxis_native::px::{compiler::compile, parse};

    // ── Mock ActionHandler for Testing ──────────────────────────────────────────

    /// Mock action handler that simulates db_get, db_put, and native functions
    /// without requiring a full Radix runtime.
    #[derive(Clone)]
    struct MockActionHandler {
        /// Simulated database state
        db: Arc<Mutex<HashMap<String, Value>>>,
        /// Record of all calls made to this handler (for verification)
        call_log: Arc<Mutex<Vec<(String, Value)>>>,
    }

    impl MockActionHandler {
        fn new() -> Self {
            Self {
                db: Arc::new(Mutex::new(HashMap::new())),
                call_log: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Pre-seed a value in the simulated database
        fn seed(&self, key: &str, value: Value) {
            let mut db = self.db.lock().unwrap();
            db.insert(key.to_string(), value);
        }

        /// Get a value from the simulated database
        fn get_db(&self, key: &str) -> Option<Value> {
            let db = self.db.lock().unwrap();
            db.get(key).cloned()
        }

        /// Get the call log for verification
        fn get_call_log(&self) -> Vec<(String, Value)> {
            let log = self.call_log.lock().unwrap();
            log.clone()
        }
    }

    impl ActionHandler for MockActionHandler {
        fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError> {
            // Record the call
            {
                let mut log = self.call_log.lock().unwrap();
                log.push((name.to_string(), params.clone()));
            }

            match name {
                "db_get" => {
                    let key = params.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                        ExecutionError::ActionFailed {
                            action: name.to_string(),
                            message: "missing required parameter: key".into(),
                        }
                    })?;

                    let db = self.db.lock().unwrap();
                    let value = db.get(key).cloned().unwrap_or(Value::Null);
                    Ok(value)
                }

                "db_put" => {
                    let key = params.get("key").and_then(|v| v.as_str()).ok_or_else(|| {
                        ExecutionError::ActionFailed {
                            action: name.to_string(),
                            message: "missing required parameter: key".into(),
                        }
                    })?;

                    let value = params.get("value").cloned().ok_or_else(|| {
                        ExecutionError::ActionFailed {
                            action: name.to_string(),
                            message: "missing required parameter: value".into(),
                        }
                    })?;

                    let mut db = self.db.lock().unwrap();
                    db.insert(key.to_string(), value.clone());
                    Ok(json!({"status": "ok", "key": key}))
                }

                "string_upper" => {
                    // Simple native function that uppercases a string
                    let input = params.get("input").and_then(|v| v.as_str()).unwrap_or("");
                    Ok(Value::String(input.to_uppercase()))
                }

                _ => Err(ExecutionError::UnknownAction(name.to_string())),
            }
        }
    }

    // ── Test Cases ───────────────────────────────────────────────────────────────

    #[test]
    fn test_orchestration_simple_pipeline() {
        // Create mock handler with seeded data
        let handler = MockActionHandler::new();
        handler.seed("test:input", json!("hello world"));

        // Compile a simple procedure
        let source = r#"
procedure test_orchestration:
  trigger: manual
  given: "Test the full orchestration pipeline"
  db_get key: "test:input" -> $input
  db_put key: "test:output" value: $input
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record = &compiled[0];

        // Execute the procedure
        let result = execute(&record.data, &handler).expect("execution failed");

        // Verify execution succeeded
        assert!(result.success, "procedure should succeed");
        assert_eq!(result.procedure_name, "test_orchestration");
        assert_eq!(result.step_results.len(), 2, "should have 2 steps");

        // Verify db_get was called first
        let call_log = handler.get_call_log();
        assert_eq!(call_log.len(), 2, "should have 2 tool calls");
        assert_eq!(call_log[0].0, "db_get");
        assert_eq!(call_log[0].1, json!({"key": "test:input"}));

        // Verify db_put was called second
        assert_eq!(call_log[1].0, "db_put");
        assert_eq!(
            call_log[1].1,
            json!({"key": "test:output", "value": "hello world"})
        );

        // Verify final database state
        assert_eq!(
            handler.get_db("test:output"),
            Some(json!("hello world")),
            "output should be written to database"
        );

        // Verify variable bindings
        assert_eq!(
            result.variables.get("input"),
            Some(&json!("hello world")),
            "input variable should be bound"
        );
    }

    #[test]
    fn test_orchestration_with_transformation() {
        // Create mock handler with seeded data
        let handler = MockActionHandler::new();
        handler.seed("test:name", json!("radix"));

        // Compile a procedure that transforms data
        let source = r#"
procedure test_transform:
  trigger: manual
  given: "Test data transformation in pipeline"
  db_get key: "test:name" -> $name
  string_upper input: $name -> $upper
  db_put key: "test:upper_name" value: $upper
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record = &compiled[0];

        // Execute the procedure
        let result = execute(&record.data, &handler).expect("execution failed");

        // Verify execution succeeded
        assert!(result.success, "procedure should succeed");
        assert_eq!(result.step_results.len(), 3, "should have 3 steps");

        // Verify call sequence
        let call_log = handler.get_call_log();
        assert_eq!(call_log.len(), 3, "should have 3 tool calls");
        assert_eq!(call_log[0].0, "db_get");
        assert_eq!(call_log[1].0, "string_upper");
        assert_eq!(call_log[1].1, json!({"input": "radix"}));
        assert_eq!(call_log[2].0, "db_put");

        // Verify final database state
        assert_eq!(
            handler.get_db("test:upper_name"),
            Some(json!("RADIX")),
            "transformed value should be written"
        );

        // Verify variable bindings
        assert_eq!(result.variables.get("name"), Some(&json!("radix")));
        assert_eq!(result.variables.get("upper"), Some(&json!("RADIX")));
    }

    #[test]
    fn test_orchestration_missing_key() {
        // Create mock handler WITHOUT seeding data
        let handler = MockActionHandler::new();

        let source = r#"
procedure test_missing:
  trigger: manual
  given: "Test handling of missing database key"
  db_get key: "test:nonexistent" -> $value
  db_put key: "test:result" value: $value
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record = &compiled[0];

        // Execute the procedure
        let result = execute(&record.data, &handler).expect("execution failed");

        // Verify execution succeeded (missing key returns null, not error)
        assert!(result.success, "procedure should succeed with null");

        // Verify null was written
        assert_eq!(
            handler.get_db("test:result"),
            Some(Value::Null),
            "null should be written for missing key"
        );

        // Verify variable contains null
        assert_eq!(
            result.variables.get("value"),
            Some(&Value::Null),
            "variable should be bound to null"
        );
    }

    #[test]
    fn test_orchestration_unknown_action() {
        let handler = MockActionHandler::new();

        let source = r#"
procedure test_unknown:
  trigger: manual
  given: "Test handling of unknown action"
  nonexistent_action param: "value"
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record = &compiled[0];

        // Execute the procedure — should fail
        let result = execute(&record.data, &handler);

        assert!(result.is_err(), "should fail when calling unknown action");

        if let Err(ExecutionError::UnknownAction(name)) = result {
            assert_eq!(name, "nonexistent_action");
        } else {
            panic!("expected UnknownAction error");
        }
    }

    #[test]
    fn test_orchestration_multiple_reads_writes() {
        // Test a more complex procedure with multiple database operations
        let handler = MockActionHandler::new();
        handler.seed("counter:a", json!(5));
        handler.seed("counter:b", json!(10));

        let source = r#"
procedure test_multi:
  trigger: manual
  given: "Test multiple database reads and writes"
  db_get key: "counter:a" -> $a
  db_get key: "counter:b" -> $b
  db_put key: "result:first" value: $a
  db_put key: "result:second" value: $b
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record = &compiled[0];

        let result = execute(&record.data, &handler).expect("execution failed");

        assert!(result.success);
        assert_eq!(result.step_results.len(), 4);

        // Verify both reads happened first
        let call_log = handler.get_call_log();
        assert_eq!(call_log[0].0, "db_get");
        assert_eq!(call_log[1].0, "db_get");
        assert_eq!(call_log[2].0, "db_put");
        assert_eq!(call_log[3].0, "db_put");

        // Verify writes
        assert_eq!(handler.get_db("result:first"), Some(json!(5)));
        assert_eq!(handler.get_db("result:second"), Some(json!(10)));

        // Verify variables
        assert_eq!(result.variables.get("a"), Some(&json!(5)));
        assert_eq!(result.variables.get("b"), Some(&json!(10)));
    }
}

/// Integration tests using the real PxActionBridge → RadixToolHandler → StateStore.
///
/// These prove the PRODUCTION path works: .px procedures execute through the
/// actual tool dispatch, not a mock.
#[cfg(test)]
mod bridge_integration_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;

    use praxis_native::px::executor::execute;
    use praxis_native::px::{compiler::compile, parse};

    use pares_radix_core::shell_executor::ShellExecutor;
    use pares_radix_core::InMemoryStateStore;
    use pares_radix_core::StateStore;

    use crate::px_action_bridge::PxActionBridge;
    use crate::radix_handler::RadixToolHandler;

    fn make_bridge() -> (PxActionBridge, Arc<InMemoryStateStore>) {
        let shell = Arc::new(ShellExecutor::new());
        let state = Arc::new(InMemoryStateStore::new());
        let handler = Arc::new(
            RadixToolHandler::new(shell, PathBuf::from("/tmp")).with_state_store(state.clone()),
        );
        (PxActionBridge::new(handler), state)
    }

    #[tokio::test]
    async fn bridge_db_put_then_get() {
        let (bridge, state) = make_bridge();

        let source = r#"
procedure bridge_roundtrip:
  trigger: manual
  given: "Test real bridge: write to DB then read back"
  db_put key: "bridge:test" value: "hello from .px"
  db_get key: "bridge:test" -> $result
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record_data = compiled[0].data.clone();

        let result = tokio::task::spawn_blocking(move || execute(&record_data, &bridge))
            .await
            .expect("spawn_blocking panicked");

        let result = result.expect("execution failed");
        assert!(result.success, "procedure should succeed");
        assert_eq!(result.step_results.len(), 2);

        // Verify state was actually persisted through the real StateStore
        let stored = state.get("bridge:test").await;
        assert_eq!(stored, Some(json!("hello from .px")));
    }

    #[tokio::test]
    async fn bridge_db_overwrite() {
        let (bridge, state) = make_bridge();

        let source = r#"
procedure bridge_overwrite:
  trigger: manual
  given: "Test real bridge: overwrite existing key"
  db_put key: "counter" value: 1
  db_put key: "counter" value: 2
  db_get key: "counter" -> $final_val
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record_data = compiled[0].data.clone();

        let result = tokio::task::spawn_blocking(move || execute(&record_data, &bridge))
            .await
            .expect("spawn_blocking panicked");

        let result = result.expect("execution failed");
        assert!(result.success);
        assert_eq!(result.step_results.len(), 3);

        // Final value should be 2 (overwritten)
        let stored = state.get("counter").await;
        assert_eq!(stored, Some(json!(2)));
    }

    #[tokio::test]
    async fn bridge_db_delete() {
        let (bridge, state) = make_bridge();

        let source = r#"
procedure bridge_delete:
  trigger: manual
  given: "Test real bridge: delete a key"
  db_put key: "ephemeral" value: "will be deleted"
  db_delete key: "ephemeral"
  db_get key: "ephemeral" -> $gone
"#;

        let doc = parse(source).expect("parse failed");
        let compiled = compile(&doc);
        let record_data = compiled[0].data.clone();

        let result = tokio::task::spawn_blocking(move || execute(&record_data, &bridge))
            .await
            .expect("spawn_blocking panicked");

        let result = result.expect("execution failed");
        assert!(result.success);

        // Key should be gone (RadixToolHandler now uses store.delete)
        let stored = state.get("ephemeral").await;
        assert_eq!(stored, None);
    }
}
