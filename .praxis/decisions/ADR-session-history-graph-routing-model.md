# ADR: Session-History-as-Graph Routing Model (replaces brain-metaphor)

**Status:** Proposed
**Date:** 2026-07-29
**Supersedes:** cerebellum/conscious/subconscious and primary-agent/subagent framing (already
being dismantled in `rename/brain-metaphor-cleanup`); DelegationBroker's role is
re-scoped by this ADR (see Decision 4).

## Context

The brain-metaphor design (cerebellum = reflexive dispatch, conscious/subconscious =
tiered model routing, primary-agent/subagent = DelegationBroker fan-out) is being
retired. `unified-router.px` already replaced the imperative Orchestrator with a
PluresDB-queue-driven dataflow (classify_and_route → context assembly / task steering →
model invocation → tool loop → delivery). What's missing is the actual **memory model**:
today "memory" (`memory.px`), "history" (`chat_history:{chat_id}` state keys), and
"tasks" (`task-system.px`, `worktask.px` conceptually) are separate, ad hoc PluresDB
key namespaces with no first-class relationship between them.

Resolved inputs carried into this ADR (from epic `pares-agens:brain-metaphor-to-graph-
routing-refactor`, all confirmed by investigation, not re-asked):

1. Continuation/addition/abort classification = a vector-search function, heuristic to
   start, refined by umbra over time — not hardcoded if/else rules.
2. Session id = thread/topic scoped, channel-independent.
3. No-match on classification = never silently drop; the model decides routing when
   the vector match is ambiguous (leave the "no match" semantics to the model itself,
   not a hardcoded default branch).
4. Schema principle: everything agens writes to PluresDB gets an embedding/vector.
   "Memory" is not a separate store — it IS the collection of session histories.
   Edges: `request-routed-to-session`, `autorecall-match-to-session`,
   `session-invokes-skill-or-tool`.
5. "Relate to tasks" = `worktask:*` only. `agenda::Task` (cron scheduler) is orthogonal
   and out of scope.
6. DelegationBroker: repurpose vs fold — decided below (Decision 4).
7. Classifier = heuristic (vector similarity + light scoring) to start; LLM
   classification allowed later but is presumptively too slow/costly for the hot path
   — do not default to it without a measured latency budget.

## Decision

### 1. Session node is the canonical unit of history

Replace `chat_history:{chat_id}` flat state keys with a first-class `session` entity.
A session is the graph's memory unit — not a separate "memory" store layered on top.

```
entity session:
  prefix: "session:pares-agens:"
  fields:
    id: String
    channel: String
    thread_key: String        # channel-independent thread/topic scoping key
    started_at: Int
    last_active_at: Int
    status: enum(open, idle, closed)
    summary: String            # rolling summary, updated on close/idle transition
    embedding: Vector           # embedding of summary+recent turns, for recall matching
    turn_count: Int
    parent_session_id: String  # "" if root; set when a session forks from another
```

Every session write recomputes `embedding` from `summary` (or from the latest turn
content pre-summary). This is what makes `autorecall-match-to-session` possible without
a separate memory index: recall is a vector search directly over `session` nodes.

Turns themselves (individual messages) are not modeled as new node types here — they
continue to live as history entries scoped by `thread_key`/`session.id`, written by
`track_inbound`/`deliver_response` (already the case in `unified-router.px`). What
changes is that the **session row itself** carries the embedding and is the recall
target, not a synthetic "memory:long_term:*" key namespace layered separately (per
input #4: memory = the collection of session histories, no separate memory store).

### 2. Three edge types, each a PluresDB relation record

```
entity edge_request_routed_to_session:
  prefix: "edge:request-routed-to-session:"
  fields:
    request_id: String
    session_id: String
    route_reason: String        # e.g. "continuation", "new-topic", "no-match-model-decided"
    match_score: Float          # vector similarity score that produced this routing, 0.0 if not vector-based
    decided_at: Int

entity edge_autorecall_match_to_session:
  prefix: "edge:autorecall-match-to-session:"
  fields:
    query_embedding_source: String   # e.g. inbound message id or request id that triggered recall
    session_id: String
    match_score: Float
    rank: Int                        # 0 = best match among the recall set
    recalled_at: Int

entity edge_session_invokes_skill_or_tool:
  prefix: "edge:session-invokes-skill-or-tool:"
  fields:
    session_id: String
    invocation_kind: enum(skill, tool)
    name: String
    invoked_at: Int
    outcome: enum(success, error, pending)
```

Rationale for three distinct entities rather than one generic "edge" table: each edge
type has a materially different shape (route_reason/match_score vs recall rank vs
invocation outcome) and distinct constraints (below). PluresDB has no native graph-edge
primitive here — edges are ordinary prefixed records referencing node ids by field,
consistent with how `feature-ledger.px` already models `feature_qa_result` as a
relation record referencing `feature_id`.

### 3. Constraints

```
constraint session_requires_identity:
  scope: session
  phase: pre_write
  when: session.write_requested == true
  require: session.id != "" AND session.thread_key != ""
  severity: error
  message: "Sessions require an id and a channel-independent thread_key."

constraint session_embedding_present_when_active:
  scope: session
  phase: pre_write
  when: session.status != "closed"
  require: session.embedding != []
  severity: error
  message: "Open/idle sessions must carry a computed embedding for recall matching."

constraint routing_edge_requires_reason:
  scope: edge_request_routed_to_session
  phase: pre_write
  when: edge.write_requested == true
  require: edge.route_reason != "" AND edge.session_id != ""
  severity: error
  message: "Routing edges must record why a request was routed to a session."

constraint recall_edge_is_ranked:
  scope: edge_autorecall_match_to_session
  phase: pre_write
  when: edge.write_requested == true
  require: edge.rank >= 0 AND edge.session_id != ""
  severity: error
  message: "Autorecall match edges must record a rank within the recall set."

constraint invocation_edge_has_target:
  scope: edge_session_invokes_skill_or_tool
  phase: pre_write
  when: edge.write_requested == true
  require: edge.name != "" AND edge.session_id != ""
  severity: error
  message: "Skill/tool invocation edges must name the invoked skill or tool."
```

### 4. DelegationBroker: repurpose as the parallel-session implementation (recommendation, not a question)

**Recommendation: repurpose DelegationBroker as the concrete Rust IO-boundary executor
for parallel sessions, fold nothing custom into the .px layer.**

Evidence for repurposing over folding-and-discarding:

- `DelegationBroker::delegate` already does exactly what "parallel session execution"
  needs at the Rust IO boundary: fan out N independent units of work concurrently via
  `tokio::spawn`/`JoinSet`, each with its own isolated `AgentContext` (message history),
  its own tool-call loop bounded by `max_turns`, and an optional steering channel for
  mid-run message injection. That is a parallel-session executor in all but name — the
  "agent_name" concept maps directly onto "which session/skill definition to run", and
  `SubTask.parent_context` already exists for grounding a child session against a
  parent's summary (this is precisely the `parent_session_id` field on `entity
  session` above).
- The steering channel (`crate::delegation::steering`) is a real, tested mechanism for
  injecting messages into a running session mid-turn — the graph model has no
  equivalent primitive and re-inventing it in .px would mean re-solving an already-
  solved concurrency problem in a language not designed for it (dataflow procedures are
  not the right place to own a tokio channel).
- What DOES fold into the new model rather than staying broker-side: the *decision* of
  when to spawn a parallel session, which session to spawn against, and what routing
  edge to record. That decision belongs in `session-routing.px` (below), which decides
  routing/classification and then invokes a generic `dispatch_parallel_session` action
  that Rust implements by calling into `DelegationBroker::delegate` — mirroring the
  existing pattern where `invoke_model`/`dispatch_tools` in `unified-router.px` are
  thin px-side calls into Rust IO-boundary actions.
- Rejected alternative — fold DelegationBroker's logic into new .px procedures and
  delete the Rust type: rejected because the concurrency primitives it owns (JoinSet,
  Arc-cloned handles across await points, per-task steering channels) are exactly the
  kind of IO/runtime-boundary logic the PX-first architecture (see
  `plures:px-first-architecture-refactor`) says Rust should own, not reimplement in a
  language with no async runtime model. Deleting it and reimplementing fan-out
  concurrency as a .px dataflow procedure would violate the "Rust is the IO boundary,
  .px is the decision owner" split this refactor is built on.
- Required follow-up change to DelegationBroker itself (dev-stage work, not part of
  this ADR's scope to implement): `SubTask.agent_name`/`AgentRegistry` currently
  resolves "agents" from a static registry keyed by name+system-prompt. Under the new
  model, a spawned parallel unit is a **session** (with its own `session` entity row,
  embedding, and edges), not a registry-defined "agent". The registry lookup should be
  replaced (or supplemented) with a session-scoped variant that seeds `AgentContext`
  from a `session_id`'s summary/history rather than only a static `AgentDefinition`.
  This is a real, scoped Rust change, tracked as follow-up dev work — not resolved by
  this ADR alone.

### 5. Classification procedure shape (heuristic-first, vector-based)

See `praxis/procedures/session-routing.px` (companion file to this ADR) for the actual
dataflow procedure. Summary of the shape: `route_request_to_session` computes an
embedding for the inbound request, searches existing open/idle `session` nodes by
vector similarity, and returns one of three outcomes — continuation (attach to
existing session, write `edge_request_routed_to_session` + one or more
`edge_autorecall_match_to_session` for the candidates considered), new session (create
a session row, write the routing edge with `route_reason: "new-topic"`), or
model-decided (match score is ambiguous — neither confidently a continuation nor
confidently new — so the ambiguous candidates are surfaced to the model itself instead
of a hardcoded default, honoring input #3). LLM-based classification remains
available as a utility procedure (`classify_session_with_llm`, mirroring the
`classify_with_llm` utility pattern already in `classify.px`) but is not on the default
hot path.

## Consequences

- `chat_history:{chat_id}` state-key usage in `unified-router.px`
  (`assemble_context`/`track_inbound`/`deliver_response`) needs a follow-up dev-stage
  migration to read/write against `session` entity rows keyed by `thread_key` instead
  of raw chat_id state keys — tracked as follow-up, not done in this design pass.
  `memory.px`'s `init_session_memory`/`extract_memories` similarly need a follow-up
  pass to source from `session.embedding` recall instead of a separate
  `memory:long_term:*` prefix, once this schema lands.
- `feature-ledger.px`'s existing `sort_by_field`/`render_markdown_table` generic
  actions (from PR #682) are a usable precedent for building a
  `render_session_graph_markdown` debug view later, if wanted — not required for this
  ADR.
- This ADR does not implement the migration of existing `chat_history`/`memory:*` data
  into the new schema; that is separate dev-stage work once the schema is reviewed.
