# ADR-0014: Pares Agens Architecture — Full Plures Stack Integration

**Status:** PROPOSED  
**Date:** 2026-04-15  
**Author:** Chief Architect (mswork) + Paradox  
**Enforcement:** This ADR is stored in PluresDB, embedded in Praxis constraints, and enforced by CI expectations.

## Context

pares-agens was designed as the AI agent framework for the Pares ecosystem. We built 20+ technologies (PluresDB, Praxis, Chronos, design-dojo, pares-manus, etc.) specifically for use in pares-agens. Currently, most are either missing from the integration or used superficially. The serve command was wired with external HTTP services (Ollama, MCP) instead of native embedded crates.

**Problem:** pares-agens uses ~30% of the plures stack. The remaining 70% sits in separate repos, untested together, losing the compound advantage of full integration.

## Decision

pares-agens MUST use the full plures technology stack natively. No external services for core capabilities. Every capability that exists in a plures repo MUST be integrated as a Rust crate dependency, not an HTTP sidecar.

## Architecture Layers

```
┌─────────────────────────────────────────────────────────────┐
│                    CHANNEL ADAPTERS                          │
│  Telegram · Discord · Signal · WhatsApp · pares-manus (GUI) │
│  crates/channels + pares-protocol for wire format            │
└─────────────────┬───────────────────────────────────────────┘
                  │ Event
┌─────────────────▼───────────────────────────────────────────┐
│                   CEREBELLUM (3-consciousness)               │
│  Conscious: direct response path (fast)                      │
│  Subconscious: deep analysis, background tasks (async)       │
│  Cerebellum: routing, complexity detection, coordination     │
│  crates/core/cerebellum + pluresdb-procedures                │
└─────────────────┬───────────────────────────────────────────┘
                  │ Routed Event + Recalled Context
┌─────────────────▼───────────────────────────────────────────┐
│                   PRAXIS ENGINE                              │
│  Facts · Rules · Constraints · Events · Decision Ledger      │
│  crates/praxis (native Rust, NOT @plures/praxis npm)         │
│  Approval gates · Authorization · Constraint enforcement     │
└─────────────────┬───────────────────────────────────────────┘
                  │ Validated Action
┌─────────────────▼───────────────────────────────────────────┐
│                   AGENT EXECUTOR                             │
│  Tool dispatch (native procedures, not HTTP)                 │
│  LLM routing (crates/models — OpenAI-compatible)             │
│  Inference (crates/inference — BitNet local, cloud fallback) │
│  Privacy filter (crates/privacy — PII redaction)             │
│  Audit log (crates/audit + Chronos state chronicle)          │
└─────────────────┬───────────────────────────────────────────┘
                  │ Read/Write
┌─────────────────▼───────────────────────────────────────────┐
│                   STORAGE LAYER                              │
│  PluresDB: memory, state, procedures, vector search          │
│  Chronos: temporal state diffs, causal chains                │
│  plures-vault (crates/arca): encrypted secrets               │
│  plures-object: large file/artifact storage                  │
│  ALL persistent state in PluresDB — zero JSON config files   │
└─────────────────┬───────────────────────────────────────────┘
                  │ Sync
┌─────────────────▼───────────────────────────────────────────┐
│                   P2P SYNC LAYER                             │
│  crates/sync: Hyperswarm DHT for device-to-device sync       │
│  crates/dmem: distributed memory mesh                        │
│  pares-protocol: encrypted wire format                       │
│  GitHub relay: fallback for corporate networks               │
└─────────────────────────────────────────────────────────────┘
```

## Technology Integration Requirements

### MUST USE (core dependencies)

| Technology | Crate | Role | Integration Point |
|---|---|---|---|
| **PluresDB** | `pluresdb` | ALL persistent state | `core/memory/store.rs`, cerebellum bridge |
| **Praxis** | `crates/praxis` | ALL business logic, constraints, rules | Every action goes through praxis gates |
| **Cerebellum** | `core/cerebellum` | 3-consciousness routing | Event processing pipeline |
| **PluresDB Procedures** | `pluresdb-procedures` | Reactive data-driven procedures | Cerebellum bridge, background tasks |
| **Chronos** | NEW dependency | State chronicle, audit trail | Every agent action gets a Chronos entry |
| **design-dojo** | `crates/tauri-app/ui` | ALL UI components | Tauri frontend |
| **plures-vault** | `crates/arca` | ALL secret storage | API keys, tokens — NEVER env vars in production |

### SHOULD USE (planned integration)

| Technology | Crate | Role | When |
|---|---|---|---|
| **pares-manus** | NEW crate/dep | Desktop automation, screen capture | v0.7 |
| **plures-object** | NEW dep | Large file storage, build artifacts | v0.7 |
| **pares-radix** | Plugin loader | Third-party procedure loading | v0.8 |
| **transformers-rs** | Embedding provider | Native ONNX embeddings | v0.7 (replace MockEmbedder) |
| **pares-protocol** | Wire format | P2P communication | v0.8 |
| **pares-cache** | Build cache | Nix binary cache for self-builds | Infrastructure |

### MUST NOT USE

| Anti-pattern | Why | Alternative |
|---|---|---|
| Ollama for embeddings | External service dependency | transformers-rs (native ONNX) or PluresDB fastembed |
| MCP HTTP for memory | Network hop for core function | PluresDB direct embed |
| JSON config files | Not auditable, no sync, no versioning | PluresDB state store |
| Node.js anything | Wrong runtime for Rust agent | Native Rust or compile to WASM |
| External cron services | Fragile, no context | crates/agenda (native scheduler) |

## Praxis Compliance

Every component in pares-agens MUST be expressible as Praxis primitives:

1. **Channel adapters** → Praxis Facts (message received, user context)
2. **Cerebellum routing** → Praxis Rules (if complexity > threshold → subconscious)
3. **Tool execution** → Praxis Constraints (authorization gates before shell exec)
4. **Memory capture** → Praxis Events (conversation stored, memory consolidated)
5. **Decision making** → Praxis Decision Ledger (every significant choice recorded)

### Enforcement Mechanism

This ADR is enforced at three levels:

1. **CI expectations** — `crates/praxis/expectations/` verify architecture rules at build time
2. **PluresDB constraints** — stored in agent's own database, checked before every action
3. **Cerebellum preprocessing** — recalled as context before every LLM call

```rust
// Example praxis constraint
Constraint::new("arch-no-external-services")
    .when(|action| action.involves_http_call())
    .unless(|action| action.target_is_llm_api() || action.target_is_user_requested())
    .reject("Core capabilities must use native crates, not HTTP services")
```

## Superiority Over OpenClaw

| Dimension | OpenClaw | pares-agens |
|---|---|---|
| **Runtime** | Node.js (500MB, 3-5s cold start) | Rust (50MB, <500ms) |
| **Memory** | Markdown files + plugin | PluresDB embedded + HNSW vector search |
| **State sync** | Manual SSH/Tailscale | Hyperswarm P2P automatic |
| **Logic engine** | None (prompts only) | Praxis (facts, rules, constraints, decisions) |
| **Audit** | None | Chronos state chronicle + crates/audit |
| **Privacy** | Trust-based | crates/privacy PII filter + crates/arca encrypted vault |
| **Tool execution** | Plugin sandbox (Node.js) | Native procedures (Rust, zero overhead) |
| **Desktop automation** | Browser relay (fragile) | pares-manus (native screen capture, GUI automation) |
| **Consciousness** | Single-threaded loop | 3-tier (conscious/subconscious/cerebellum) |
| **Plugins** | JS plugins that break on update | PluresDB procedures (data, not code) |
| **CI/CD** | External GitHub Actions only | crates/faber (agent-first CI runner) |
| **Research** | Manual | crates/autoresearch (autonomous experiment loop) |
| **Training** | None | crates/trainer (fine-tuning) |
| **Inference** | Cloud API only | crates/inference (BitNet local) + cloud fallback |

## Implementation Order

Phase 1 (NOW): Core agent loop with PluresDB + Cerebellum + Praxis + tools ← DONE
Phase 2 (THIS WEEK): Chronos integration, real embeddings, multi-channel
Phase 3 (NEXT WEEK): pares-manus, plures-vault production, P2P sync
Phase 4 (MONTH 1): pares-radix plugin loading, marketplace, training pipeline

## Verification

This ADR is considered implemented when:
- [ ] `cargo test --workspace` passes with all integrations
- [ ] Praxis expectations verify architecture rules
- [ ] No HTTP calls for core capabilities (memory, procedures, embeddings)
- [ ] Chronos records every agent action
- [ ] plures-vault stores all secrets (no env vars in production)
- [ ] PluresDB is the ONLY persistence layer
- [ ] Cerebellum routes every event through 3-consciousness

## Amendment: PluresDB IS the Nervous System (2026-04-15)

PluresDB is not just storage. On every `put()`:
1. **CRDT merge** — conflict-free replication
2. **Auto-embed** — fastembed BAAI/bge-small-en-v1.5 (native ONNX, zero config)
3. **HNSW index update** — vector search immediately available
4. **P2P sync** — Hyperswarm replication to peers (automatic)
5. **Procedure triggers** — reactive data-driven execution
6. **State diff** — write metadata = Chronos audit trail

### What pares-agens MUST NOT reinvent:
- **Embeddings** — DELETE MockEmbedder, OllamaEmbedder. Use `PluresDB::FastEmbedder`
- **Sync** — Use PluresDB Hyperswarm, not custom sync layer
- **Procedures** — Use `pluresdb-procedures` DSL, not manual `ProcedureRegistry`
- **Audit** — PluresDB write metadata IS chronos. Query it, don't rebuild it.

### Correct PluresDbStore initialization:
```rust
let embedder = FastEmbedder::new("BAAI/bge-small-en-v1.5")?;
let store = Arc::new(
    CrdtStore::open(path)?
        .with_embedder(Arc::new(embedder))
);
CrdtStore::spawn_embedding_worker(Arc::clone(&store));
// Every put() now auto-embeds. Vector search works. P2P syncs.
```

### Praxis → PluresDB mapping:
| Praxis Concept | PluresDB Implementation |
|---|---|
| Facts | PluresDB nodes (auto-embedded, searchable) |
| Rules | PluresDB procedures (reactive, data-driven) |
| Constraints | Pre-write hooks on CrdtStore |
| Events | Write events from CrdtStore |
| Decision Ledger | Nodes with approval gate metadata |
| Chronos audit | Write metadata (actor, timestamp, causal links) |

### The only external call: LLM API
Everything else — memory, embeddings, search, procedures, sync, audit — is PluresDB native.
