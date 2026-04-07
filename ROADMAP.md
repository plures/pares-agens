# Pares Agens Roadmap

## Current: v0.5.x (pre-release)

## Phase 1: Core Runtime (v0.6)
- [ ] PluresDB Praxis gate — constraint-aware procedure execution (PR #451)
- [ ] Model router — route tasks to appropriate models (local vs cloud) based on complexity
- [ ] Streaming responses — SSE/WebSocket streaming for chat UI
- [ ] Tool execution sandbox — isolated environment for agent tool calls
- [ ] Session persistence — resume conversations across app restarts

## Phase 2: Memory & Context (v0.7)
- [ ] PluresLM integration — long-term memory via MCP protocol
- [ ] Context window management — automatic summarization when approaching token limits
- [ ] RAG pipeline — retrieve relevant documents before model calls
- [ ] Conversation branching — explore alternative response paths
- [ ] Memory decay — reduce weight of old memories over time

## Phase 3: Multi-Agent (v0.8)
- [ ] Agent-to-agent communication — message passing between procedure-backed agents
- [ ] Coordinator agent — orchestrate task decomposition across specialists
- [ ] Shared memory — agents collaborate through shared PluresDB graph
- [ ] Agent marketplace — discover and install community agents from pares-modulus
- [ ] Audit trail — full decision logging via Chronos

## Phase 4: Desktop Experience (v0.9)
- [ ] System tray presence — background agent accessible from tray
- [ ] Hotkey activation — global shortcut to summon agent overlay
- [ ] File system tools — read/write/search local files with permission gates
- [ ] Clipboard integration — auto-capture context from clipboard
- [ ] Notification actions — actionable notifications from background tasks

## Phase 5: Production (v1.0)
- [ ] Auto-update — Tauri updater with rollback support
- [ ] Telemetry (opt-in) — anonymous usage metrics for improvement
- [ ] Error reporting — crash reports with user consent
- [ ] Plugin API stable — versioned plugin interface with compatibility guarantees
- [ ] Cross-platform installers — .dmg, .msi, .AppImage, .deb, Flatpak

