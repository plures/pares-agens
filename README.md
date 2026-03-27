# Pares Agens

[![CI](https://github.com/plures/pares-agens/actions/workflows/ci.yml/badge.svg)](https://github.com/plures/pares-agens/actions/workflows/ci.yml)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)
[![Crates.io](https://img.shields.io/crates/v/pares-agens-core.svg)](https://crates.io/crates/pares-agens-core)

> *Your AI. Your data. Your machine.*

A reactive AI agent powered by PluresDB procedures, PluresLM memory, and any model you choose — local or cloud. Runs on Windows, macOS, and Linux as a native desktop app. No WSL, no Docker required for end users.

## Table of Contents

- [What Makes It Different](#what-makes-it-different)
- [Features](#features)
- [Model Support](#model-support)
- [Quick Start](#quick-start)
- [API & Usage](#api--usage)
- [Prerequisites](#prerequisites)
- [Build from Source](#build-from-source)
- [Running Tests](#running-tests)
- [Architecture](#architecture)
- [Pricing](#pricing)
- [Pares Sociorum](#pares-sociorum)
- [Status](#status)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

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

## API & Usage

- **Rust API:** `pares-agens-core` is the public crate for embedding the runtime.
  See [`docs/API.md`](docs/API.md) for the main entry points.
- **CLI + Procedure DSL:** see
  [`docs/book/src/api-reference.md`](docs/book/src/api-reference.md).

## Prerequisites

To **use** Pares Agens, just download the installer for your platform — no additional software required.

To **build from source** or contribute, you need:

- [Rust](https://rustup.rs/) stable (1.78 or later)
- [Node.js](https://nodejs.org/) 20+ and npm (for the Tauri UI)
- [Tauri v2 system dependencies](https://tauri.app/start/prerequisites/) for your platform:
  - **Linux:** `libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev`
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`)
  - **Windows:** Microsoft Visual Studio C++ Build Tools + WebView2 (usually pre-installed)

## Build from Source

```bash
# 1. Clone the repository
git clone https://github.com/plures/pares-agens.git
cd pares-agens

# 2. Check that the workspace compiles
cargo check --workspace

# 3. Build all Rust crates
cargo build --workspace

# 4. Install Tauri CLI and build the desktop app
cargo install tauri-cli --version "^2"
cd crates/tauri-app
npm install
cargo tauri build
```

The compiled installer will be under `crates/tauri-app/target/release/bundle/`.

## Running Tests

```bash
# Run the full Rust test suite
cargo test --workspace

# Run tests for a single crate
cargo test -p pares-agens-core

# Run Clippy (must be clean — CI enforces -D warnings)
cargo clippy --workspace --all-targets -- -D warnings

# Run the UI QA suite (requires a production build first)
cd crates/tauri-app && npm install
cargo tauri build
node qa/qa-mvp.mjs
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
- [Getting Started Guide](docs/book/src/getting-started.md)
- [Procedure Authoring](docs/book/src/procedure-authoring.md)
- [API Reference](docs/book/src/api-reference.md)

## Contributing

We welcome contributions! Please follow these steps:

1. **Fork** the repository and create a feature branch:
   ```bash
   git checkout -b feat/your-feature
   ```
2. **Make your changes** — all new features require tests; all bug fixes require a failing test first.
3. **Lint** before pushing:
   ```bash
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```
4. **Commit** using [Conventional Commits](https://www.conventionalcommits.org/):
   ```
   feat(core): add new memory eviction strategy
   fix(ui): correct chat scroll behavior
   ```
5. **Open a Pull Request** — PR titles must follow the same conventional commit format (they become the squash-merge commit message on `main`).

See the [Development Guide](https://github.com/plures/development-guide) for full standards on commit conventions, CI/CD, and the PR workflow.

## License

[AGPL-3.0](LICENSE) © [Plures](https://github.com/plures)
