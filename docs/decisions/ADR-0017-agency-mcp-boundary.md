# ADR-0017: Pares Agens ("Agency") ↔ PluresLM MCP Integration Boundary

**Status:** PROPOSED (design-only, no code)

**Date:** 2026-07-23

**Deciders:** Chief Architect, PluresLM maintainers

**Epic:** pluresLM:agency-integration (P2)

## Context and Problem Statement

"Agency" refers to `plures/pares-agens` — the reactive AI agent runtime where
PluresDB procedures define behavior and PluresLM provides memory. Pares Agens
already embeds a native `mcp-client` crate (`crates/mcp-client`) capable of
speaking MCP over stdio or HTTP, and a native `crates/core` with its own
`auth`, `delegation`, `cerebellum`, and `channel_contract` modules.

Separately, `pluresLM-mcp` (Node/TypeScript) is a standalone MCP server that
exposes `pluresLM_store` / `pluresLM_search` / `pluresLM_forget` /
`pluresLM_index` / `pluresLM_status` / `pluresLM_profile` tools backed by its
own PluresDB-topic-scoped store, intended primarily for OpenClaw and other
MCP-speaking hosts.

ADR-0014 (`full-plures-stack.md`) explicitly states pares-agens **MUST NOT**
use "MCP HTTP for memory" as a core capability — memory must be a native
PluresDB embed, not a network hop. This creates a direct tension with any
proposal to have Agency call `pluresLM-mcp` as its memory provider.

`PLURESDB-SERVICE-BOUNDARIES.md` additionally establishes the org-wide rule:
if a PluresDB store is shared by more than one client/runtime, there must be
exactly one store-owning manager process; other processes are thin clients,
never opening the live store directly.

This ADR resolves the boundary: **what, if anything, should Agency call
through `pluresLM-mcp`, versus what must stay a native embed** — and defines
the adapter contract, auth/privacy model, capability discovery, and test
harness for whichever integration surface is approved, without writing code.

## Decision Drivers

- Respect ADR-0014 (no HTTP/MCP hop for Agency's own core memory read/write
  path — recall/capture must stay native PluresDB embed for latency and
  offline-first guarantees).
- Respect PLURESDB-SERVICE-BOUNDARIES.md (single store owner; adapters are
  thin clients; no dual-writer/dual-lock scenarios).
- Preserve pluresLM-mcp's existing role serving **other** MCP hosts
  (OpenClaw, external tools, non-Rust clients) without regressing it.
- Avoid data fork: Agency's local PluresDB store and pluresLM-mcp's store
  must not silently diverge if both touch the same logical memory topic.
- Keep the integration testable without a channel adapter (Telegram, etc.).

## Considered Options

### Option 1: Agency calls pluresLM-mcp over MCP for all memory ops

Agency's `mcp-client` crate treats `pluresLM-mcp` as a normal MCP tool
server; `on_message` procedures call `pluresLM_search`/`pluresLM_store` tools
remotely for every recall/capture.

**Pros:**
- Reuses existing MCP client code path already used for other tools.
- Single implementation of memory semantics (TS side) shared across hosts.

**Cons:**
- Directly violates ADR-0014 ("MUST NOT use MCP HTTP for memory").
- Adds network/process hop latency to every message turn.
- Two runtimes (Rust core store + Node pluresLM-mcp store) risk becoming
  two lock owners over conceptually the same data unless topic-scoped
  carefully.
- Breaks offline-first guarantee if pluresLM-mcp is unreachable.

### Option 2: Full native embed, zero pluresLM-mcp integration

Agency only ever uses its own embedded PluresDB store
(`FastEmbedder` + `CrdtStore`) for memory. `pluresLM-mcp` remains solely an
OpenClaw-facing MCP server with no relationship to Agency.

**Pros:**
- Fully compliant with ADR-0014 and service-boundaries doc.
- No cross-runtime coupling; each system independently testable.
- No latency/availability coupling.

**Cons:**
- No shared/portable memory between an OpenClaw user and their Agency
  instance unless a separate sync mechanism exists.
- Duplicates "memory service" concerns (indexing, embeddings, forget) in
  two codebases with no reuse.
- Doesn't answer the actual epic ask (an integration seam is expected to
  exist somewhere).

### Option 3 (Recommended): Native embed for core memory + narrow, optional MCP adapter for cross-host discovery/import-export only

Agency's `on_message`/`on_timer` reactive path uses **only** the native
embedded PluresDB store for recall/capture (per ADR-0014, unchanged).
`pluresLM-mcp` remains the store-owning manager for **its own** topic(s) and
continues serving OpenClaw and other non-Rust MCP hosts (per
service-boundaries doc: pluresLM-mcp already *is* the "one store-owning
manager" for that topic).

The **integration seam** is a narrow, explicitly-invoked (not per-turn)
capability:

- Agency's `crates/mcp-client` may connect to `pluresLM-mcp` as an ordinary
  MCP tool server, exposed to the model only as **optional, user-invoked
  tools** (e.g., "search my OpenClaw memories", "import notes from
  OpenClaw"), never as the implicit recall/capture path.
- Any data pulled from `pluresLM-mcp` into Agency crosses through the same
  ingestion path as any other MCP tool result: it becomes conversation
  context for that turn and, if captured, is captured into **Agency's own
  native store** via `pluresLM.capture` — not written back into
  pluresLM-mcp's store. Agency never opens pluresLM-mcp's live PluresDB
  file directly; it only calls its MCP tool interface (already the
  service-boundary rule: adapters go through the manager's API, not around
  it).
- No dual-writer: `pluresLM-mcp`'s store is written to only by
  `pluresLM-mcp` and its existing OpenClaw-side clients. Agency is a
  **read-mostly, tool-invoked consumer**, not a peer sync participant.
- If bidirectional sync is ever desired, that is a **separate, future ADR**
  (e.g., Hyperswarm P2P topic-sharing), not part of this integration.

**Pros:**
- Fully compliant with both ADR-0014 (no MCP hop in the core recall/capture
  loop) and service-boundaries (pluresLM-mcp remains sole owner of its
  store; Agency never opens the file, never becomes a second lock holder).
- Delivers the actual cross-system value (a user's OpenClaw history is
  reachable from Agency) without coupling availability or latency of the
  primary agent loop to a Node process.
- Reuses `crates/mcp-client`'s existing stdio/HTTP transport and
  `openai_tools()` conversion — no new transport code needed, only a
  registration/config decision (still design-only here).
- Testable independently: pluresLM-mcp's tool contract can be exercised via
  its existing MCP transport with no channel adapter; Agency's native
  memory path is unaffected and independently testable.

**Cons:**
- Two distinct "memory" concepts exist in a user's mental model (Agency's
  own memory vs. imported OpenClaw memory) — must be labeled clearly at the
  tool level (see Capability Discovery below).
- Requires ongoing discipline that no future PR "helpfully" wires
  pluresLM_search into the default recall path.

## Decision Outcome

**Chosen option:** Option 3 — native embed for core memory; pluresLM-mcp
integration is a narrow, optional, tool-invoked adapter, never the implicit
recall/capture path.

**Rationale:** This is the only option that satisfies both existing binding
architecture decisions (ADR-0014, service-boundaries doc) simultaneously
while still providing a real integration seam between Agency and the
existing OpenClaw/pluresLM-mcp ecosystem.

### Adapter Contract (design only — no implementation in this change)

Define a new, explicitly optional Agency-side capability,
`agency.external_memory_bridge`, with this contract:

```text
Capability: external_memory_bridge
Transport:  MCP (stdio or HTTP/SSE), via existing crates/mcp-client
Direction:  read-mostly (search, get); write path is capture-into-own-store
            only, never write-through to the remote server
Invocation: explicit tool call only (never injected into default recall)
Tools consumed (from pluresLM-mcp, unmodified):
  - pluresLM_search(query, limit?, minScore?) -> memory[]
  - pluresLM_status() -> { count, syncStatus, topic } (health/inspection only)
Tools NOT consumed by Agency:
  - pluresLM_store        (no write-through; Agency captures locally instead)
  - pluresLM_forget        (deletion stays under pluresLM-mcp's own control)
  - pluresLM_index         (codebase indexing stays an OpenClaw-side concern)
  - pluresLM_profile       (out of scope for v1; revisit if a real need
                             emerges)
Config surface (design-only, not implemented here):
  - `[external_memory_bridge]` table in Agency config/state (PluresDB state
    row, not a JSON file — per ADR-0014's "no JSON config files" rule):
      enabled: bool (default false)
      transport: "stdio" | "sse"
      endpoint/command: string
      topic_label: string (human label shown to the user, e.g. "OpenClaw
                    memories")
Failure mode: bridge unavailable => tool call fails gracefully, does not
              block or degrade the native recall/capture path.
```

### Auth / Privacy

- **Auth:** the bridge is configured per-user via Agency's `crates/arca`
  (plures-vault) for any credentials (e.g., `PLURES_DB_TOPIC` /
  `PLURES_DB_SECRET` used by pluresLM-mcp) — never plain env vars in
  production, consistent with ADR-0014's "MUST NOT: JSON config files /
  env-var secrets in production" stance. The topic key functions as a
  shared-secret capability token; Agency stores it only in the vault, never
  logs it, never surfaces it in prompts/tool descriptions.
- **Data flow boundary:** results returned by `pluresLM_search` are treated
  as untrusted external content by the model-router/prompt-builder layer
  (same trust class as any other tool result / MCP payload) — they must be
  wrapped/labeled as external content, not treated as first-party Agency
  memory, to prevent prompt-injection-via-memory-content.
  If Praxis PII/privacy filtering is desired, `crates/privacy` should
  post-process bridge results before they enter the prompt — future work
  item, not in this ADR's scope, but a required gate before enabling the
  bridge by default for any user.
- **Consent/visibility:** because this crosses a system boundary (Agency
  reading a store that may contain content from OpenClaw sessions the user
  ran outside of Agency), the bridge must be **opt-in per user** and
  visibly labeled in any UI/tool listing as "external / OpenClaw memory,"
  distinct from Agency's own PluresLM memories.
- **No write-through**, as stated in the adapter contract, removes the
  largest privacy/consistency risk (Agency accidentally polluting a user's
  OpenClaw memory store with Agency-only context).

### Capability Discovery

- The bridge's tools are registered with `crates/mcp-client`'s existing
  `openai_tools()` conversion, but **gated by the `enabled` config flag** —
  when disabled (default), the tools are not advertised to the model at
  all, so no accidental invocation and no wasted context-window tokens
  describing an unavailable capability.
- When enabled, the tool description surfaced to the model must explicitly
  state the tool searches **"external OpenClaw memory (read-only,
  separate from your own memory)"** — this labeling is part of the
  contract, not cosmetic, since it affects both privacy transparency and
  correct model behavior (agent must not assume bridge content is
  first-party).
- Discovery/health should be queryable via `pluresLM_status()` so Agency
  (or a human) can confirm the bridge is live and see topic/sync state
  before relying on it — this maps directly onto the
  service-boundaries doc's requirement that "every service must have a
  CLI, HTTP, MCP, or IPC interface usable by tests without a channel
  adapter," which pluresLM-mcp already satisfies.

### Test Harness (design only)

Per PLURESDB-SERVICE-BOUNDARIES.md's verification gate, testing must exercise
the service API directly, not via a channel adapter:

1. **pluresLM-mcp side (already testable today, no new work needed):**
   start `pluresLM-mcp` in stdio mode with a disposable
   `PLURES_DB_TOPIC`, connect a raw MCP client, call `pluresLM_store` then
   `pluresLM_search`/`pluresLM_status`, assert round-trip. This is the
   existing contract Agency's bridge will depend on — no changes required
   to pluresLM-mcp itself for Option 3.
2. **Agency bridge contract test (future PR, not this ADR):** a
   `crates/mcp-client` integration test using its existing
   `tests/mock_server.rs` pattern, but pointed at a real (disposable)
   `pluresLM-mcp` stdio process instead of a mock, verifying:
   - bridge disabled ⇒ `pluresLM_search` tool absent from
     `openai_tools()` output.
   - bridge enabled ⇒ tool present, correctly labeled, and a successful
     call returns memory results tagged as external content.
   - `pluresLM_store` / `pluresLM_forget` / `pluresLM_index` /
     `pluresLM_profile` are never present in Agency's tool list regardless
     of config (enforces the "tools NOT consumed" list above).
   - killing the pluresLM-mcp process mid-session does not affect Agency's
     native recall/capture path (native path test runs with bridge process
     killed and must still pass).
3. **Negative/regression test (future PR):** assert that no code path in
   `crates/core/handlers/{auto_capture,auto_recall}.rs` references the
   bridge or any MCP transport — keeps ADR-0014 compliance enforceable in
   CI (e.g., a grep-based Praxis expectation, matching the existing pattern
   in `crates/praxis/expectations/`).

None of the above tests are implemented as part of this ADR; they are the
acceptance criteria for whichever follow-up PR implements Option 3.

### Consequences

**Positive:**
- Resolves the ADR-0014 / integration-request tension explicitly instead of
  leaving it ambiguous for whoever implements the epic.
- Keeps pluresLM-mcp's existing single-store-owner guarantee intact; Agency
  never becomes a second writer or lock holder.
- Provides a concrete, minimal adapter contract implementers can build
  against without re-litigating architecture.

**Negative:**
- Users get two separate "memory" surfaces to reason about (Agency-native
  vs. bridged-external) until/unless a future sync ADR unifies them.
- No memory captured *in* Agency ever flows back to a user's OpenClaw
  pluresLM-mcp store — that is a deliberate scope cut, not an oversight,
  and should be called out to stakeholders expecting bidirectional sync.

**Neutral:**
- Bidirectional sync, if wanted later, is pushed to a follow-up ADR
  building on Hyperswarm P2P (already a planned pares-agens dependency per
  ADR-0014), not this one.

### Implementation Notes (for the future implementing PR — not this change)

- No code, stubs, or config files are added by this ADR.
- Implementing PR must add: config schema entry (as a PluresDB state row,
  not JSON), `crates/mcp-client` registration gated on the `enabled` flag,
  tool-description labeling, and the three test-harness items above.
- Implementing PR must also add a Praxis expectation (or equivalent CI
  check) asserting `handlers/auto_recall.rs` and `handlers/auto_capture.rs`
  never reference the bridge, to keep this boundary enforced mechanically
  rather than by convention alone.

## Links

- [ADR-0014: Full Plures Stack Integration](./ADR-0014-full-plures-stack.md)
- [ADR-0015: Distributed Procedure Evolution](./ADR-0015-distributed-procedure-evolution.md)
- [PluresDB Service Boundaries](../../../development-guide/design/PLURESDB-SERVICE-BOUNDARIES.md)
- [PARES-AGENS Architecture](../../../development-guide/design/PARES-AGENS.md)
- pluresLM-mcp README (tool contract: `pluresLM_store`, `pluresLM_search`,
  `pluresLM_forget`, `pluresLM_index`, `pluresLM_status`, `pluresLM_profile`)
- `crates/mcp-client` (`client.rs`, `openai.rs`, `transport/{stdio,http}.rs`)
  — existing transport this ADR reuses without modification
