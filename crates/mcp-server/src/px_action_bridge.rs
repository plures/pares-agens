//! Bridge between .px procedure executor and Radix MCP tools.
//!
//! This is the integration layer: .px procedures call actions by name,
//! and this bridge routes them to Radix's MCP tool implementations.
//!
//! Architecture:
//!   .px procedure step: read_file {path: "/foo/bar.txt"}
//!     → ActionHandler::call("read_file", {"path": "/foo/bar.txt"})
//!       → RadixToolHandler::dispatch_tool("read_file", {"path": "/foo/bar.txt"})
//!         → actual file read
//!           → Value result back to .px executor

use std::collections::HashMap;
use std::sync::Arc;

use praxis_native::px::executor::{ActionHandler, ExecutionError};
use serde_json::Value;
use tokio::runtime::Handle;

use crate::handler::ToolHandler;
use crate::radix_handler::RadixToolHandler;

/// Bridge that routes .px procedure action calls to Radix MCP tool dispatch.
pub struct PxActionBridge {
    handler: Arc<RadixToolHandler>,
}

impl PxActionBridge {
    /// Create a new bridge wrapping a `RadixToolHandler`.
    pub fn new(handler: Arc<RadixToolHandler>) -> Self {
        Self { handler }
    }
}

impl ActionHandler for PxActionBridge {
    fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError> {
        // call_tool is async; block on it from the sync ActionHandler interface.
        // This assumes a Tokio runtime is active on the current thread.
        let handler = Arc::clone(&self.handler);
        let name_owned = name.to_owned();
        let params_owned = params.clone();

        let result = Handle::current()
            .block_on(async move { handler.call_tool(&name_owned, params_owned).await });

        if result.is_error {
            Err(ExecutionError::ActionFailed {
                action: name.to_owned(),
                message: result.content,
            })
        } else {
            // Return the content as a JSON string value.
            // Attempt to parse as JSON first; fall back to string.
            let value = match serde_json::from_str(&result.content) {
                Ok(v) => v,
                Err(_) => Value::String(result.content),
            };
            Ok(value)
        }
    }

    fn evaluate_condition(&self, expr: &str, vars: &HashMap<String, Value>) -> bool {
        // Delegate to default implementation.
        praxis_native::px::executor::default_evaluate_condition(expr, vars)
    }
}
