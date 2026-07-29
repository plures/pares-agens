# ADR-0015: Autorecall Topic-Shift Detection — Retire the Dead `actions.rs` Stub

**Status:** PROPOSED (design only — no implementation until reviewed)
**Date:** 2026-07-24
**Epic:** `pares-agens:autorecall-orchestrator-fixes`
**Supersedes:** none
**Related:** ADR-0012 (authorization gate), #549 (topic-shift detection introduced), #624 (amnesia/timeout fixes)

## 1. Summary

The prior analysis pass flagged `actions.rs::detect_topic_shift_action` as
"looks dead/orphaned vs `mod.rs`'s real implementation." This retry
**verifies that finding is correct** and designs the fix: delete the dead
stub, document the live path, and close two real gaps in the live topic-shift
implementation (`Orchestrator::detect_topic_shift` in `mod.rs`) that the epic's
"topic-shift suppression" complaint is actually about.

No code changes are made in this document. It is the design artifact gating
implementation.

## 2. Verified Finding: `actions.rs::detect_topic_shift_action` is dead code

### 2.1 Evidence

`crates/core/src/orchestrator/actions.rs:530`:

```rust
/// Detect topic shift (placeholder — needs embedding comparison).
fn detect_topic_shift_action(params: &Value) -> Result<Value, ExecutionError> {
    // Without embeddings, assume no shift (conservative)
    let _topic = params["topic"].as_str().unwrap_or_default();
    Ok(json!(false))
}
```

Registered only in the `AsyncActionHandler::call` dispatch table
(`actions.rs:1003`) under the string key `"detect_topic_shift"`. That
dispatch table is invoked exclusively by the **.px trigger/procedure
runtime** (`PxProcedureAdapter` → `AsyncActionHandler::call`), which is
reached only when a `.px` procedure text literally calls the action by name.

The only `.px` source referencing that action name is
`praxis/procedures/classify.px:46`:

```
detect_topic_shift {topic: $topic} -> $topic_shift
```

...inside `procedure classify_message(...)`. Tracing the call graph for
`classify_message`:

- `praxis/procedures/preprocess.px:42` calls `classify_message {...}` as a
  *dataflow* step inside `procedure preprocess(event from "inbound")`.
- `preprocess.px`'s `preprocess` procedure is itself dataflow-shaped
  (`... from "inbound"`, `... into "preprocessed"`), i.e. it is meant to run
  through the **dataflow bridge** (`dataflow_bridge.rs`), not the older
  trigger-based `PxBridge`.
- The only Rust call sites for `classify_message` as a *named procedure
  invocation* are: `px_bridge.rs:196` (`PxBridge::classify_message`, a thin
  wrapper) and `px_bridge.rs:267` (a **unit test only**, `bridge.classify_message("hello", ...)`).
- **No production code path in `orchestrator/mod.rs`, `agent.rs`, or
  `agents-plugin/runtime.rs` calls `PxBridge::classify_message` or
  `PxBridge::call("classify_message", ...)`.** `rg` over
  `crates/core/src` and `crates/agens-plugin/src` confirms the only
  non-test caller of `.classify_message(` is the doc test in
  `px_bridge.rs`.
- Orchestrator's real preprocessing entry point is
  `Orchestrator::preprocess()` (`mod.rs:277`), called from
  `agent.rs:794` and `agent.rs:924`. That function does **not** call
  `px_bridge.classify_message` or `dataflow_bridge` for classification —
  it calls `self.detect_topic_shift(event, &query_embedding)` directly
  (`mod.rs:314`), a private method defined at `mod.rs:622` with real
  cosine-similarity logic and an embedding cache
  (`self.topic_embeddings: Mutex<HashMap<...>>`).
- Orchestrator's `route()` path (`mod.rs:456-484`) does route through
  `dataflow_bridge` → `px_bridge` → `router::decide()` in that
  precedence order, but that path is for **routing decisions**
  (`route_event` / `router::decide`), not for the `classify_message`
  procedure that contains `detect_topic_shift`. `preprocess.px`'s
  `preprocess` procedure (which calls `classify_message`) is never wired
  to either bridge from a production caller — only `route_event`-shaped
  procedures are (confirmed via `with_px_bridge` / `with_dataflow_bridge`
  call sites in `agens-plugin/runtime.rs:322,329,400,5279,5288,5357`,
  all of which wire routing, not the preprocess/classify dataflow).

### 2.2 Conclusion

**Verified: `actions.rs::detect_topic_shift_action` is orphaned dead code.**
It exists only to satisfy a `.px` procedure (`classify.px`'s
`classify_message`) that itself has no production-code invoker. The
constant-`false` placeholder body ("assume no shift, conservative") can
never suppress or corrupt live topic-shift behavior because it never runs
in production. Its presence is a **maintenance hazard**, not a functional
bug: someone reading `classify.px` or `actions.rs` reasonably concludes
topic-shift detection is a naive always-false stub, when the real
implementation is a working cosine-similarity comparator 300 lines away in
a different file, reached through a completely different call path.

This also means: **the epic's "topic-shift suppression" symptom, if real,
is not caused by this dead stub.** The live implementation
(`Orchestrator::detect_topic_shift`, `mod.rs:622`) is the only candidate, and
Section 3 audits it directly.

## 3. Live Implementation Audit: `Orchestrator::detect_topic_shift` (`mod.rs:622`)

```rust
fn detect_topic_shift(&self, event: &Event, current_embedding: &[f32]) -> bool {
    let Some(channel_key) = event_channel_key(event) else { return false; };

    if let Event::Message { content, .. } = event {
        if content.trim().len() < 20 {
            // cache update, then...
            return false;   // (A) short-message suppression
        }
    }

    let mut embeddings = match self.topic_embeddings.lock() {
        Ok(guard) => guard,
        Err(e) => { warn!(...); return false; }   // (B) poisoned-lock suppression
    };
    let shifted = embeddings.get(&channel_key)
        .map(|previous| cosine_similarity(previous, current_embedding)
            < self.config.topic_similarity_threshold)
        .unwrap_or(false);   // (C) first-turn-per-channel suppression
    embeddings.insert(channel_key, current_embedding.to_vec());
    shifted
}
```

Called only from `preprocess()` (`mod.rs:314`), and **only when
`skip_recall` is false** (`mod.rs:292-300`): messages with ≤3 words never
reach `detect_topic_shift` at all — they hit the "fast path" that skips
embedding + recall entirely, silently leaving `topic_shifted = false`
regardless of actual topic history. This is a **fourth, distinct**
suppression path not visible by reading `detect_topic_shift` in isolation.

### 3.1 Four suppression paths, in effective order

| # | Location | Condition | Effect | Is it a bug? |
|---|----------|-----------|--------|---------------|
| 1 | `mod.rs:292-300` | message ≤ 3 words | `skip_recall=true`; `detect_topic_shift` never called; `topic_shifted` stays `false` | **Design gap** — a 3-word message *can* be a real topic shift ("ok new question", "switch to X") and will never clear stale context. |
| 2 | `mod.rs:630` | `content.trim().len() < 20` chars | forces `false`, but *does* update the embedding cache | Intentional (documented: follow-ups like "do that"). Reasonable, but 20 chars is a blunt proxy for "short reply" — e.g. "no, german recipes" (19 chars) is a topic shift and is 19 chars, suppressed. |
| 3 | `mod.rs:642-645` | `topic_embeddings.lock()` returns `Err` (poisoned mutex) | logs a warning, returns `false` permanently for that process lifetime once poisoned | **Silent failure mode** — no recovery path; once poisoned, topic-shift detection is dead for the rest of the process, with only a `warn!` log and no alert/metric/self-heal. |
| 4 | `mod.rs:649-651` | no prior embedding cached for `channel_key` (first message in channel, or cache evicted) | `.unwrap_or(false)` — first turn in any channel is *never* a shift by definition, which is correct, but is indistinguishable in logs/metrics from paths 1–3. | Correct behavior, but **unobservable** — cannot tell "worked correctly, first turn" from "suppressed due to bug" from logs alone. |

### 3.2 What the epic likely means by "topic-shift suppression"

Given the four paths above, the plausible bug reports feeding this epic are:
short/terse topic-changing messages (path 1, ≤3 words: "new topic", "switch
topics", "stop that") silently fail to clear stale recalled context, and
there is **no telemetry** distinguishing "correctly detected no shift" from
"structurally incapable of detecting a shift because the message was too
short to reach the check." This matches a `#549`/`#624`-era class of bug
(amnesia/stale-context issues already fixed once in #624 for a different
mechanism).

## 4. Design

### 4.1 Goals

1. Remove the dead orphaned code path so the codebase has one obvious
   source of truth for topic-shift detection.
2. Close the short-message blind spot (path 1) without regressing the
   documented "don't treat short follow-ups as shifts" behavior (path 2).
3. Make suppression **observable** (metrics/log fields) so paths 1–4 are
   distinguishable in production telemetry, closing the loop the epic
   needs for diagnosis.
4. Give poisoned-lock failure (path 3) an explicit recovery path instead
   of silent permanent degradation.
5. Add unit tests that pin current + new behavior per suppression path so
   regressions are caught mechanically, not just by prose.

### 4.2 Non-goals

- No change to the cosine-similarity threshold value (`0.72`) — that's a
  tuning question, out of scope for this design.
- No change to routing precedence (`dataflow_bridge` → `px_bridge` →
  `router::decide`).
- No migration of `classify_message`/`classify.px` onto a live call path —
  that is a separate "wire up dataflow preprocess" epic if ever needed;
  this design only removes the dead stub, it does not resurrect the
  procedure.

### 4.3 Seams (where changes land)

| Seam | File | Change type |
|------|------|-------------|
| S1 | `crates/core/src/orchestrator/actions.rs` | **Delete** `detect_topic_shift_action` fn + its `"detect_topic_shift" => ...` dispatch arm. Add a `// REMOVED:` comment pointing to `mod.rs::detect_topic_shift` for future readers who grep the old name. |
| S2 | `praxis/procedures/classify.px` | Remove `detect_topic_shift {topic: $topic} -> $topic_shift` line from `classify_message`, since its only implementation is being deleted and the procedure has no live caller. Add a header comment noting `classify_message` is currently unreachable from production Rust (dataflow wiring not done) so nobody re-adds calls to a phantom action. |
| S3 | `crates/core/src/orchestrator/mod.rs` (`preprocess`, `mod.rs:277-330`) | Replace the binary `skip_recall` short-circuit with a **three-way** path: `skip_recall_for_length` (≤3 words) still skips *recall* (expensive embedding lookup unchanged for cost reasons) but no longer implicitly forces `topic_shifted = false` — instead it explicitly records `topic_shift_check = Skipped { reason: "short_message" }` in a new observability struct (see S4), preserving current *behavior* (still no shift triggered — we are not embedding 3-word messages, that's an explicit non-goal) while making the omission visible. |
| S4 | `crates/core/src/orchestrator/mod.rs` (`detect_topic_shift`, `mod.rs:622-651`) | Change return type from `bool` to a small enum `TopicShiftOutcome { Shifted, NotShifted, SkippedShortMessage, SkippedShortReply, SkippedCachePoisoned, SkippedNoPriorTurn }` (or equivalent struct with a reason field) so callers get both the boolean *and* the reason. `preprocess()` derives `topic_shifted: bool` from it (`matches!(outcome, Shifted)`) so **no behavior changes** for the `items.clear()` decision at `mod.rs:369` — only observability changes. |
| S5 | `crates/core/src/orchestrator/mod.rs` (poisoned-lock branch, `mod.rs:642-645`) | Recovery path: on `PoisonError`, call `.into_inner()` to recover the guard (mutex poisoning here only reflects "a prior thread panicked while holding it", the `HashMap` itself is not corrupt) instead of unconditionally returning `false` forever. Keep the `warn!` but change it to reflect that recovery succeeded, and add a counter metric `cerebellum_topic_embeddings_lock_recovered_total` so repeated recoveries are visible (signal of a real panic bug elsewhere, without permanently disabling topic-shift detection as a side effect). |
| S6 | new: `crates/core/src/orchestrator/mod.rs` tests module (`mod.rs:960+`, existing `#[cfg(test)]`) | Add table-driven unit tests, one per row in §3.1's table, asserting the returned `TopicShiftOutcome` variant for: ≤3-word message, <20-char message, poisoned lock (recovered, not permanently false), first-turn-per-channel, and a genuine shift (low cosine similarity) — plus a regression test that `actions.rs` no longer contains `detect_topic_shift_action` (grep-based `assert!` in a `#[test]` is acceptable, or CI grep, see §4.5). |

### 4.4 Recovery paths

- **Poisoned lock (S5):** recover via `into_inner()` rather than treating
  poisoning as fatal to the feature. Justification: `topic_embeddings` is a
  plain `HashMap<String, Vec<f32>>` with no invariants that a panicking
  writer could leave "torn" in a way that produces incorrect (as opposed to
  merely stale) cosine-similarity results — worst case is one stale/missing
  entry, which the `unwrap_or(false)` already handles safely. Silently
  disabling the feature forever is strictly worse than momentarily reduced
  accuracy.
- **Dead `.px` action removal (S1/S2):** staged as two independent, small,
  revertible commits (Rust deletion, then `.px` deletion) so if the "verify
  it's truly dead" finding is somehow wrong in a way `rg`/call-graph
  tracing missed, `git revert` on just the `.px` change re-enables the
  procedure without touching Rust, and vice versa.
- **Regression guard (S6):** a CI-visible test that fails loudly if anyone
  reintroduces `detect_topic_shift_action`-shaped dead code (name-based
  grep assertion) rather than relying on future code review catching it
  again.

### 4.5 Test plan (design-level; no code yet)

1. `cargo test -p pares-radix-core orchestrator::` — existing suite must
   stay green through S3–S5 (no behavior regression for the
   `items.clear()` semantics currently tested, e.g. `mod.rs:1024,1039,1054`
   preprocess integration tests).
2. New unit tests per §4.3/S6, one assertion per suppression path.
3. New test: parse `classify.px` after S2 and assert
   `detect_topic_shift` is no longer referenced (prevents silent
   re-coupling to a deleted action name).
4. New test: grep-based check (either a `build.rs` step or a `cargo test`
   using `include_str!` + `assert!(!contents.contains("detect_topic_shift_action"))`)
   against `actions.rs` so a future contributor cannot silently re-add the
   orphaned stub without it failing CI.
5. Manual verify step for the eventual implementation PR: exercise a real
   3-word topic-changing message ("switch to cooking") in a channel with
   prior unrelated context loaded, and confirm (via new observability
   field, S4) that the outcome is logged as `SkippedShortMessage` rather
   than silently indistinguishable from `NotShifted`.

## 5. Risks / Open Questions

- **R1:** Deleting `classify.px`'s `detect_topic_shift` line changes the
  `classify_message` procedure's output shape (`$topic_shift` var
  disappears). Since no production caller exists (verified §2), this is
  safe today, but if a future dataflow-migration epic resurrects
  `classify_message` as a live path, it will need to re-derive
  `topic_shift` from the real `mod.rs` implementation, not reintroduce the
  dead stub. This design should be linked from that future epic.
- **R2:** Should the ≤3-word fast path (suppression #1) ever be changed to
  *not* skip recall for topic-changing short messages? Left as non-goal
  here — flagged for a follow-up design if telemetry (post S3/S4) shows
  it's a frequent real-world miss.
- **R3:** Metric names in S5 are illustrative; align with whatever
  metrics/telemetry convention `pares-agens` already uses elsewhere in
  `orchestrator/` before implementation (check for an existing `metrics`
  crate usage pattern in the codebase during the fix stage, not this
  design stage).

## 6. Approval Gate

This ADR is **design-only**. Per epic instructions: no implementation
until this is reviewed and explicitly approved. Next stage (fix) should
implement S1–S6 as separate, reviewable commits in the order listed, each
gated by its own test pass before proceeding to the next.
