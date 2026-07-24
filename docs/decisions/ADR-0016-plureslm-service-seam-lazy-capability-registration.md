# ADR-0016: PluresLM Memory Seam — Lazy Capability Registration via Service Boundary

**Status:** PROPOSED (design/seam-verification only — no implementation until reviewed)
**Date:** 2026-07-23
**Author:** Subagent (design task, epic `pluresLM:pares-agens-integration`, requested by kbristol via main-agent orchestrator)
**Depends on:** [PluresDB Service Boundaries](https://github.com/plures/development-guide/blob/main/design/PLURESDB-SERVICE-BOUNDARIES.md), [PARES-AGENS Architecture](https://github.com/plures/development-guide/blob/main/design/PARES-AGENS.md), OBSERVABILITY-EVENT-CONTRACT (link TBD), [ADR-0014](./ADR-0014-full-plures-stack.md)
**Scope:** Design + seam verification ONLY. This ADR does not authorize any code change. It is the gate a follow-up implementation PR must pass review against.

## Context

### The seam under inspection

`pares-agens/crates/core/src/memory/store.rs` defines `PluresLm` and a
`MemoryStore` trait with two implementations today:

- `InMemoryStore` — test/embedded fallback, no persistence.
- A PluresDB-backed store that opens `pluresdb::CrdtStore` (via `SledStorage`)
  **directly inside the `core` crate**, plus `pluresdb_sync::Replicator` /
  `SeaKeyPair` wiring in the same file.

This is precisely the failure mode `PLURESDB-SERVICE-BOUNDARIES.md` documents
from the prior OpenClaw/PluresLM plugin incident: a host process (here,
`pares-agens core`, potentially spawned per-channel-adapter, per-CLI-invocation,
or per-cerebellum-runtime) opens the live PluresDB store and holds its native
exclusive file lock. `ADR-0014` already commits pares-agens to "PluresDB
embedded, not HTTP sidecar" for performance — that decision is not being
reversed here. What this ADR adds is the missing middle layer the boundary doc
requires: **exactly one store-owning service/manager**, with `core::memory`
becoming a *client* of it rather than a second/Nth store owner.

### Why this matters now, specifically for pares-agens

pares-agens has more concurrent-access surface than the OpenClaw case that
motivated the original boundary doc:

- `crates/tui`, `crates/cli`, `crates/tauri-app`, and channel adapters
  (`crates/channels`) are all separate binaries/processes that may each
  construct a `PluresLm` today.
- `crates/mcp-client` + the "PluresLM MCP server" (referenced in `PARES-AGENS.md`
  M4 milestone) is a second, external-facing access path into the same memory
  store — for tools like Docker MCP Toolkit clients.
- Cerebellum's 3-consciousness routing (`crates/core/src/cerebellum/*`) implies
  conscious/subconscious paths may run as separate tasks or processes recalling
  memory concurrently.
- `crates/core/src/delegation/*` (agent broker/aggregator) can spawn sub-agents
  that each want `recall`/`capture` — another N clients against one store.

If each of these opens `CrdtStore::open(path)` independently, the second opener
fails or silently degrades (the exact bug class the boundary doc names:
"catching errors and returning empty search results").

### Constraint inventory already governing this seam

1. **PLURESDB-SERVICE-BOUNDARIES.md — Rule:** "If a PluresDB store is shared by
   more than one client or runtime, there must be exactly one store-owning
   service/manager process... Host plugins and apps are clients of that
   manager; they do not each open the live store directly."
2. **ADR-0014 amendment ("PluresDB IS the Nervous System"):** every `put()`
   already does CRDT merge, auto-embed, HNSW update, P2P sync, procedure
   triggers, Chronos-equivalent write metadata — this is store-internal
   behavior the service must not reimplement or bypass.
3. **ADR-0014 "MUST NOT USE":** no HTTP sidecar for core capabilities. The
   service in this ADR is **in-process-or-local-IPC**, not a network hop to an
   external product — see "Service transport" below for how this stays
   consistent with ADR-0014 while still being a single-owner service.
4. **OBSERVABILITY-EVENT-CONTRACT.md:** any long-running or asynchronous
   operation this seam introduces (index rebuild, service reconnect, gate
   consolidation) MUST emit `plures.proc.event.v1`-shaped events on the
   `proc.event:*` channel — reuse, don't invent a parallel logging shape.
5. **C-TEST-002 (implied by both docs above):** verification must never depend
   on a channel adapter (Telegram, Tauri UI, etc.). Local QA must exercise the
   service API directly.

## Decision

Introduce a **PluresLM Memory Service** as the single store-owning process for
pares-agens, and refactor `core::memory::PluresLm` from "a struct that opens
the DB" into "a struct that is a thin client of the service." Capability
registration into the rest of pares-agens (cerebellum recall hook, MCP tool
surface, delegation broker context assembly) becomes **lazy**: capabilities are
advertised/registered only once the client has a live, health-checked
connection to the service — never speculatively bound to a not-yet-verified
live store handle.

### Layering (mapped onto `PLURESDB-SERVICE-BOUNDARIES.md`'s 3-tier split)

```
┌──────────────────────────────────────────────────────────────┐
│ Adapters (thin clients — never open the live store)          │
│  - core::memory::PluresLm         (in-process client facade) │
│  - cerebellum recall/capture hook (calls PluresLm client)     │
│  - crates/mcp-client / PluresLM MCP server (external clients) │
│  - crates/tui, crates/cli, crates/tauri-app                   │
│  - delegation broker/aggregator sub-agent contexts            │
└───────────────────────┬────────────────────────────────────────┘
                        │ service calls: recall / capture / status /
                        │ health / index / consolidate / migrate
┌───────────────────────▼────────────────────────────────────────┐
│ PluresLM Memory Service (ONE process per store; owns the lock) │
│  - exposes: recall, get, capture, capture_fact, ingest,        │
│    scan_all, status, health, index/sync, consolidate, migrate  │
│  - lazy capability registry (see below)                        │
│  - emits proc.event:* on register/degrade/recover              │
└───────────────────────┬────────────────────────────────────────┘
                        │ owns exclusive native lock
┌───────────────────────▼────────────────────────────────────────┐
│ PluresDB live store (CrdtStore/SledStorage, embeddings, sync)  │
└──────────────────────────────────────────────────────────────┘
```

### Service transport (staying ADR-0014-compliant)

ADR-0014 forbids HTTP sidecars for core capabilities, but does not forbid a
single-owner service — it forbids *reimplementing* PluresDB behavior outside
PluresDB and forbids *network hops for embeddings/procedures/sync specifically*.
This ADR satisfies both:

- **Default/primary transport:** local IPC (Tokio UDS on Unix / named pipe on
  Windows, matching the existing `pluresdb_sync::TransportConfig` /
  `TransportMode` abstractions already in the dependency graph) or an
  in-process `Arc<PluresLmService>` singleton when the caller runs in the same
  binary as the service (e.g., a single-binary desktop build). No network
  socket, no remote host, no additional latency class beyond a local syscall.
- **External MCP surface:** the "PluresLM MCP server" milestone (M4 in
  `PARES-AGENS.md`) is the same service exposed over MCP stdio/HTTP transport
  for **out-of-process tool clients only** (Docker MCP Toolkit, external
  editors) — this is the adapter layer, not a second store owner, and it talks
  to the same service, never to `CrdtStore` directly.
- Net effect: exactly one process opens `CrdtStore`. Every other process —
  same-binary or cross-binary — is a client over IPC or in-process handle.

### Lazy capability registration (the core new mechanism)

**Problem this solves:** the OpenClaw incident happened because tool/capability
registration was **eager and unconditional** — the adapter announced
`memory_search`/`memory_get` as available before verifying it could actually
reach a healthy, lock-holding store. A restricted/embedded runtime then saw a
registered-but-nonfunctional capability.

**Rule:** No component may register a memory capability (cerebellum recall
hook, MCP tool listing, delegation broker's `allowed_tools` inclusion of
memory-backed tools) until it has:

1. Established a connection to the PluresLM Memory Service (IPC handle or
   in-process `Arc`), and
2. Received one successful `health()` response from that service confirming
   the service itself holds a live, unlocked store connection, and
3. Recorded the registration as an event on `proc.event:*`
   (`kind: "started"` → `"completed"` once step 2 succeeds), consistent with
   `OBSERVABILITY-EVENT-CONTRACT.md`.

Concretely, in Rust terms (interface sketch only — **not implementation**):

```rust
// core::memory — the client facade replacing direct CrdtStore ownership
pub struct PluresLm {
    client: PluresLmServiceClient,   // IPC or in-process handle to the ONE service
    registration: CapabilityRegistration,
}

pub enum CapabilityRegistration {
    Unregistered,                 // no connection attempted yet
    Pending { attempt_started: Instant },
    Registered { since: Instant },  // health-checked; capability IS advertised
    Degraded { since: Instant, last_error: String }, // was registered, service unreachable now
}
```

- `Unregistered → Pending`: on first use (lazy — not at process startup;
  a CLI one-shot invocation that never touches memory never pays the
  connection cost).
- `Pending → Registered`: only after `health()` succeeds. Only in `Registered`
  state does the capability appear in cerebellum's tool set, the MCP
  `list_tools()` response, or a delegation sub-agent's `allowed_tools`.
- `Registered → Degraded`: any service call failure (timeout, IPC broken pipe,
  service-reported lock loss) demotes immediately. A degraded capability is
  **removed from advertised capability lists**, not silently made to return
  empty/successful-looking results (explicitly what the boundary doc forbids).
- `Degraded → Pending`: automatic retry with backoff; re-enters `Registered`
  only after a fresh successful `health()`.

This makes "capability appears in a tool list" and "capability actually works
against a live store" the same statement by construction — closing the exact
gap that caused the OpenClaw `active-memory` embedded-agent bug (registered
tool, unreachable store).

## Failure / recovery semantics

| Scenario | Client-visible behavior | Service-visible behavior |
|---|---|---|
| Service not yet started, client requests recall | Capability is `Unregistered`/`Pending`; caller gets an explicit `ServiceUnavailable` error, never an empty-success recall. Cerebellum treats this as "memory unavailable this turn," not "no memories found." | N/A — service process not running |
| Service holds lock, second process also tries `CrdtStore::open` directly (should be structurally impossible post-refactor, but must fail loud if it happens) | N/A | Second open attempt errors immediately (native lock contention); this is a **regression signal**, not a supported path — CI/lint should catch any new direct `pluresdb::CrdtStore::open` outside the service crate |
| Service crashes mid-session | All connected clients demote to `Degraded` on next failed call (not proactively — no polling loop, consistent with "don't poll" guidance elsewhere in this workspace) | Service process exits; lock is released cleanly (or, if unclean, PluresDB's own lock-recovery — verified in Verification Gate step 5 below) |
| Service restarts | Clients in `Degraded` retry with backoff (jittered, capped) and re-register on first successful `health()` | Service reopens store; must detect and clear any stale lock file from an unclean prior exit before accepting connections |
| Network/IPC partition (multi-device P2P sync case) | Local service keeps serving from local store; sync layer (`pluresdb_sync::Replicator`) reconciles when reachable — **not** a capability-registration event, purely a sync-layer concern already owned by ADR-0014's P2P sync layer | No capability degradation; sync lag is a separate signal (already covered by ADR-0014 Hyperswarm layer), must not be conflated with service-reachability degradation |
| Concurrent capture during index/consolidate | Service serializes internally (single lock owner); clients see normal latency, not errors | Service must not block `recall` for the duration of a `consolidate`/reindex — if it must, it MUST emit `proc.event` `progress` so callers can distinguish "slow" from "stuck" |

All degrade/recover transitions MUST emit `proc.event:*` records
(`kind: started|completed|failed|blocked|heartbeat` per the existing schema) so
the existing dev-lifecycle relay/tail tooling works against this seam for free,
per `OBSERVABILITY-EVENT-CONTRACT.md`. No new event shape is introduced.

## Channel-independent API

Per `PLURESDB-SERVICE-BOUNDARIES.md`'s "local automation interface" requirement,
the service API is defined once and used identically by every adapter:

```
recall(query, limit, exclude?) -> Vec<MemoryEntry>
get(id) -> Option<MemoryEntry>
capture(exchange) -> Vec<String>            // returns captured entry ids
capture_fact(fact, tags) -> Option<String>
ingest_documents_path(path) -> usize
scan_all() -> Vec<MemoryEntry>
status() -> ServiceStatus                    // lock state, entry count, index freshness
health() -> HealthReport                      // used for capability registration gating
consolidate() -> ConsolidationReport
migrate(from_version, to_version) -> MigrationReport
```

This is the same method set `core::memory::PluresLm` exposes today — the
refactor changes *who implements it* (service, not each adapter process), not
its shape. Telegram, Tauri, CLI, TUI, and MCP adapters all call this same
surface; none of them may special-case behavior per channel at the memory
layer. Channel-specific formatting (`ChannelContract` in
`channel_contract.rs`) is explicitly a separate, downstream concern and is
unaffected by this ADR.

## Local QA (no channel adapter required)

Directly satisfies `PLURESDB-SERVICE-BOUNDARIES.md`'s verification gate,
scoped to this seam:

1. Start the PluresLM Memory Service against a real (test) PluresDB store.
2. Run ≥2 independent clients concurrently (e.g., two `core::memory::PluresLm`
   client instances in separate tokio tasks/processes, simulating
   cerebellum + a CLI invocation) issuing `recall`/`capture` concurrently.
   Assert both succeed with no lock errors.
3. Exercise `recall`, `get`, `capture`, `ingest_documents_path`/sync, `status`,
   `health` through the client API only — no Telegram/Tauri/Discord involved.
4. Simulate an embedded/isolated runtime (a client constructed with a
   restricted allowlist, analogous to OpenClaw's `active-memory` lane) and
   verify it can call `recall` successfully through the same registered
   capability path — this is the regression test for the bug that motivated
   `PLURESDB-SERVICE-BOUNDARIES.md`.
5. Kill the service process mid-session with a client connected; verify the
   client demotes to `Degraded` (not a silent empty success), restart the
   service, verify the client recovers to `Registered` and subsequent calls
   succeed with no stale lock and no data loss.
6. Attempt a second, out-of-band `CrdtStore::open` against the same store path
   while the service is running; assert it fails fast (proves single-owner
   invariant holds) rather than silently succeeding.
7. Run this as an actual binary/integration test (per boundary doc's "run the
   binary/service, not only unit tests"), not solely `cargo test --lib`.

## Consequences

### Positive
- Closes the exact seam-class bug documented in
  `PLURESDB-SERVICE-BOUNDARIES.md` before pares-agens reaches multi-adapter
  production use (it currently has fewer adapters live than OpenClaw did when
  the bug was found, so this is a preventive fix, not a reactive one).
- Capability lists (cerebellum tools, MCP `list_tools`, delegation
  `allowed_tools`) become trustworthy — "listed" implies "verified reachable,"
  removing a class of confusing agent-visible failures.
- Reuses the existing `proc.event:*` observability contract instead of
  inventing new telemetry, keeping the dev-lifecycle tooling ecosystem-agnostic.
- Stays inside ADR-0014's "no HTTP sidecar for core capabilities" constraint —
  IPC/in-process, not a network product.

### Negative / costs
- Introduces a new long-lived process (or in-process singleton with lifecycle
  management) that must be started, supervised, and health-checked —
  operational surface that didn't exist when `core` opened the DB directly.
- `Unregistered`/`Pending` states mean the very first memory call in a fresh
  process pays a connection-establishment latency cost (mitigated by lazy
  registration — paid once, on first use, not at every process startup).
- Requires a CI/lint rule (new, out of scope for this ADR to implement) that
  flags any new direct `pluresdb::CrdtStore::open` call outside the service
  crate, to keep the single-owner invariant from silently regressing.

## Open questions for review (must be resolved before implementation PR)

1. **Process model:** is the service a separate OS process per pares-agens
   installation, or an in-process singleton guarded by a `OnceCell<Arc<..>>`
   when all adapters run in one binary (e.g., the Tauri desktop build)? This
   ADR treats both as valid "single owner" shapes but the implementation PR
   must pick one (or both, selected by build target) explicitly.
2. **Backoff parameters** for `Degraded → Pending` retry (initial delay, cap,
   jitter) — not specified here; implementation PR must set concrete values
   and justify them against expected service restart time.
3. **Where does the service process get supervised from?** (`crates/agenda`
   native scheduler per ADR-0014? OS service manager? Tauri app lifecycle?)
   Out of scope for this design; must be resolved in implementation PR.
4. Does `crates/sync`'s Hyperswarm P2P layer sit *inside* the service (single
   owner also owns sync) or as a separate process the service talks to? This
   ADR assumes **inside** (consistent with ADR-0014's "PluresDB is the nervous
   system" — sync is store-native behavior) but this must be confirmed against
   `crates/sync`'s actual coupling to `CrdtStore` before implementation.

## Non-goals (for this ADR)

- Does not change `ADR-0014`'s decision to embed PluresDB rather than run it
  as an external product — this is a refactor of *ownership location*, not a
  reversal of "no HTTP sidecar."
- Does not specify the wire format for the external PluresLM MCP server
  (M4 milestone) beyond "it must call the service, never `CrdtStore`
  directly" — MCP protocol details are out of scope here.
- Does not implement the CI/lint guard against direct `CrdtStore::open`
  outside the service crate — flagged as a required follow-up, not delivered
  by this ADR.
- Does not touch `channel_contract.rs` / per-channel rendering — orthogonal
  concern.

## Verification gate for this ADR itself

This document is not "done" until:
- [ ] Reviewed against `PLURESDB-SERVICE-BOUNDARIES.md`'s "required design
      checks" list (all six bullets answered above — confirm each is
      unambiguous before approval).
- [ ] Open questions section has assigned owners/answers or is explicitly
      deferred with rationale.
- [ ] No implementation PR opened against `core::memory::store.rs` until this
      ADR status changes from PROPOSED to ACCEPTED.
