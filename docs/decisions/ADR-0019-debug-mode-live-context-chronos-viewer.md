# ADR-0019: Debug-Mode Live-Context Window + Richer Chronos Flow Visualization

**Status:** PROPOSED (design only — no implementation until reviewed, per C-DEV-001 .px-first)
**Date:** 2026-07-24
**Deciders:** kbristol (feature request, 2026-07-24)
**Related:** ADR-0015 (`pares-agens` is a plugin, not a host — architecture baseline this design
must respect), `plures/chronos` (state-chronicle engine), `design-dojo`'s `ChronicleViewer.svelte`,
`plureslm-openclaw` PR #16 (memory_get exact-lookup fix, pending)

## 1. Summary

kbristol requested five related capabilities:

1. A GUI/TUI **debug-mode window** showing live agent context and how it changes in real time.
2. A **richer graphical Chronos flow-of-operations visualization**, building on the existing
   `ChronicleViewer.svelte` / ChronosDevTools work — not a rewrite.
3. A **pause button** to halt on context change for live inspection.
4. **Richer per-operation Chronos event detail** — today's logging is too shallow (event type
   only; no inputs/outputs).
5. **Drill-down UX**: clicking an operation card shows full detail, including autorecall search
   results and the *actual* recalled content (depends on the `memory_get` exact-lookup fix,
   plureslm-openclaw PR #16).

This ADR is design-only. No implementation code is produced in this pass.

## 2. Prior-art audit (avoid duplicating existing work)

**Finding: there is no separate "ChronosDevTools" component.** The existing, real building block
is `design-dojo/src/lib/app/ChronicleViewer.svelte` (~640 lines, shipped, Storybook-covered,
commit `23be414`, later hardened for a11y/ESLint). It already provides:

- Timeline of chronicle nodes (state changes) with path/diff/timestamp
- Causal-chain highlighting (click a node → see its cause, walk backward)
- Semantic search bar (`onsearch` callback → parent calls PluresDB vector search)
- Search-result and causal-trace (`TraceResult`) rendering
- Path-prefix / time-range filtering, expand/collapse diff detail
- Keyboard navigation, `prefers-reduced-motion`, TUI-compatible via `useTui()`

**Underlying engine:** `plures/chronos` (repo, standalone) — `createChronos(db)` auto-subscribes to
PluresDB `.on()` diffs, no manual log calls; `withCause`/`currentCause` (AsyncLocalStorage) build
causal chains; query API is `trace()/range()/subgraph()/history()/stats()`. A persistent writer
(`createPersistentWriter`) durably stores nodes/edges in PluresDB. This was later partly absorbed
into `pluresdb-chronos` (`pluresdb/crates/pluresdb-chronos`, 775 LOC, 13 tests) as the state-
timeline crate consumed by `pluresdb-node`/WASM bindings (`chronos.rs`: `WasmChronosTimeline` —
`record/history/recent/timeline/set_level`).

**Node/edge shape today (the "too shallow" complaint, confirmed):** `ChronicleNode` = `{ id,
timestamp, path, diff: {before, after}, cause, context }`. There is no `operation` concept
distinct from a raw state diff — no inputs/outputs, no operation *kind* beyond the implicit path,
no linkage to what triggered the diff beyond a single `cause` id. This matches the schema-expansion
work item requested in this ADR.

**"ADR-0015" status:** in `design-dojo` there is no ADR-0015 (no `.praxis`/ADR directory exists in
that repo at all — governance for design-dojo docs lives in `docs/`, not ADR files). The
`pares-agens:ADR-0015` slot is already occupied (`docs/decisions/ADR-0015-autorecall-topic-shift-
dead-code.md`, unrelated: dead-code retirement in Cerebellum topic-shift detection, currently
PROPOSED). **This is therefore a NEW ADR (0019, next free slot in `pares-agens/docs/decisions/`),
not an ADR-0015 revision** — the task brief's premise that ADR-0015 covers this feature was
incorrect; there is no prior ADR for debug-mode/Chronos-viewer to revise.

**Conclusion — build ON, don't rebuild:** `ChronicleViewer.svelte` is the correct UI foundation for
requirement 2 (richer visualization) and requirement 5 (drill-down). It needs new props/slots for
per-operation detail and a drill-down panel — not a new component. The Chronos *event schema* (in
`pluresdb-chronos` / `plures/chronos`) needs the expansion in §4 to carry the richer data the UI
would display. No existing artifact provides requirements 1 (live-context window) or 3 (pause) —
those are genuinely new agens-side surfaces.

## 3. Repo routing (per repo-routing-validation.md decision tree)

| Concern | Repo | Why |
|---|---|---|
| Chronos event-schema expansion (operation kind, inputs/outputs, richer node shape) | `pluresdb` (`pluresdb-chronos` crate) + `plures/chronos` (TS engine) | Chronos is a PluresDB-adjacent state-chronicle capability; schema lives with the engine that emits it, per PLURES-FOUNDATION's "CRDT/storage/reactive-trigger feature → pluresdb" rule. |
| Debug-mode live-context window, pause semantics, agent-loop instrumentation | `pares-agens` (private plugin — this is agent-runtime behavior, IP-protected per the public/private boundary in PLURES-FOUNDATION) | Live context = the agent's active working context (recall results, prompt assembly, tool-call trace) — this is agent-only logic and must not land in open `pares-radix`. |
| ChronicleViewer UI enhancements (drill-down panel, operation-card rendering, richer diff view) | `design-dojo` | UI component library; consumed by any Praxis-based app. |
| Wiring the debug window into the radix host surface (GUI/TUI render modes) | `pares-agens` provides the data + ActionHandler; `pares-radix` (or agens's own Svelte surface loaded via modulus) renders it — per ADR-0015, agens does not become its own host binary. | Respects ADR-0015: agens is a plugin, not a host. |
| `memory_get` exact-lookup fix | `plureslm-openclaw` (already filed, PR #16, external dependency — not in scope here) | Separate repo, separate fix, already in flight. |

This is legitimately cross-repo (pluresdb + pares-agens + design-dojo). Per repo-routing-validation,
the primary owner is **`pares-agens`** (the feature is agent-debug UX; the other two repos each own
one clearly-scoped extension of existing capability, not new features of their own).

## 4. Design

### 4.1 What "live context" means operationally

"Live context" = the agent's currently-assembled working state for the in-flight turn:

- The **prompt-assembly context** (system prompt + injected personality/constraints + conversation
  history window) — sourced from `pares-agens`'s existing `prompt_builder`/`agent::build_system_prompt`
  path (already instrumented per PLURES-FOUNDATION's serve-spine notes).
- The **recall/autorecall state** — what PluresLM/PluresDB queries fired, what came back, and what
  was actually injected into context (ties directly to requirement 5 and the PR #16 dependency).
- The **tool-call trace** for the turn (which tools were invoked, with what args, what they
  returned) — this is exactly Chronos-node shaped once §4.3's schema lands.

**Event source powering the window:** the existing Chronos subscription mechanism
(`createChronos(db).on(...)` / `pluresdb-chronos`'s reactive record path), NOT a new logging
channel. The debug window is a **live consumer of the Chronos node stream**, scoped to the
current agent session/turn (filter by a `session_id`/`turn_id` field — see §4.3). This is a read-
only observer; it does not participate in the agent's control flow except via the pause hook
in §4.2. Concretely: `pares-agens` subscribes to `chronos.on('node', ...)` (or the PluresDB
reactive-query equivalent) filtered to its own emitted nodes, and streams them to whatever render
surface (GUI/TUI) is active via the existing render-mode/praxis-fact mechanism (`render.mode`),
not a bespoke transport.

### 4.2 Pause semantics

**Granularity: per-tool-call / per-Chronos-node boundary, not per-turn.** Rationale: pausing only
at turn granularity defeats the stated purpose ("halt on context change for live inspection") —
context changes happen *within* a turn (each recall, each tool call, each dataflow bridge hop
emits its own Chronos node). Pausing at turn boundaries would let an entire turn's context churn
happen invisibly between pauses.

- **Pause point:** immediately *after* a Chronos node is recorded (post-emit), *before* the
  agent's execution proceeds to the action that node represents' next dependent step. This means
  pause is implemented as a **hook on the Chronos emit path** (a subscriber that can request the
  runtime block), not a hook scattered through every call site — consistent with "Chronos logging
  is a consequence of praxis contracts, not manual calls" (ADR-0016, 2026-05-08 architecture).
- **What freezes:** the single in-flight agent turn's continuation (the async task/procedure step
  that would consume the node that was just recorded). Other concurrent turns/sessions are
  unaffected — pause is scoped by `session_id`/`turn_id`, never global.
- **How it's requested:** the debug window's pause button sets a praxis fact (e.g.
  `debug.pause_requested = true` scoped to the session), consistent with radix's "state is always
  a praxis fact" rule (PLURES-FOUNDATION svelte-Tauri template notes) — not an imperative RPC.
- **Resume:** clearing the fact (`debug.pause_requested = false`) or a per-pause step/continue
  action releases exactly one blocked continuation; a "run to completion" action clears the fact
  for the remainder of the turn.
- **Timeout/self-heal:** a pause has a configurable max-hold duration (default generous, e.g. 10
  minutes) after which it auto-resumes with a logged Chronos node (`operation: "debug_pause_
  timeout_autoresume"`) so a forgotten debug session can never permanently wedge a live agent
  turn — this is a Level-2 (testing/reliability) concern this design must not skip.

### 4.3 Chronos event-schema expansion

Current shape (both `plures/chronos`'s `ChronicleNode` and `pluresdb-chronos`'s Rust equivalent):

```
{ id, timestamp, path, diff: { before, after }, cause, context }
```

**Proposed expanded shape** (additive — existing fields unchanged, so existing consumers of
`history()/range()/trace()` keep working; this is a schema *extension*, not a breaking change):

```
{
  id, timestamp, path, diff: { before, after }, cause, context,   // unchanged
  operation: {
    kind: string,          // e.g. "tool_call", "recall_query", "prompt_assemble",
                            // "dataflow_step", "cerebellum_route", "state_diff" (fallback
                            // for today's raw-diff-only nodes, for backward compatibility)
    session_id: string,    // NEW — required for live-context scoping (§4.1) and pause (§4.2)
    turn_id: string,       // NEW — required for pause granularity
    inputs: unknown | null,   // NEW — e.g. tool-call args, recall query text/embedding params
    outputs: unknown | null,  // NEW — e.g. tool-call result, recall hits (see §4.4 re: real content)
    duration_ms: number | null,  // NEW — optional, populated when the operation has a measurable span
  } | null   // null for legacy nodes emitted before this schema landed — never a stub/fake value
}
```

- `operation` is **optional/nullable** at read time so existing persisted Chronos data (pre-
  migration) does not need a backfill to remain valid; new-write consumers should populate it and
  new UI should treat `operation == null` as "shallow legacy event" rather than crash or fabricate
  data (no-stub rule: an absent field is honest, a fabricated one is not).
- `inputs`/`outputs` are the actual values passed/returned, not summaries or truncated previews, so
  the drill-down (§4.4) has something real to show. Size/PII policy (e.g. truncating huge payloads,
  redacting secrets) is an implementation-stage decision against whatever the existing Chronos
  buffer/sink already does for large diffs — not invented fresh here.
- Emission stays a **consequence of praxis contracts** (ADR-0016 pattern): the operation-kind
  emitters (tool dispatch, recall call, dataflow bridge hop) each already exist as single seams in
  `pares-agens`; each needs one additional field-population call at its existing Chronos-emit site,
  not a new instrumentation layer.

### 4.4 Drill-down surfacing real recalled content (not a semantic-fallback substitute)

This is where requirement 5 intersects directly with `plureslm-openclaw` PR #16. Today's known bug
(PR #16, OPEN): `memory_get`/`readFile()` resolves an incoming path via `store.recall(id, 1)` — a
**semantic similarity search over the id string itself** — and falls back to `hits[0]` when no
exact id match is found. Against the real migrated store (opaque `mem:memory:<slug>:<chunkIndex>`
ids that don't lexically resemble their own content), this **silently returns an unrelated nearest-
neighbor chunk instead of the actual recalled content**. If the Chronos drill-down panel calls the
same buggy path to show "what was actually recalled," it would display **wrong content with no
indication it's wrong** — a stub-shaped bug (looks like a real answer, isn't) that this design must
not paper over.

**Decision: the drill-down panel's "show actual recalled content" feature has a hard dependency on
PR #16 landing (the fix to use `store.get(id)` exact pass-through, erroring on unknown ids) before
it can be built for real.** Per the workspace's NO-STUBS gate, this design does **not** propose a
temporary semantic-fallback display as a stand-in — that would be exactly the fabricated-content
anti-pattern the gate bans. Sequencing:

1. `operation.outputs` for a `recall_query` node stores the **PluresDB node ids** actually returned
   by the recall call (not resolved content) at emit time — this part has no dependency on PR #16
   and can be built now.
2. The drill-down panel, when the user expands a `recall_query` operation card, calls
   `memory_get`/`store.get(id)` (exact lookup) *at click time* to fetch and display the real content
   for each returned id.
3. **Until PR #16 lands**, the drill-down panel must either (a) be gated absent for
   `recall_query` node content-resolution specifically (per the "feature simply does not exist yet"
   allowed form of "not done"), showing only the ids + operation metadata, or (b) surface the fetch
   through the *already-fixed* `store.get(id)` call directly (bypassing the buggy `readFile()`
   wrapper) if that lower-level exact-get is independently available and correct today — this is an
   implementation-stage decision to verify against the current `plureslm-openclaw` source, not
   assumed here.
4. Once PR #16 lands, the same call path in the drill-down panel is simply correct without any
   further design change — no special-casing needed once the exact-lookup is honest.

## 5. Non-goals

- No change to Chronos's persistence/storage engine (`pluresdb-chronos` crate internals, sync,
  Hyperswarm) beyond the additive schema field.
- No rewrite of `ChronicleViewer.svelte`'s existing timeline/search/trace UI — only additive props
  for operation-kind badges and a drill-down detail panel.
- No new transport/protocol for streaming Chronos nodes to the debug window — reuse the existing
  Chronos subscription + praxis-fact/render-mode mechanism.
- No resolution of PR #16 itself in this ADR (external dependency, already filed and in review).

## 6. Recommended breakdown into child epics/tasks (next phase: dev/QA/deploy)

1. **[pluresdb / plures/chronos] Chronos operation-schema expansion** — add `operation` block
   (§4.3) to `ChronicleNode` (TS) and the Rust equivalent in `pluresdb-chronos`; additive, non-
   breaking; unit tests for null-safety on legacy nodes; wire `session_id`/`turn_id` population at
   each existing emit seam in `pares-agens`.
2. **[pares-agens] Live-context Chronos subscription + pause-fact hook** — subscribe to the
   session-scoped Chronos node stream; implement the pause-fact gate at the emit-consumer boundary
   (§4.2) with the auto-resume timeout safeguard; expose both via an ActionHandler/MCP-tool surface
   so GUI and TUI render modes share one data path (no duplicate logic per render mode).
3. **[design-dojo] ChronicleViewer operation-card + drill-down panel** — additive props consuming
   the new `operation` field; operation-kind badges/icons; expandable detail panel for
   inputs/outputs; Storybook stories for the new states (with-operation, legacy-null-operation,
   recall-drill-down with real vs. gated content per §4.4 step 3).
4. **[pares-agens] Debug-mode window wiring into render surface** — GUI panel + TUI-compatible view
   (per ChronicleViewer's existing `useTui()` support) showing the live subscription + pause button;
   gated behind a debug-mode praxis fact/flag, not always-on.
5. **[cross-repo, blocked on plureslm-openclaw#16] Recall-content drill-down finalization** — once
   PR #16 lands, wire the drill-down's `recall_query` content resolution through the fixed exact-
   lookup path and remove the interim gating from item 3.

Each item gets its own design→dev→test→verify staged lifecycle per DEVELOPMENT-LIFECYCLE.md; item 5
is explicitly blocked pending an external PR and should not start implementation until PR #16 is
confirmed merged.

## 7. Approval Gate

This ADR is **design-only**, per C-DEV-001 (.px-first) and this task's explicit no-implementation
scope. No code, no PR opened. Next stage requires explicit review/approval before any of the five
child epics in §6 begin implementation.
