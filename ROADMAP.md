# Pares Agens Roadmap

## ✅ Completed: v0.5.x (Dogfood Ready)
- [x] PluresDB-backed memory with native fastembed (BAAI/bge-small-en-v1.5)
- [x] Telegram adapter (teloxide)
- [x] 3-consciousness Cerebellum (GPT-4.1 conscious + Opus 4.6 deep)
- [x] 7 tools (run_command, read/write/edit_file, web_search/fetch, list_directory)
- [x] Copilot OAuth device flow
- [x] Conversation persistence in PluresDB (ChatTurn)
- [x] DelegationBroker with 3 built-in subagents
- [x] Logprobs confidence escalation
- [x] Fact autocapture + procedure writer

## Phase 1: Core Runtime (v0.6)
- [x] PluresDB Praxis gate — constraint-aware procedure execution
- [x] Model router — Cerebellum routes to GPT-4.1/Opus 4.6/subagents
- [ ] Streaming responses — SSE/WebSocket for chat UI (#510)
- [x] Tool execution — 7 tools via ProcedureToolDispatcher
- [x] Session persistence — ChatTurn in PluresDB

## Phase 2: Memory & Context (v0.7)
- [x] PluresDB integration — native fastembed, auto-embed on every put()
- [ ] Context window management — auto-summarize at token limit (#511)
- [ ] RAG pipeline — document ingestion + retrieval (#512)
- [ ] Conversation branching — alternative response paths (#513)
- [x] Memory decay — forgetting engine with retention policies
- [ ] Hyperswarm sync — multi-host memory replication (#495)
- [ ] Encrypted PluresDB — at rest + in transit (#496)
- [ ] Slash commands for topic/key management (#497)
- [ ] Adapter conflict detection C-ADAPTER-001 (#498)
- [ ] Setup wizard for Hyperswarm (#499)

## Phase 3: Multi-Agent (v0.8)
- [x] Agent-to-agent communication — DelegationBroker
- [x] Coordinator agent — Cerebellum routes to specialists
- [x] Shared memory — all agents share PluresDB
- [ ] Agent marketplace — install from pares-modulus (#514)
- [ ] Audit trail — Chronos decision logging (#515)

## Phase 4: Desktop Experience (v0.9)
- [x] System tray — crates/tauri-app/src/tray.rs
- [x] Chat UI — Svelte 5 + design-dojo + PluresDB history
- [ ] Hotkey activation — global shortcut (#516)
- [ ] Clipboard integration — auto-capture context (#517)
- [ ] Notification actions — actionable desktop notifications (#518)

## Phase 5: Production (v1.0)
- [ ] NixOS service deployed on praxisbot (#502)
- [ ] Context management — topic detection (#503)
- [ ] Proactive org monitoring — CI, PRs, issues (#504)
- [ ] Direct code fixes — clone, fix, push (#505)
- [ ] Self-update via NixOS rebuild (#506)
- [x] Telegram slash commands — /start /help /status (#507)
- [ ] Production hardening — error recovery, logging (#508)
- [ ] Opt-in telemetry (#519)
- [ ] Plugin API stable (#520)
- [ ] Cross-platform installers (#521)

## Summary

| Phase | Total | Done | Remaining |
|-------|-------|------|-----------|
| v0.6 | 5 | 4 | 1 |
| v0.7 | 10 | 2 | 8 |
| v0.8 | 5 | 3 | 2 |
| v0.9 | 5 | 2 | 3 |
| v1.0 | 10 | 1 | 9 |
| **Total** | **35** | **12** | **23** |
