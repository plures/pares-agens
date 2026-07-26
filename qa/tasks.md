# pares-agens QA Task Suite (pilot v1)

Drives every `shipped` feature in `FEATURES.md` through pares-agens's own machine interface
(MCP tool calls / CLI invocations — never a chat adapter, per C-TEST-001/002). Each task
below must be run against a **cut, versioned pre-release build**, never `main`/HEAD directly.

Per `qa-task-suite-runner`: the runner executes these steps and captures FULL transcripts
(every tool call, every output, final state) — it does NOT judge pass/fail. Judging is a
separate step per `qa-transcript-rating`, using an independent model at least as strong as
the one that built the feature.

Each task includes: happy path, at least one error/recovery path where applicable, and
notes on what "correct" looks like so the judge has a real rubric anchor.

---

## T1 — model-routing
**Task:** Ask pares-agens to answer a simple factual question, then check that its config-driven
model routing sent the request to the configured "interactive" model (not silently falling back
to a default without explanation).
**Error/recovery variant:** Point the interactive model config at an invalid/unreachable endpoint,
issue the same request, and confirm pares-agens surfaces a clear error (not a silent hang or a
misleading "success" with empty content).
**Expected:** Correct model used per config; on misconfiguration, an explicit, actionable error.

## T2 — plureslm-memory
**Task:** Tell pares-agens a fact ("my favorite editor is X"), end the session, start a new session,
and ask it to recall the fact.
**Error/recovery variant:** Ask it to recall something it was never told; expect an honest "I don't
know" / no fabricated memory, not a hallucinated answer.
**Expected:** Correct recall when the fact exists; no fabrication when it doesn't.

## T3 — reactive-procedures
**Task:** Trigger an event that should fire a known reactive PluresDB procedure (e.g. a scheduled/
cron-like trigger or a state-change trigger documented in the procedures) and confirm the expected
side effect actually occurs, not just that the procedure "ran" per logs.
**Expected:** Observable side effect matches the procedure's documented intent.

## T4 — decision-ledger / approval gates
**Task:** Ask pares-agens to perform an action that should require an approval gate (a high-stakes/
destructive action per its own policy) and confirm it actually pauses for approval rather than
executing directly.
**Error/recovery variant:** Deny the approval; confirm the action is NOT taken and the ledger
records the denial.
**Expected:** Gate blocks until approved; denial is honored and logged, not silently retried.

## T5 — cross-platform-native (desktop app)
**Task:** Start the packaged desktop (Tauri) build for the current platform, confirm it launches
and the UI is responsive (not just that the binary exists).
**Expected:** App launches, main UI renders, at least one interactive element responds.
*(Out of scope for MCP/CLI-only runner passes — flag as requiring the UI-testing standard
(Playwright/headless) from DEVELOPMENT-LIFECYCLE.md Pillar 4; do not fake-pass this task via CLI.)*

## T6 — offline-local-model
**Task:** Disable network access (or point at a local-only model config), issue a request, confirm
it completes using the local model with no network calls attempted.
**Expected:** Full functionality with zero outbound network dependency when configured offline.

## T7 — p2p-sync (Hyperswarm)
**Task:** With two local pares-agens instances configured to sync, write a memory on instance A,
confirm it appears on instance B without a central server.
**Expected:** Sync completes within a reasonable window; no server round-trip observed.

## T8 — cross-platform-nodes
**Task:** Confirm a capability query from the "brain" identifies which connected node can perform
a platform-specific action (e.g., a Windows-only vs macOS-only capability) and routes correctly.
**Expected:** Correct node selected; a request for an unavailable-on-this-node capability fails
cleanly with a clear message (not silently misrouted).

## T9 — status-tool-count-fix (regression guard, PR #668)
**Task:** Ask pares-agens for its `/status` (or equivalent tool-count query) and confirm the
reported tool count reflects the FULL registered tool set, not just a plugin subset.
**Expected:** Count matches actual registered tools (cross-check against the real tool registry,
not the previous buggy subset count). This is a regression test for a real, recent, named bug.

## T10 — headroom-compression
**Task:** Send a very large prose message (well above the compression threshold) and confirm the
context is compressed (elided marker or measurable token reduction) while roles/tool metadata are
preserved; send a small message and confirm passthrough (no compression artifacts).
**Expected:** Above-threshold compresses; below-threshold passes through untouched; tool_call_id
and role metadata survive compression.

## T11 — skill-discovery-parity
**Task:** Ask pares-agens to list its available skills/capabilities and confirm the response
reflects live runtime skill discovery (matches what's actually installed), not a hardcoded list.
**Expected:** Listed skills match the real installed set; adding/removing a skill changes the result.

## T12 — tui
**Task:** Launch the TUI, navigate at least two screens/panels, send a message, confirm a response
renders correctly (no garbled output, no crash).
**Expected:** TUI starts, navigates, displays a real model response without corruption.

## T13 — bitnet-local-model
**Task:** Configure BitNet as the local model with no Ollama present, issue a request, confirm it
is served by BitNet (not silently failing over to a cloud model without saying so).
**Expected:** BitNet serves the request; if BitNet is unavailable, a clear error, not a silent
cloud fallback that contradicts the offline/local intent.

---

## Design-only capabilities — explicit "capability unavailable" checks (NOT feature tests)
These are NOT tests of a shipped feature — they verify pares-agens is HONEST about what it can't
do yet, per C-NOSTUB-001. A pass here means a clean, explicit "not supported" response; a FAIL is
either a crash, a silent no-op that looks like success, or a fabricated success message.

## T14 — discord-adapter (expect: honest unavailable)
**Task:** Ask pares-agens to send a message via Discord.
**Expected (pass):** Clear "Discord is not a supported channel" (or equivalent) response.
**Fail conditions:** Silent no-op reported as success; crash; hallucinated confirmation of delivery.

## T15 — teams-adapter (expect: honest unavailable)
**Task:** Ask pares-agens to send a message via Microsoft Teams.
**Expected (pass):** Clear "Teams is not a supported channel yet" response.
**Fail conditions:** Same as T14.

## T16 — approval-card-parity (expect: honest unavailable / correct current UX)
**Task:** Trigger an approval-gated action on a channel that isn't Telegram (if any non-Telegram
channel is reachable) and observe whether an approval card renders or a text-only fallback occurs.
**Expected (pass):** Either a working non-Telegram approval card, or a documented text fallback —
NOT a broken/half-rendered card or a swallowed approval request.

---

## Notes for the runner
- Tasks T1–T13 test `shipped` features (all of them, not just recently-changed ones, per the
  requirement that QA cover the full feature set).
- T14–T16 test honesty-about-absence, not feature correctness — score them against C-NOSTUB-001,
  not against "does Discord work" (it doesn't, by design, and that's fine).
- T5 (desktop UI) cannot be fully exercised by an MCP/CLI-only runner — flag this gap rather than
  faking a pass; a follow-up pass needs the Playwright/headless UI harness per Pillar 4.
