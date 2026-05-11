# Pares Agens Roadmap

## Role in OASIS
Pares Agens is the agent runtime that powers OASIS’s decentralized multi‑agent orchestration: task decomposition, capability‑based routing, and execution across local + cloud resources. It is the control plane that connects PluresDB procedures, PluresLM memory, and tool execution into a coherent, autonomous system.

## Current State
- **v1.12.1** workspace release (Rust core + Tauri desktop shell) with model routing, MCP tools, PluresDB procedures, and PluresLM memory integration.
- Desktop app is pre‑alpha but core runtime compiles and runs.
- Open work includes CI failures plus Telegram UX commands (/approve, /sessions, /web, /model).

## Milestones

### Phase 1 — Runtime Reliability + CI
- Resolve current CI failures and stabilize release automation.
- Harden crash recovery + persistence for PluresLM/PluresDB state.
- Tighten clippy/test gates across workspace crates.

### Phase 2 — Operator Workflow (Telegram + Approvals)
- Ship /approve and approval flows for high‑stakes actions.
- Add /sessions and /web for operational visibility + quick web lookup.
- Add interactive /model picker for routing configuration.

### Phase 3 — Desktop Experience
- Hotkey activation, clipboard integration, and notification actions.
- Installer UX polish + auto‑update path.
- Telemetry opt‑in with clear privacy controls.

### Phase 4 — OASIS Orchestration
- Task scheduler (crates/agenda) for multi‑step workflows.
- Pares Manus integration for browser/GUI control on capability nodes.
- Session management and context compaction for long‑running commerce flows.
