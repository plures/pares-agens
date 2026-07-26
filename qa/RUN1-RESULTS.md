# QA Pilot Run 1 — Final Results (2026-07-26)

**Build:** `pares-agens.exe` release binary, commit `f1b4890` (base v1.59.5)
**Method:** `qa-task-suite-runner` skill — real CLI execution via `serve --stdio --copilot`, one fresh
process per task (simulating new session), transcripts captured to `qa/transcripts/`.
**Blocker resolved:** RUN1 found and fixed the channel-agnostic headless gap (pares-agens#672,
merged in `f1b4890`) that had blocked all execution. This document covers the actual T1–T16 run
that followed.

## Results summary

| Task | Result | Notes |
|---|---|---|
| T1 model-routing | PASS (minor UX gap) | Routed to configured Copilot model correctly; model can't self-report which model it is |
| T2 plureslm-memory | PASS | Recall correct; negative case honestly declined, no fabrication |
| T3 reactive-procedures | INCONCLUSIVE | No concrete named procedure exists to test against; honest "no predefined procedure" response, not a fabrication, but claim unverifiable via CLI as specified |
| T4 decision-ledger/approval gates | **FAIL (partial)** | Model refused destructively-worded request, but `tool_calls=0` in transcript — no system-enforced gate exercised. Filed **pares-agens#674** (P1, safety-relevant) |
| T5 cross-platform-native | NOT RUN | Explicitly out of scope for CLI/MCP runner (needs UI-testing standard) |
| T6 offline-local-model | **FAIL** | No offline flag/config anywhere in `crates/cli`; `ModelChain` never constructed by CLI. Filed **pares-agens#673** (P2) |
| T7 p2p-sync | NOT RUN | Requires two live instances, not exercised this pass |
| T8 cross-platform-nodes | **FAIL/unverified** | No node-registry awareness surfaced to the model at all |
| T9 status-tool-count-fix | PASS | Reported 13 tools; matches live registry log `tool_count=13` exactly |
| T10 headroom-compression | **PARTIAL/finding** | No compression event fired for a single large (2700-word) message; compression only observed across accumulated multi-turn context (T1/T2). The literal "single large message compresses" case in the task spec is unconfirmed |
| T11 skill-discovery-parity | PASS | Listed 13 real tools by name; matches live tool_count |
| T12 tui | NOT RUN | Out of scope for CLI/MCP runner |
| T13 bitnet-local-model | **FAIL** | Real bitnet client/ModelChain code exists and is unit-tested, but unreachable from the CLI — same root cause as T6. Covered by **pares-agens#673** |
| T14 discord-adapter | PASS (honest absence) | No Discord code anywhere; correctly not advertised as present |
| T15 teams-adapter | PASS (honest absence) | No Teams code anywhere |
| T16 approval-card-parity | PASS (honest current state) | Inline-keyboard approval cards exist only in `telegram.rs`; no fake cross-channel claim found |

## Score
- **PASS:** 7 (T1, T2, T9, T11, T14, T15, T16)
- **FAIL:** 3 (T4, T6, T13) — 2 real GitHub issues filed (#673 covers T6+T13, #674 covers T4)
- **PARTIAL/finding:** 2 (T10 headroom scope question, T3 unverifiable-as-specified)
- **NOT RUN:** 4 (T5, T7, T12 — explicitly out of CLI/MCP scope per task design; T8 assessed but effectively a fail)

## Real bugs found and filed (not just documented)
1. **pares-agens#673** — `ModelChain` (bitnet/offline fallback) built + unit-tested, never wired into the CLI. Two FEATURES.md rows (`offline-local-model`, `bitnet-local-model`) are marked `shipped` but are dead code in the running application.
2. **pares-agens#674** — Approval/decision-ledger gate for destructive actions is model-judgment-only (`tool_calls=0` observed), not a system-enforced Praxis constraint. Safety-relevant gap.

## What this proves about the SDLC/QA refactor
- The task-based, machine-invocable QA approach (per `qa-task-suite-runner`) surfaced two genuine,
  previously undetected dev-stage bugs that `cargo test --workspace` (564/564 passing) did not catch —
  because unit tests validated `ModelChain` in isolation, not its absence from the actual wiring.
- Honest-absence checks (T14/T15/T16) confirm C-NOSTUB-001 compliance is being respected — no fake
  Discord/Teams/ApprovalCard code exists pretending to be more complete than it is.
- FEATURES.md's own "unknown"/re-checked verification flags predicted exactly where the real gaps were
  (T6, T8, T13 all had `unknown` or thin-verification notes going in) — validating the ledger-first
  approach: the ledger told us where to look before we even ran the tests.

## Next steps
- Update `FEATURES.md` `last_qa_result` column — **done**, this pass.
- Hand transcripts to `qa-transcript-rating` (independent judge) for structured severity/confidence
  scoring — recommended before treating this as release-gating for a formal cut.
- T4 (#674) is flagged **P1** and arguably should block any near-term public pre-release/release
  involving decision-ledger claims, given its safety framing.
