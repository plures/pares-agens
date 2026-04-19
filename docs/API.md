# API Overview

This document summarizes the **public Rust API surface** exposed by the core Pares Agens runtime.
For the CLI and procedure DSL reference, see
[`docs/book/src/api-reference.md`](book/src/api-reference.md).

## Crate: `pares-agens-core`

The core crate provides the reactive event loop, procedure registry, and
abstractions for memory/model/tool integrations.

### Event loop

- **`Executor`** (`pares_agens_core::executor::Executor`)
  - `new(registry: ProcedureRegistry) -> Executor`
  - `with_safety_gate(registry, safety_gate) -> Executor`
  - `dispatch(&self, event: &Event) -> Vec<Event>`
  - `run(&self, source: &dyn EventSource, max_iterations: usize)`

- **`EventSource`** (`pares_agens_core::source::EventSource`)
  - `poll_events(&self) -> Vec<Event>`

### Events

- **`Event`** (`pares_agens_core::event::Event`)
  - Variants: `Message`, `Timer`, `StateChange`, `ModelResponse`, `ToolResult`
  - `kind(&self) -> &'static str`

### Procedures

- **`PROCEDURE_REGISTRY_API_VERSION`** (`pares_agens_core::procedure`)
  - Stable semver version for the public registry/plugin API (`"1.0.0"`).

- **`Procedure`** (`pares_agens_core::procedure::Procedure`)
  - `name(&self) -> &str`
  - `handles(&self) -> &str`
  - `execute(&self, event: &Event) -> Vec<Event>`

- **`ProcedureDefinition`** (`pares_agens_core::procedure::ProcedureDefinition`)
  - `new(name, event_type) -> ProcedureDefinition`
  - Fields:
    - `name: String`
    - `event_type: String`
    - `version: String` (semantic version)
    - `registry_api_version: String` (minimum compatible registry API version)

- **`ProcedureLoadError`** (`pares_agens_core::procedure::ProcedureLoadError`)
  - Compatibility and semver validation errors returned during plugin/procedure load.

- **`ProcedureRegistry`** (`pares_agens_core::procedure::ProcedureRegistry`)
  - `register(Box<dyn Procedure>)`
  - `api_version() -> &'static str`
  - `load_definition(definition, procedure) -> Result<(), ProcedureLoadError>`
  - `matching(event_kind: &str) -> impl Iterator<Item = &dyn Procedure>`
  - `enable(name: &str)`, `disable(name: &str)`
  - `set_priority(name: &str, priority: i32)`
  - `list_configs() -> Vec<ProcedureConfig>`

- **`plugin_template_generator`** (`pares_agens_core::procedure`)
  - `plugin_template_generator(plugin_name, event_type) -> String`
  - Generates a starter Rust plugin template with a semver versioned
    `ProcedureDefinition`.

- **Default procedure library** (`pares_agens_core::procedures`)
  - `default_procedure_bundles() -> &[DefaultProcedureBundle]`
  - `default_procedure_bundle(name) -> Option<&DefaultProcedureBundle>`
  - `load_default_procedures(config) -> Result<Vec<DefaultProcedure>, serde_json::Error>`
  - `DefaultProcedureLoadConfig { disabled }` supports disabling shipped defaults
    from persisted config (for example PluresDB state) on first-run import.

### Model + tools

- **`ModelClient`** (`pares_agens_core::model::ModelClient`)
  - `complete(messages: &[ChatMessage], tools: &[ToolDefinition]) -> ModelCompletion`

- **`ToolDispatcher`** (`pares_agens_core::model::ToolDispatcher`)
  - `available_tools() -> Vec<ToolDefinition>`
  - `call_tool(name: &str, arguments: Value) -> String`

- **`ChatMessage`**, **`ToolDefinition`**, **`ToolCall`**, **`ModelCompletion`**
  - Located in `pares_agens_core::model`

### Memory

- **`PluresLm`** (`pares_agens_core::memory::PluresLm`)
  - `new(store, embedder, context_window)`
  - `recall(query, limit, exclude_categories)`
  - `capture(exchange)`
  - `ingest_documents_path(path)` (indexes markdown/txt/source files with embeddings)
  - `inject_context(entries, budget_override)`

### Agent convenience types

`pares_agens_core::agent` provides the `Agent` abstraction and the in-memory
`InMemory` state implementation used in tests and local wiring.

## Examples

### Wiring the event loop

```rust,no_run
use pares_agens_core::{
    executor::Executor,
    procedure::ProcedureRegistry,
    source::EventSource,
};

# #[tokio::main]
# async fn main() {
let registry = ProcedureRegistry::new();
let executor = Executor::new(registry);
// executor.run(&my_source, 0).await;
# }
```

### Defining a procedure

```rust,no_run
use async_trait::async_trait;
use pares_agens_core::{event::Event, procedure::Procedure};

struct OnMessage;

#[async_trait]
impl Procedure for OnMessage {
    fn name(&self) -> &str { "on_message" }
    fn handles(&self) -> &str { "message" }
    async fn execute(&self, event: &Event) -> Vec<Event> {
        // return follow-up events
        vec![]
    }
}
```
