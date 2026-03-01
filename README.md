# Pares Agens

> *Your AI. Your data. Your machine.*

A reactive AI agent powered by PluresDB procedures, PluresLM memory, and any model you choose — local or cloud. Runs on Windows, macOS, and Linux as a native desktop app. No WSL, no Docker required for end users.

## What Makes It Different

**The database IS the agent.** Traditional agent frameworks are thousands of lines of procedural code. Pares Agens inverts this — all agent behavior lives in [PluresDB](https://github.com/plures/pluresdb) reactive procedures. The runtime is a thin event loop. You customize behavior by editing procedures, not forking code.

```
Traditional: message → [5,000 lines of code] → model → [3,000 lines of routing] → response
Pares Agens: message → PluresDB procedure fires → model call → response
```

## Features

- **Any model** — OpenAI, Anthropic Claude, Google Gemini, local models via Docker Model Runner, or any OpenAI-compatible endpoint. Route different tasks to different models (local for cron, cloud for interactive).
- **Persistent memory** — PluresLM auto-captures conversations, decisions, preferences. Recalls relevant context before every response.
- **Reactive procedures** — Agent behavior defined as database procedures that fire on events (messages, timers, state changes). Not code.
- **Decision ledger** — Praxis logs every interaction. High-stakes actions require approval gates before executing.
- **Native on every device** — Desktop app (Windows/macOS/Linux), mobile (iOS/Android), all synced via PluresDB + Hyperswarm. No messages lost, no server required.
- **Offline-capable** — With a local model, everything works without internet.
- **P2P sync** — Hyperswarm connects your devices. Memories sync without a server.
- **Cross-platform nodes** — One agent brain, many hands. Windows, macOS, mobile nodes provide platform capabilities.

## Model Support

| Provider | Models | Use Case |
|----------|--------|----------|
| **Anthropic** | Claude Opus, Sonnet, Haiku | Interactive chat, complex reasoning |
| **OpenAI** | GPT-5, GPT-4o, o1/o3 | General purpose, coding, analysis |
| **Google** | Gemini Pro, Flash | Summarization, multimodal |
| **Docker Model Runner** | Qwen3, Llama, Phi, any GGUF | Free local inference, background work |
| **Any OpenAI-compatible** | Ollama, vLLM, LM Studio, etc. | Self-hosted, custom models |

Configure model routing rules — use cheap/local models for cron jobs and subagents, powerful cloud models for interactive conversation:

```toml
[models.interactive]
provider = "anthropic"
model = "claude-sonnet-4-20250514"

[models.background]
provider = "docker-model-runner"
model = "ai/qwen3"
endpoint = "http://localhost:12434/v1"

[models.coding]
provider = "openai"
model = "gpt-5"
```

## Quick Start

```bash
# Download for your platform
# Windows: pares-agens-setup.msi
# macOS: pares-agens.dmg
# Linux: pares-agens.AppImage

# First run opens a setup wizard:
# 1. Name your agent
# 2. Pick a model (local or enter API key)
# 3. Optionally connect devices (mobile, other desktops — syncs via Hyperswarm)
# 4. Start chatting
```

## Architecture

```
┌──────────────────────────────────────────────┐
│              Tauri Desktop App                │
│           (design-dojo UI / tray)             │
├──────────────────────────────────────────────┤
│              Client Adapters                  │
│    Desktop │ Mobile │ Web │ (Telegram bridge) │
├───────────────────┬──────────────────────────┤
│     PluresDB Core │                           │
│  ┌───────────────┐│  ┌─────────────────────┐ │
│  │  Procedures   ││  │   Model Router      │ │
│  │  • on_message ││  │  Anthropic │ OpenAI │ │
│  │  • on_timer   ││  │  Gemini │ Local DMR │ │
│  │  • on_state   ││  │  Any OpenAI-compat  │ │
│  └───────────────┘│  └─────────────────────┘ │
│  ┌───────────────┐│  ┌─────────────────────┐ │
│  │  PluresLM     ││  │   MCP Tools         │ │
│  │  • memories   ││  │  GitHub │ Brave     │ │
│  │  • embeddings ││  │  Playwright │ etc.  │ │
│  │  • patterns   ││  │  Docker MCP Toolkit │ │
│  └───────────────┘│  └─────────────────────┘ │
│  ┌───────────────┐│                           │
│  │ Praxis Ledger ││                           │
│  │ • audit trail ││                           │
│  │ • approvals   ││                           │
│  └───────────────┘│                           │
└───────────────────┴──────────────────────────┘
```

## Pricing

| | Free | Pro $9/mo |
|---|---|---|
| Local models | ✅ | ✅ |
| Cloud models | ✅ | ✅ |
| PluresLM memory | ✅ | ✅ |
| Devices | 3 | 6 |
| Model routing | — | ✅ |
| P2P sync | ✅ | ✅ |
| MCP tools | ✅ | ✅ |
| Praxis audit | — | ✅ |

## Pares Sociorum

| Product | Role |
|---|---|
| **Pares Arca** | Distributed cache (free tier entry) |
| **Pares Agens** | AI agent ← *you are here* |
| **Pares Manus** | Capability nodes (Windows/macOS/mobile) |
| **Pares Rector** | Goal-based orchestrator |
| **Pares Saxum** | Rock-Lobster plugin (ops interface) |

All built on [PluresDB](https://github.com/plures/pluresdb) + [Hyperswarm](https://github.com/plures/hyperswarm).

## Status

🚧 **Pre-alpha** — Core runtime compiles, procedures execute, model router and MCP client working. Tauri UI next.

## Documentation

- [Architecture](https://github.com/plures/development-guide/blob/main/design/PARES-AGENS.md)
- [Release Plan](https://github.com/plures/development-guide/blob/main/strategy/PARES-AGENS-RELEASE-PLAN.md)
- [Development Guide](https://github.com/plures/development-guide)

## License

AGPL-3.0
