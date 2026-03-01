# API Reference

Complete reference for IPC commands, the procedure DSL, and built-in MCP tool definitions.

## CLI commands

### `pares agens init`

Initialise a new agent configuration.

```
pares agens init [OPTIONS]

Options:
  --name <NAME>    Agent display name (default: "assistant")
  --config <PATH>  Config file path (default: ~/.config/pares-agens/config.toml)
```

### `pares agens chat`

Start an interactive chat session on the local terminal channel.

```
pares agens chat [OPTIONS]

Options:
  --config <PATH>  Config file path
```

### `pares agens start`

Start the agent as a background daemon (all configured channels).

```
pares agens start [OPTIONS]

Options:
  --config <PATH>  Config file path
  --foreground     Run in foreground instead of daemonising
```

### `pares agens stop`

Stop the running daemon.

### `pares agens status`

Print runtime status (model, memory, channels, sync).

### `pares agens memory`

Manage the pluresLM memory store.

```
pares agens memory <SUBCOMMAND>

Subcommands:
  store    --content <TEXT> --category <CAT> [--tags <TAGS>]
  search   <QUERY> [--limit <N>] [--category <CAT>]
  list     [--limit <N>] [--category <CAT>]
  clear    [--category <CAT>]
  import   --format <FORMAT> <FILE>
  export   --format <FORMAT> [--output <FILE>]
```

### `pares agens config`

Get or set individual config values.

```
pares agens config get <KEY>
pares agens config set <KEY> <VALUE>
```

### `pares agens pro`

Manage the Pro licence.

```
pares agens pro <SUBCOMMAND>

Subcommands:
  status     Print licence status
  activate   <LICENCE-KEY>
  deactivate
  purchase   Open the purchase page in a browser
```

### `pares agens ledger`

Manage the Praxis decision ledger.

```
pares agens ledger <SUBCOMMAND>

Subcommands:
  list    [--limit <N>] [--status pending|approved|rejected]
  export  --format json [--output <FILE>]          # Pro only
```

---

## Procedure DSL

### `Procedure` trait

```rust
pub trait Procedure: Send + Sync {
    /// Unique name used for logging and registration.
    fn name(&self) -> &str;

    /// Return `true` if this procedure should handle `event`.
    fn handles(&self, event: &Event) -> bool;

    /// Process the event and return zero or more follow-up events.
    fn execute(&self, event: &Event) -> Vec<Event>;
}
```

### `Event` enum

```rust
pub enum Event {
    Message {
        content: String,
        channel: String,
        role: String,        // "user" | "assistant"
    },
    ModelResponse {
        content: String,
        channel: String,
    },
    Timer {
        name: String,
    },
    StateChange {
        key: String,
        value: String,
    },
    ToolResult {
        tool_name: String,
        output: String,      // JSON-encoded tool output
    },
}
```

### `MemoryClient` trait

```rust
pub trait MemoryClient: Send + Sync {
    /// Store a memory entry.
    fn capture(&self, entry: MemoryEntry) -> Result<String>;

    /// Return up to `limit` semantically similar memories.
    fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
}

pub struct MemoryEntry {
    pub id: Option<String>,      // Assigned by the store; None on write
    pub content: String,
    pub category: String,
    pub tags: Vec<String>,
}
```

### `ModelClient` trait

```rust
pub trait ModelClient: Send + Sync {
    /// Send a single prompt and return the completion text.
    fn complete(&self, prompt: &str) -> Result<String>;
}
```

---

## Built-in MCP tools

MCP tools are exposed via the agent's tool dispatcher. Tools are invoked by the model when it
determines an action is needed.

### `memory_store`

Store a memory entry.

**Input schema**

```json
{
  "content":  "string (required)",
  "category": "string (required)",
  "tags":     ["string"] 
}
```

**Output**

```json
{ "id": "mem_abc123" }
```

### `memory_recall`

Retrieve semantically similar memories.

**Input schema**

```json
{
  "query": "string (required)",
  "limit": "integer (default: 5)"
}
```

**Output**

```json
[
  { "id": "mem_abc123", "content": "...", "category": "conversation", "tags": [], "score": 0.94 }
]
```

### `state_set`

Set a key/value pair in the agent's in-memory state store. Triggers `Event::StateChange`.

**Input schema**

```json
{
  "key":   "string (required)",
  "value": "string (required)"
}
```

**Output**

```json
{ "ok": true }
```

### `state_get`

Read the current value of a state key.

**Input schema**

```json
{ "key": "string (required)" }
```

**Output**

```json
{ "key": "cpu_temp", "value": "72.4" }
```

### `shell_exec`

Execute a shell command and return stdout/stderr.

> **Security note:** `shell_exec` is disabled by default. Enable it explicitly in `config.toml`:
>
> ```toml
> [tools.shell_exec]
> enabled = true
> allow_list = ["git", "cargo", "ls"]   # Optional allowlist
> ```

**Input schema**

```json
{
  "command": "string (required)",
  "timeout_secs": "integer (default: 30)"
}
```

**Output**

```json
{
  "exit_code": 0,
  "stdout": "...",
  "stderr": ""
}
```

---

## Configuration reference

Full `config.toml` schema:

```toml
# Agent identity
[agent]
name = "my-assistant"

# Model provider
[model]
provider      = "ollama"           # "ollama" | "openai"
base_url      = "http://localhost:11434"
model         = "llama3"
api_key       = ""                 # prefer env var
timeout_secs  = 120
max_tokens    = 4096
temperature   = 0.7
system_prompt = ""

# Optional: distributed model routing (Pro)
[model.dmr]
enabled  = false
nodes    = []
strategy = "round-robin"           # "round-robin" | "least-loaded"

# Communication channels (repeat [[channels]] for multiple)
[[channels]]
type        = "local"              # "local" | "telegram"

[[channels]]
type        = "telegram"
token       = ""                   # prefer TELEGRAM_BOT_TOKEN env var
allowed_ids = []                   # empty = allow all (not recommended)

# Periodic timers (repeat [[timers]] for multiple)
[[timers]]
name     = "heartbeat"
interval = "60s"

# P2P sync (Pro)
[sync]
enabled = false
topic   = ""

# Cloud backup (Pro)
[backup]
enabled  = false
provider = "nubis"

# MCP tool permissions
[tools.shell_exec]
enabled    = false
allow_list = []
```
