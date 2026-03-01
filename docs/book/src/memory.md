# PluresLM Memory

Pares Agens uses **pluresLM** as its memory substrate. Every conversation, code pattern, and
decision is stored in a local vector store and recalled automatically to provide the agent with
relevant context.

## How it works

When you send a message the agent:

1. **Recalls** the top-k most semantically similar memories (via `MemoryClient::recall`)
2. Injects those memories as context into the model prompt
3. Generates a response
4. **Captures** the full turn (user message + response) back into memory (via `MemoryClient::capture`)

This loop happens automatically through the built-in `AutoRecall` and `AutoCapture` procedures.

## Memory categories

Each memory entry has a `category` field that controls how it is indexed and recalled:

| Category | Description | Stored automatically |
|---|---|---|
| `conversation` | Chat turns (user + assistant) | ✅ Yes |
| `code-pattern` | Code snippets and explanations | ✅ Yes (when code detected) |
| `error-fix` | Error messages paired with their solutions | ✅ Yes |
| `preference` | Expressed user preferences | ✅ Yes |
| `decision` | Significant decisions and their rationale | ✅ Yes |
| `custom` | Any category you define | Manual only |

## Manually storing a memory

```sh
pares agens memory store \
  --content "Prefer tabs over spaces in all Rust files" \
  --category preference \
  --tags "rust,formatting"
```

Or from within the chat:

```
You: /memory store "Deploy always runs on Friday afternoons" --category decision
```

## Manually searching memories

```sh
pares agens memory search "rust formatting preferences"
```

```
Score  Category     Content
0.94   preference   Prefer tabs over spaces in all Rust files
0.81   decision     Agreed to use rustfmt defaults for new crates
0.72   code-pattern Example of cargo fmt configuration in Cargo.toml
```

## Viewing recent memories

```sh
pares agens memory list --limit 20
```

```sh
# Filter by category
pares agens memory list --category preference
```

## Memory storage

By default memory is stored in a local PluresDB database at:

```
~/.local/share/pares-agens/memory.db
```

The database uses a flat-file format with AES-256 encryption at rest. No cloud sync occurs on
the Free plan.

## Clearing memory

```sh
# Clear all memories (irreversible)
pares agens memory clear

# Clear a specific category
pares agens memory clear --category conversation
```

## Semantic search under the hood

pluresLM uses a local embedding model (sentence-transformers compatible) to convert text into
vectors. Recall queries perform approximate nearest-neighbour search using HNSW. The embedding
model runs entirely on your CPU — no network calls are made.

## Cross-device memory sync (Pro)

With a Pro licence, memories sync across all your devices via Hyperswarm P2P:

```toml
[sync]
enabled = true
topic   = "my-private-topic-key"   # shared secret between your devices
```

See [Pro Features](pro-features.md) for details.
