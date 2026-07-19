# Telegram Turn UX — design (kbristol 2026-07-19)

## Problem (ground-truth from code, not memory)
`crates/channels/src/telegram.rs` (3152 lines), progressive-turn block ~L2489:

1. **Hourglass**: placeholder is a bare `⏳` (L2492). Tool-heavy turns rewrite the whole
   line with `⏳ Working… 🔧 {name} (step N)` (L2562) every debounce tick — flickers,
   noisy, no stable status label.
2. **No steering**: inbound Telegram messages during an active turn spawn an independent
   new turn. `SteeringTx`/`SteeringRx` (`core/src/delegation/steering.rs`) + the broker
   drain point (`broker.rs:204`) exist but are wired only to sub-agent delegation, never
   to the user's Telegram turn.
3. **Dead control buttons**: handler is `Update::filter_message()` only (L1374) — there is
   **no `callback_query` branch**. `approval_keyboard()` (L892) renders ✅/❌ buttons but
   tapping them is a no-op (nothing consumes the callback). No Stop button at all.

## Decision (kbristol): option (b)
Mid-turn user message is **injected into the running turn** via `SteeringTx` (adapts
without losing progress), NOT a hard interrupt/restart. Feasible because the broker turn
loop already drains a steering queue between steps.

## Architecture — per-chat active-turn registry (the shared seam)
Both steering and control-buttons need to reach the *currently running turn for a chat*.
Introduce one concurrency-safe registry:

```
ActiveTurns = Arc<DashMap<ChatId, TurnHandle>>
TurnHandle { steering_tx: SteeringTx, cancel: CancellationToken, request_id: String }
```

- Register on turn start (when placeholder is sent), remove on turn end.
- **Steering (S2)**: message branch checks `ActiveTurns.get(chat)`. If a turn is live →
  `steering_tx.send(text)` (route into running turn) instead of starting a new turn.
- **Control (S3)**: new `callback_query` branch parses `callback_data`:
  - `stop:{request_id}` → `handle.cancel.cancel()` (co-op cancel the turn)
  - `approval:yes|no:{request_id}` → resolve the pending approval (wire to existing
    approval request path so the ✅/❌ buttons finally work)
- **Progress UX (S1)**: independent — replaces the status-string logic only. Stable
  single-line status: `▸ {phase}` with a compact spinner frame, tool name without the
  ever-incrementing raw step counter dominating; switch to answer text once content
  streams (unchanged). No behavior depends on the registry.

## Side-effect boundary (C-DEV-001)
Pure turn-control policy → `.px` (`praxis/procedures/telegram-turn-ux.px`): when to steer
vs. start-new, what control actions are valid, approval resolution rules. Rust is only the
Telegram API side-effect actor (send/edit message, answer callback) + the registry plumbing.

## Test plan (C-TEST-002 — channel-agnostic, local)
- `steering.rs` already unit-tested (send/drain/has_pending/clone) — extend with a
  route-decision test (live turn ⇒ steer; no turn ⇒ new).
- Registry: unit tests for register/lookup/remove, concurrent access, cancel propagation.
- `callback_data` parse/dispatch: pure-function tests (`stop:`/`approval:yes|no:` → action),
  no Telegram transport required.
- Status renderer: pure `render_status(phase, tool, streamed)` → string, asserted directly
  (no bot). Existing tests at L2738 already assert `approval_keyboard`/normalization purely.
- Build the binary, run headless `packages.default`; verify it compiles + tests green.

## Non-goals
- No new loop machinery (reuse broker drain). No forced restart semantics. GUI crates
  untouched. Does not depend on praxisbot being rebuildable.
