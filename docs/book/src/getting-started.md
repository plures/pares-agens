# Getting Started

Install Pares Agens, connect a model, and send your first message — in under 5 minutes.

## Prerequisites

- Linux, macOS, or Windows (WSL2 supported)
- A running **Ollama** instance ([ollama.com](https://ollama.com)) *or* any OpenAI-compatible API endpoint
- ~500 MB disk space for the agent binary and local database

> **💡 Quickest path**
> Install Ollama, pull `llama3`, then follow the steps below. You'll be chatting in under 5 minutes.

## Step 1 — Install

### Universal installer (Linux / macOS)

```sh
curl -sSL install-pares.plures.io | sh
```

### Nix / NixOS

```sh
# Nix profile
nix profile install github:plures/pares-agens

# NixOS module
services.pares-agens.enable = true;
```

### Windows

Download the `.msi` installer from the [latest release](https://github.com/plures/pares-agens/releases/latest)
and run it. The `pares` command will be added to your PATH automatically.

### Build from source

```sh
git clone https://github.com/plures/pares-agens
cd pares-agens
cargo build --release
# binary at ./target/release/pares
```

## Step 2 — Initialise your agent

```sh
pares agens init --name="my-assistant"
```

This creates a configuration file at `~/.config/pares-agens/config.toml` and initializes a local
PluresDB database for memory storage.

## Step 3 — Configure a model

Edit `~/.config/pares-agens/config.toml` and set the model endpoint:

```toml
[model]
provider = "ollama"          # or "openai" for OpenAI-compatible APIs
base_url = "http://localhost:11434"
model    = "llama3"
```

See the [Model Configuration](model-configuration.md) guide for all options including local Qwen3
clusters and cloud APIs.

## Step 4 — Start chatting

```sh
pares agens chat
```

You should see:

```
pares-agens v0.1.0
Model:   ollama/llama3 @ localhost:11434
Memory:  PluresDB (local, 0 memories)
Channel: local

You:
```

Type a message and press `Enter`. The agent will respond using the configured model and
automatically store the conversation in local memory.

> **✅ That's it!**
> You're now running a fully local, offline-capable AI agent. Everything stays on your machine.

## Verify the agent is working

```sh
pares agens status
```

```
✅ Model:    ollama/llama3 (connected)
✅ Memory:   PluresDB local (42 memories)
✅ Channels: local
⬜ P2P sync: disabled (Pro feature)
```

## Next steps

- [Author custom procedures](procedure-authoring.md) to give your agent new behaviours
- [Configure a different model](model-configuration.md) — local Qwen3, cloud OpenAI, Anthropic-compatible
- [Set up Telegram](channel-setup.md) so you can chat from your phone
- [Explore the API Reference](api-reference.md) to connect external services
