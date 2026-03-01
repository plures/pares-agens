# Procedure Authoring

Extend Pares Agens with custom behaviors by writing typed Rust procedures. Procedures are
first-class event handlers that plug into the reactive event loop.

## Concepts

### Events

Everything in Pares Agens is driven by *events*. An event carries a typed payload and is
dispatched by the executor to all registered handlers:

- `Event::Message` — an inbound chat message from any channel
- `Event::ModelResponse` — a completed model response
- `Event::Timer` — a periodic timer tick
- `Event::StateChange` — an arbitrary key/value state update

### Procedures

A *procedure* is an async handler that receives an event context and returns a `Vec<Event>` of
follow-up events to inject back into the loop. Built-in procedures include `OnMessage`,
`AutoRecall`, and `AutoCapture`.

## Your first procedure

Add `pares-agens-core` to your `Cargo.toml`:

```toml
[dependencies]
pares-agens-core = { git = "https://github.com/plures/pares-agens" }
tokio = { version = "1", features = ["full"] }
```

Implement the `Procedure` trait:

```rust
use pares_agens_core::{Event, Procedure};

pub struct EchoProcedure;

impl Procedure for EchoProcedure {
    fn name(&self) -> &str {
        "echo"
    }

    fn handles(&self, event: &Event) -> bool {
        matches!(event, Event::Message { .. })
    }

    fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Message { content, channel, .. } = event {
            vec![Event::Message {
                content: format!("Echo: {content}"),
                channel: channel.clone(),
                role: "assistant".into(),
            }]
        } else {
            vec![]
        }
    }
}
```

## Example 2 — Timer procedure

Fire a reminder every 60 seconds:

```rust
use pares_agens_core::{Event, Procedure};

pub struct HeartbeatProcedure;

impl Procedure for HeartbeatProcedure {
    fn name(&self) -> &str { "heartbeat" }

    fn handles(&self, event: &Event) -> bool {
        matches!(event, Event::Timer { name, .. } if name == "heartbeat")
    }

    fn execute(&self, _event: &Event) -> Vec<Event> {
        vec![Event::Message {
            content: "💓 Still alive.".into(),
            channel: "local".into(),
            role: "assistant".into(),
        }]
    }
}
```

Register the timer in `config.toml`:

```toml
[[timers]]
name     = "heartbeat"
interval = "60s"
```

## Example 3 — State-change procedure

React when a watched key changes:

```rust
use pares_agens_core::{Event, Procedure};

pub struct AlertOnHighTemp;

impl Procedure for AlertOnHighTemp {
    fn name(&self) -> &str { "alert-on-high-temp" }

    fn handles(&self, event: &Event) -> bool {
        matches!(event, Event::StateChange { key, .. } if key == "cpu_temp")
    }

    fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::StateChange { value, .. } = event {
            let temp: f64 = value.parse().unwrap_or(0.0);
            if temp > 90.0 {
                return vec![Event::Message {
                    content: format!("⚠️ CPU temp critical: {temp}°C"),
                    channel: "local".into(),
                    role: "assistant".into(),
                }];
            }
        }
        vec![]
    }
}
```

## Example 4 — Memory-augmented procedure

Use the `MemoryClient` trait to recall and capture memories:

```rust
use pares_agens_core::{Event, Procedure, memory::MemoryClient};
use std::sync::Arc;

pub struct MemoryAwareProcedure {
    pub memory: Arc<dyn MemoryClient>,
}

impl Procedure for MemoryAwareProcedure {
    fn name(&self) -> &str { "memory-aware" }

    fn handles(&self, event: &Event) -> bool {
        matches!(event, Event::Message { .. })
    }

    fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Message { content, channel, .. } = event {
            // Store every incoming message
            let entry = pares_agens_core::memory::MemoryEntry {
                content: content.clone(),
                category: "conversation".into(),
                tags: vec!["inbound".into()],
            };
            // capture() is synchronous in the free-threaded executor
            let _ = self.memory.capture(entry);

            // Recall related memories for context
            let recalls = self.memory.recall(content, 5).unwrap_or_default();
            let context = recalls.iter()
                .map(|m| format!("- {}", m.content))
                .collect::<Vec<_>>()
                .join("\n");

            vec![Event::ModelResponse {
                content: format!("Context:\n{context}\n\nYou said: {content}"),
                channel: channel.clone(),
            }]
        } else {
            vec![]
        }
    }
}
```

## Example 5 — Model-calling procedure

Delegate to the configured model and return its response:

```rust
use pares_agens_core::{Event, Procedure, model::ModelClient};
use std::sync::Arc;

pub struct LlmProcedure {
    pub model: Arc<dyn ModelClient>,
}

impl Procedure for LlmProcedure {
    fn name(&self) -> &str { "llm" }

    fn handles(&self, event: &Event) -> bool {
        matches!(event, Event::Message { role, .. } if role == "user")
    }

    fn execute(&self, event: &Event) -> Vec<Event> {
        if let Event::Message { content, channel, .. } = event {
            match self.model.complete(content) {
                Ok(response) => vec![Event::ModelResponse {
                    content: response,
                    channel: channel.clone(),
                }],
                Err(e) => vec![Event::Message {
                    content: format!("Model error: {e}"),
                    channel: channel.clone(),
                    role: "assistant".into(),
                }],
            }
        } else {
            vec![]
        }
    }
}
```

## Registering procedures

Register procedures when building the executor:

```rust
use pares_agens_core::Executor;

let executor = Executor::new()
    .register(EchoProcedure)
    .register(HeartbeatProcedure)
    .register(AlertOnHighTemp);

executor.run().await?;
```

## Trigger types summary

| Trigger | Event variant | When fired |
|---|---|---|
| Inbound message | `Event::Message` | User sends a message on any channel |
| Model completion | `Event::ModelResponse` | Model finishes generating a response |
| Timer tick | `Event::Timer` | Scheduled interval fires |
| State change | `Event::StateChange` | A watched key/value pair is updated |
| Tool result | `Event::ToolResult` | MCP tool call completes |

## Built-in variables

When writing procedure logic, these runtime values are always available via the event payload:

| Variable | Source | Description |
|---|---|---|
| `content` | `Event::Message` | The raw text of the message |
| `channel` | `Event::Message` | Channel identifier (`"local"`, `"telegram"`, …) |
| `role` | `Event::Message` | `"user"` or `"assistant"` |
| `name` | `Event::Timer` | Timer name as declared in `config.toml` |
| `key` / `value` | `Event::StateChange` | The state key and its new value |
| `tool_name` / `output` | `Event::ToolResult` | Completed MCP tool name and JSON output |
