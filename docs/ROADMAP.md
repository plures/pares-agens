# Pares Agens Roadmap

## Role in Pares Ecosystem
Pares Agens is the AI agent runtime: a reactive event loop that executes PluresDB procedures, routes model calls, and persists memory via PluresLM. It is the “brain” that responds to messages, timers, and state changes across the mesh.

## Current State
Core runtime compiles and includes the executor, procedures, model/memory abstractions, and delegation. The desktop shell exists (Tauri app), but production hardening and deep persistence still need work. Release planning exists and milestone issues define the next hardening phase.

## Milestones

### Near-term (Q2 2026)
- Complete v0.6.0 production hardening tasks (issues #374–#382).
- Stabilize model routing + tool dispatch interfaces and document expected contracts.
- Improve PluresLM state persistence and recovery flows (crash-safe startup).
- Tighten CI: enforce clippy/test gate and QA smoke runs for desktop builds.

### Mid-term (Q3-Q4 2026)
- Integrate Praxis approval gates with UI for high-stakes actions.
- Expand persistence: encrypted secrets, durable state snapshots, and migration tooling.
- Ship model management UX (profiles, per-scope routing, local model discovery).
- Mature MCP client integration with capability discovery and timeouts.

### Long-term
- Production-grade desktop experience (installer UX, auto-update, telemetry opt-in).
- Multi-device sync polish: latency tuning, conflict resolution, and observability.
- Publish stability SLOs for event latency and memory recall quality.
