# Pares-Agens vs OpenClaw — Competitive Analysis

**Date:** 2026-04-15
**Author:** Chief Architect (mswork)

## Raw Numbers

| Metric | OpenClaw 2026.4.5 | pares-agens 0.6.1 |
|---|---|---|
| **Language** | Node.js / TypeScript | Rust |
| **Install size** | **1.3 GB** | **43 MB** binary |
| **Runtime deps** | 50 npm packages | 0 (static binary) |
| **Skills/Crates** | 53 skills (markdown) | 24 crates (compiled Rust) |
| **Channel adapters** | ~28 | 1 (Telegram) |
| **Tools** | 41 | 3 native + tool loop |
| **Docs** | 405 markdown files | ADR-0014 + integration docs |
| **Tests** | Unknown | 1,025 Rust tests |
| **Code** | ~90K lines JS (dist) | 55K lines Rust (source) |
| **Memory** | PluresLM plugin (MCP HTTP) | PluresDB embedded (native) |
| **Embeddings** | External (Ollama/OpenAI) | Native fastembed (ONNX) |
| **Cold start** | ~3-5s | <500ms |

## Where pares-agens is SUPERIOR

| Capability | pares-agens | OpenClaw |
|---|---|---|
| **Memory** | PluresDB embedded, auto-embed on every write, HNSW vector search, P2P sync | Markdown files + MCP HTTP plugin |
| **Architecture** | 3-consciousness (standard/deep_reasoner/orchestrator) with complexity routing | Single-threaded event loop |
| **Logic engine** | Praxis native Rust (facts, rules, constraints, decision ledger) | None — prompts only |
| **Privacy** | crates/privacy (PII filter) + crates/arca (encrypted vault) | Trust-based |
| **Performance** | 43MB Rust binary, <500ms cold start | 1.3GB Node.js, 3-5s cold start |
| **Sync** | PluresDB Hyperswarm P2P (automatic) | Manual SSH/Tailscale |
| **Audit** | PluresDB write metadata = Chronos (free) | None |
| **Offline** | crates/inference (BitNet local) | Cloud API only |
| **Self-contained** | Single binary, zero deps | Node.js + npm + gateway daemon |
| **Agent loop** | Self-contained handle_event() with tool loop | Adapter-side LLM calls |

## Where OpenClaw is SUPERIOR (gaps to close)

| Capability | OpenClaw | pares-agens | Severity | Fix Path |
|---|---|---|---|---|
| **Channels** | 28 adapters | 1 (Telegram) | 🔴 Critical | Discord, Signal, WhatsApp (v0.7-0.8) |
| **Tools** | 41 (browser, image, TTS, PDF) | 3 native + loop | 🔴 Critical | MCP client exists, cloud API wrappers |
| **Sub-agents** | Full orchestration | Delegation broker (stub) | 🟡 High | Wire crates/core delegation |
| **Browser** | Playwright automation | None | 🟡 High | pares-manus for desktop automation |
| **Skills ecosystem** | 53 skills + marketplace | Procedures (stub) | 🟡 High | PluresDB procedures > markdown |
| **Media gen** | Image/video/music | None | 🟡 Medium | Cloud API wrappers |
| **TTS** | OpenAI + sherpa-onnx | None | 🟡 Medium | Cloud API wrapper |
| **Cron** | Built-in scheduler | crates/agenda (stub) | 🟡 Medium | Wire agenda crate |
| **Web search** | Brave search + fetch | None | 🟡 Medium | reqwest + API key |
| **Community** | Large user base | None | 🟢 Low | Comes with quality |

## Assessment

**pares-agens is architecturally superior but functionally incomplete.**

The foundation is stronger in every dimension — native Rust, embedded DB with auto-embeddings, 3-consciousness routing, Praxis logic engine, P2P sync, privacy crates, audit trail. OpenClaw cannot match this without a complete rewrite.

But OpenClaw has 53 skills and 41 tools that work today. The gap is **tools and channels, not architecture.** Tools are API wrappers — the easiest thing to add.

**Estimated time to functional parity: 2-3 weeks focused.**
**Estimated time to superiority on ALL dimensions: 6-8 weeks.**

The competitive moat is the plures stack — no one else has an embedded CRDT graph DB with auto-embeddings, a native logic engine, and 3-consciousness routing in a single binary.
