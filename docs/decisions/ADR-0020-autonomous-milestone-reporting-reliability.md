# ADR-0020: Autonomous Milestone Reporting Reliability

**Status:** PROPOSED (design-only)
**Date:** 2026-07-24
**Supersedes:** none. Prior "ADR-0020" claimed by an earlier session was never
actually committed — verified 2026-07-24 that no such file exists in
`docs/decisions/`, `praxis/decisions/`, or anywhere in `git log --all`.
The branch previously associated with it (`design/autonomous-milestone-reporting-reliability`)
has a tip commit (`4438305`) that is actually ADR-0017 (Agency↔PluresLM MCP
boundary, already merged via #629) — a stale/reused branch name, zero
unmerged content. This document is the real, from-scratch diagnosis.

## Context

praxisbot (pares-agens autonomous dispatch) self-assigned a reporting
milestone and then went silent for ~3 hours before eventually failing with a
mid-turn timeout, with no intermediate progress signal ever reaching any
channel. The goal of this ADR is to explain *why the silence is possible at
all* by tracing the actual completion-reporting code path, not to re-litigate
the specific incident.

## Investigation and Evidence

### 1. Completion reporting is in-memory only — no durability

`crates/core/src/delegation/manager.rs`:

- `SubAgentManager` holds `completion_tx: mpsc::UnboundedSender<CompletionEvent>`
  (line 140), created via `mpsc::unbounded_channel()` in `SubAgentManager::new`
  (lines 146-154).
- `CompletionEvent` (struct at line 68) has five fields: `session_id`,
  `agent_name`, `result: Result<String, String>`, `duration`, and
  `undelivered_steerings`. **No persistence, no epic/task/channel
  correlation IDs beyond the raw session UUID.**
- The event is constructed and sent once, at the tail of the spawned tokio
  task in `spawn()` (lines 267-278):
  ```rust
  let event = CompletionEvent { session_id: session_id.clone(), agent_name, result, duration, undelivered_steerings };
  if let Err(e) = tx.send(event) {
      debug!(session_id = %session_id, "completion event receiver dropped: {e}");
  }
  ```
- This is a plain in-process Tokio `mpsc` channel. It exists only for the
  lifetime of the `SubAgentManager` instance and the process. If the
  receiving end has been dropped (server restart, receiver task panicked,
  process crash) the `send` returns `Err`, which is swallowed at `debug!`
  level and the event is **gone forever** — there is no queue, no disk
  write, no retry, no replay.
- **Conclusion: completion reporting is 100% in-memory. A crash, restart, or
  simply nobody currently listening on the channel silently and permanently
  drops the milestone-completion signal.** This is the direct mechanism by
  which "3h of silence" is possible: if the forwarding task isn't running (see
  §2) or the channel receiver was dropped, the sub-agent can complete (or
  time out) and nothing downstream ever finds out.

### 2. `kill()` never produces a `CompletionEvent`

`crates/core/src/delegation/manager.rs`, `kill()` (lines 310-330):

```rust
pub async fn kill(&self, session_id: &str) -> bool {
    let handle = self.handles.lock().await.remove(session_id);
    if let Some(h) = handle {
        h.abort();
        let mut sessions = self.sessions.write().await;
        if let Some(info) = sessions.get_mut(session_id) {
            info.status = SessionStatus::Killed;
            info.completed_at = Some(Utc::now());
        }
        self.steering_txs.write().await.remove(session_id);
        info!(session_id = %session_id, "sub-agent killed");
        true
    } else {
        false
    }
}
```

`kill()` only updates the in-memory `SessionInfo.status` to `Killed` and logs
an `info!` line. It never touches `completion_tx`. Compare to the normal
completion path inside `spawn()`'s background task, which is the *only*
place a `CompletionEvent` is ever constructed. Because `h.abort()` is a raw
Tokio task abort, the async task that would have built and sent the
`CompletionEvent` (at the tail of the `spawn()` closure) is torn down
mid-flight and never reaches that code — so **an aborted/killed session
produces no completion signal of any kind**, in-memory or otherwise. Any
external observer (dashboard, reporting pipeline, chat channel) has to poll
`SubAgentManager::list()`/`get()` to ever discover a session was killed; it
is never pushed.

The same asymmetry applies to a hard task-manager timeout that is enforced
above this layer (e.g. a wrapping "mid-turn timeout") — if the enclosing
runtime kills the process/task via an external abort rather than the
`tokio::time::timeout` already inside `spawn()` (lines 216-224, which *does*
correctly produce a `CompletionEvent` with `SessionStatus::TimedOut`), the
same silent-abort gap applies.

### 3. `spawn_completion_forwarder` is dead code — no production call site

`crates/mcp-server/src/server.rs` (lines 372-390) defines:

```rust
pub fn spawn_completion_forwarder(
    mut completion_rx: mpsc::UnboundedReceiver<pares_agens_core::delegation::CompletionEvent>,
    notification_tx: mpsc::UnboundedSender<ServerNotification>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(event) = completion_rx.recv().await {
            ...
            if notification_tx.send(notif).is_err() {
                break;
            }
        }
    })
}
```

It is re-exported from `crates/mcp-server/src/lib.rs:45`
(`pub use server::{spawn_completion_forwarder, ...}`). A repo-wide search
(`crates/**/*.rs`, excluding `target/`) for `spawn_completion_forwarder(`
finds **exactly one occurrence: the function definition itself.** There is
no call site anywhere in `main.rs`, any binary entry point, any plugin
wiring, or any test. This function is never invoked in the running binary.

**Conclusion: even the one function whose entire purpose is to bridge
`CompletionEvent`s into `ServerNotification`s (which would presumably reach a
client/channel) is unreachable dead code.** Combined with §1/§2, this means
the *only* currently-live path by which a completion could reach anything
is whatever bespoke code directly holds the `mpsc::UnboundedReceiver` (if
any) — and even that path is in-memory-only, non-durable, and blind to
kill/abort.

### 4. No outbox / retry / idempotency / replay / restart-recovery mechanism

Grepped for `outbox`, `retry`, `idempoten`, `replay` across `crates/**/*.rs`
and `praxis/**/*.px`: no persistent outbox table, no retry-with-backoff
wrapper around notification delivery, no idempotency key associated with a
`CompletionEvent` (the only identifier is the ephemeral `session_id` UUID,
not stable across a restart), and no restart-recovery step that re-derives
"did any spawned session finish while nobody was listening?" from
persisted state. `SessionInfo` (the struct actually queryable via
`list()`/`get()`) *is* held in an `Arc<RwLock<HashMap<...>>>` for the life
of the process, but that map is never persisted to PluresDB/Chronos/disk —
a process restart loses it entirely, and even short of a restart, nothing
polls it proactively; it is pull-only.

### 5. No correlation to task/epic/channel

`CompletionEvent` and `SessionInfo` both key on `session_id` (a
`Uuid::new_v4()` minted at spawn time) and `agent_name` only. Neither struct
carries a `task_id`, `epic_id`, or `channel`/`delivery target` field. Compare
to `autonomous-dispatch.px`'s `EvaluableTask` fact, which *does* have a
stable `id` used as the PluresDB key prefix `task:{id}:*`
(`praxis/procedures/autonomous-dispatch.px`, `evaluate_dispatch`). There is
no code path that stitches a `CompletionEvent.session_id` back to the
`task:{id}` that caused the dispatch, so even if a `CompletionEvent` did
reliably reach a listener, that listener has no reliable way to know which
task/epic/channel it belongs to without an out-of-band lookup that does not
currently exist.

### 6. Semantic mismatch: Rust task-manager states vs. `.px` task states

- Rust `SessionStatus` (`crates/core/src/delegation/manager.rs:30-40`):
  `Running | Completed | Failed(String) | Killed | TimedOut` — five states,
  tracking a spawned *sub-agent session*.
- `.px` task state (`praxis/procedures/task-system.px:15`):
  `status: enum(pending, complete)` — only two states, tracking a
  *dispatched autonomous task* record in PluresDB.

There is no `in_progress`/`running` state in the `.px` task model even
though `autonomous-dispatch.px`'s `evaluate_dispatch` procedure explicitly
writes `pluresdb_write {key: "task:{$best.id}:status", value: "in_progress"}`
(a string not enumerated in the fact's own `status` type comment) and never
writes anything else — there is no procedure anywhere in
`autonomous-dispatch.px` or `task-system.px` that transitions a task from
`in_progress` back to `complete` on completion, nor to a `failed`/`killed`
terminal state on error. `task-system.px`'s `complete_task` procedure (lines
41-47) exists and *can* write `status: "complete"`, but nothing in
`autonomous-dispatch.px` calls it — the dispatch side marks a task
`in_progress` and then there is no wired dataflow edge back from a Rust-side
`CompletionEvent`/`SessionStatus` into a `.px` procedure that ever calls
`complete_task`. A task that gets dispatched and whose sub-agent then times
out or is killed (§2/§3 above showing no completion signal reaches
anything) is left stuck at `in_progress` forever, with no `.px` constraint
watching for that (the two `heartbeat-logic.px` / `autonomous-dispatch.px`
constraints that exist — `dispatch_respects_cooldown`,
`dispatch_respects_max_attempts` — only gate *new* dispatch, they do not
detect or repair a stuck `in_progress` task).

### 7. Heartbeat quiet-hours/daily caps are charged at dispatch time, not delivery time

`crates/core/src/heartbeat.rs`, `tick()` (lines 226-236):

```rust
async fn tick(&self) {
    if !self.config.enabled { return; }
    if self.config.is_quiet_hour() { return; }
    // ── Cerebellum gate (zero tokens) ──
    let mut work_items: Vec<String> = Vec::new();
    ...
```

`is_quiet_hour()` (defined at `heartbeat.rs:55`) is checked once, at the top
of `tick()`, before any work is evaluated or any autonomous task is
dispatched. There is no second quiet-hours/cap check anywhere on the
*delivery* side — i.e. nothing re-checks `is_quiet_hour()` (or any daily-cap
counter — no `daily_cap`/`max_per_day` field or counter exists in
`heartbeat.rs`, `personality.rs`, or `runtime.rs` at all, confirmed by grep)
at the point a `CompletionEvent`/notification would actually be delivered to
a channel. This means: a long-running task dispatched at, say, 22:55 that
completes at 23:10 (inside quiet hours) has no gate re-evaluating whether
*delivering* that result at 23:10 should be suppressed or deferred — the
quiet-hours check already passed at dispatch time and is architecturally
incapable of running again at delivery time because there is no delivery-time
hook at all (per §1-§3, nothing is listening for delivery in production).
Conversely, if delivery *did* exist, it would need its own quiet-hours check
independent of the dispatch-time one; today neither exists in a wired state.

## Root Cause Summary

The "3h silence, then a mid-turn timeout" failure mode is not one bug — it
is the compounding of several real gaps, each independently confirmed above:

1. Completion signaling is a bare in-memory `mpsc` channel with no
   persistence (§1).
2. `kill()`/abort paths never produce a completion signal at all, silently
   (§2).
3. The one function that would forward completions to a client-visible
   notification is dead code — never called (§3).
4. There is no outbox/retry/idempotency/replay/restart-recovery layer to
   catch what the in-memory channel drops (§4).
5. Completion events cannot be correlated back to the task/epic/channel that
   caused them (§5).
6. The Rust task-manager's five-state model and the `.px` two-state model
   are not wired together; a task can get stuck at `in_progress` forever
   with nothing to detect or repair it (§6).
7. Quiet-hours/cap gating only exists at dispatch time; there is no
   delivery-time equivalent, so even if delivery existed it would have no
   guardrail of its own (§7).

Given all of this, a task can be dispatched, run for hours, get killed or
time out, and the *entire system* — Rust task manager, `.px` dispatch layer,
and any human-facing channel — has no reliable way to ever learn that
outcome. Silence is not a bug in one component; it is the expected behavior
of the current wiring.

## Proposed Fix (design-only, not yet implemented)

Introduce a durable **lifecycle-event + outbox** pipeline:

1. **Durable lifecycle events.** Replace the bare `mpsc::UnboundedSender` at
   the tail of `SubAgentManager::spawn()`'s task with a write to a durable
   `outbox` table/stream (PluresDB-backed, matching the pattern already used
   for `task:{id}:*` keys in `autonomous-dispatch.px`) keyed by a stable
   `(task_id, epic_id, session_id)` tuple. Every terminal `SessionStatus`
   transition (`Completed`, `Failed`, `Killed`, `TimedOut`) writes exactly one
   outbox row, including from `kill()` — `kill()` must be changed to write a
   `Killed` lifecycle event synchronously before/instead of relying on the
   aborted task's tail code to do it (today it never runs).
2. **Correlation fields.** Extend `CompletionEvent`/the new outbox record
   schema with `task_id: Option<String>`, `epic_id: Option<String>`, and
   `channel: Option<String>`, threaded through from `SpawnOptions` (which
   already carries `parent_context` — extend that context to carry these
   IDs) so a listener can always answer "which task/epic/channel does this
   belong to" without a side lookup.
3. **Outbox delivery worker with retry.** A single, always-on worker (wired
   into the actual binary entry point, unlike `spawn_completion_forwarder`
   today) drains the outbox, delivers to the destination channel, and only
   marks a row delivered on ack; undelivered rows are retried with backoff.
   This worker is the thing that must be proven to have a real call site —
   the exact class of bug found in §3.
4. **Restart recovery.** On process start, the worker re-scans the outbox
   for any `pending`/`in_progress` rows and resumes delivery — closing the
   restart-loses-everything gap in §1/§4.
5. **`.px` state alignment.** Expand `task-system.px`'s `status` enum to
   include `in_progress` and `failed`/`killed` (matching the Rust
   `SessionStatus` states already in use), and add a dataflow edge in
   `autonomous-dispatch.px` (or a new procedure) that consumes the durable
   lifecycle event and calls the existing `complete_task` procedure (or a new
   `fail_task`/`kill_task` procedure) — closing the stuck-`in_progress` gap
   in §6.
6. **Delivery-time quiet-hours/cap check.** Add a second `is_quiet_hour()`
   (and, if a daily cap is ever introduced, a cap check) evaluation at the
   point the outbox worker is about to deliver, independent of the
   dispatch-time check in `heartbeat.rs::tick()` — closing §7. A task
   completed during quiet hours should have its delivery deferred to the
   next non-quiet window, not silently dropped or silently sent regardless.

This ADR is design-only: no code changes are included in this commit. The
next stage is a dev-lifecycle task definition (per
`repos/plures/development-guide/practices/session-workspace-isolation.md`
and the pares-radix dev-lifecycle orchestration pattern) to implement items
1-6 above with test coverage for each of the seven root-cause gaps
identified.

## Consequences

- Adds a PluresDB-backed outbox table/stream and a durable worker — new
  operational surface, but it is the minimum needed to make "praxisbot
  reported nothing for 3 hours" architecturally impossible rather than
  merely unlikely.
- Requires extending `SpawnOptions`/`parent_context` and the `.px`
  `task-system.px`/`autonomous-dispatch.px` state model — a coordinated
  change across the Rust core crate and the praxis procedures, which is why
  this is being designed as one ADR before either side is touched.
- Does not attempt to fix the specific 3h-silence incident post hoc; it
  fixes the structural gaps that made any such incident possible at all.
