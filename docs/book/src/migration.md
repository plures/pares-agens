# Migration from OpenClaw

This guide walks you through migrating an existing **OpenClaw** installation to Pares Agens.

## Overview of changes

| Concept | OpenClaw | Pares Agens |
|---|---|---|
| Configuration | `openclaw.json` | `~/.config/pares-agens/config.toml` |
| Plugin system | TypeScript plugins | Rust procedures |
| Memory store | SQLite + custom schema | PluresDB (local vector store) |
| Model config | Per-plugin model key | Central `[model]` section |
| Telegram channel | Plugin: `@openclaw/telegram` | Built-in `[[channels]]` entry |
| CLI | `openclaw` | `pares` |

## Before you start

1. **Back up your OpenClaw data:**

   ```sh
   cp -r ~/.openclaw ~/.openclaw-backup-$(date +%Y%m%d)
   ```

2. **Note your current model and API key settings** — you will re-enter them in the new config.

3. **Export your OpenClaw memory** (if you want to migrate conversation history):

   ```sh
   openclaw memory export --format json > openclaw-memory.json
   ```

## Step 1 — Install Pares Agens

```sh
curl -sSL install-pares.plures.io | sh
```

Verify the install:

```sh
pares --version
# pares-agens 0.1.0
```

## Step 2 — Initialise

```sh
pares agens init --name="my-assistant"
```

This creates `~/.config/pares-agens/config.toml`.

## Step 3 — Migrate model configuration

Open your old `openclaw.json` and locate the model settings. Translate them into the new format:

**OpenClaw (`openclaw.json`)**

```json
{
  "model": {
    "provider": "openai",
    "apiKey": "sk-...",
    "model": "gpt-4o"
  }
}
```

**Pares Agens (`config.toml`)**

```toml
[model]
provider = "openai"
base_url = "https://api.openai.com/v1"
model    = "gpt-4o"
# Set OPENAI_API_KEY env var instead of storing the key here
```

See [Model Configuration](model-configuration.md) for the full reference.

## Step 4 — Migrate channel configuration

**OpenClaw**

```json
{
  "plugins": [
    { "name": "@openclaw/telegram", "token": "123456789:ABCdef..." }
  ]
}
```

**Pares Agens**

```toml
[[channels]]
type  = "telegram"
token = "123456789:ABCdef..."
allowed_ids = [987654321]
```

See [Channel Setup](channel-setup.md) for all options.

## Step 5 — Import OpenClaw memory (optional)

If you exported your memory in Step 0, import it into pluresLM:

```sh
pares agens memory import --format openclaw openclaw-memory.json
```

This converts each OpenClaw memory entry into a pluresLM `conversation` memory with the
original timestamp preserved.

> **Note:** Only text-based memories are migrated. Attachments and binary data are skipped.

## Step 6 — Migrate custom plugins

OpenClaw plugins written in TypeScript need to be rewritten as Rust procedures. The concepts
map directly:

| OpenClaw plugin | Pares Agens equivalent |
|---|---|
| `onMessage` handler | `Procedure` with `handles(Event::Message)` |
| `onTimer` handler | `Procedure` with `handles(Event::Timer)` |
| `memory.store()` | `MemoryClient::capture()` |
| `memory.search()` | `MemoryClient::recall()` |
| `model.complete()` | `ModelClient::complete()` |

See [Procedure Authoring](procedure-authoring.md) for full examples.

## Step 7 — Test the migration

```sh
pares agens start
pares agens chat
```

Send a test message. Verify the agent responds and memory is working:

```sh
pares agens memory list --limit 5
```

## Keeping OpenClaw running during transition

You can run both agents simultaneously on different ports. Pares Agens defaults to no
network port (local stdin/stdout only) so there is no port conflict with OpenClaw's HTTP server.

## Getting help

If you run into issues during migration, open an issue on
[GitHub](https://github.com/plures/pares-agens/issues) with the `migration` label.
