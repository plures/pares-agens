# pares-agens Git-History Feature Audit

Scope: mine `git log --all` (831 commits, HEAD at `d98e7a5`/`f1b4890`, 2026-07-26) for
attempted-but-incomplete features, then verify each against the CURRENT source tree —
specifically whether `crates/cli/src/main.rs` can actually reach the code. Read-only;
no files modified except this report. Builds on `FEATURES.md` and `qa/RUN1-RESULTS.md`
from the prior QA pilot rather than repeating their findings.

## Method
- `git log --all --oneline -i -E --grep=...` across two grep passes (process-language:
  wip/scaffold/initial/partial/stub/prototype/in-progress/part-1; feature-language:
  hyperswarm/p2p/tauri/mobile/ios/android/discord/teams/offline/bitnet/self-improve/
  self-update/umbra/honn/shadow-learning/evolve/decision-ledger).
- For every candidate, `git --no-pager grep` on current HEAD in `crates/` to establish
  whether the symbol exists, then narrowed to `crates/cli/src/main.rs` and the two
  concrete command enums (`Commands::Serve`, `Commands::Tui`, `Commands::Migrate`) to
  determine real reachability from the shipped binary.
- No `umbra`, `honn`, `shadow-learning`, or `self-improve` hits anywhere in `git log --all`
  — those speculative terms from the task brief do not correspond to any commit in this
  repo's history. Not fabricating findings for them.

---

## 1. ModelChain / BitNet / offline-mode — unit-tested, never constructed by the CLI
**Already tracked** in `FEATURES.md` rows `offline-local-model` and `bitnet-local-model`
(filed as `pares-agens#673`, P2). This audit independently reconfirms with tighter evidence
and adds the git-history provenance the ledger row doesn't cite:

- **Attempted in:** `2e24fd0` (`feat(inference): add model auto-discovery API and canonical
  local-bitnet router config`), `91c21f7` (`feat: wire BitNet as local model client with
  --bitnet-model-path flag`), `83f5c40` (`feat(inference): add CPU BitNet expert pool with
  shared KV cache and RAM-aware scheduling (#595)`), `29ffb21` (`[WIP] Add distributed BitNet
  inference across Hyperswarm cluster (#596)`), `2641806` (`feat: GPU inference pool —
  multi-model BitNet on single GPU (#350)`), `ae47596` (`feat(inference): bitnet.cpp FFI
  bindings + build system (#330)`).
- **Current state:** `ModelChain` struct lives in `crates/core/src/model_chain.rs:12`.
  `ModelChain::new(...)` is called ONLY from that file's own `#[test]` module
  (`crates/core/src/model_chain.rs:170,181,188,199,207,214`). `git --no-pager grep -n
  "ModelChain" -- crates/cli` returns zero hits. No `--offline`/`--bitnet` flag exists on
  `Commands::Serve` or `Commands::Tui` in `crates/cli/src/main.rs` (full arg list read at
  lines 1919-2043; the only model-selection args are `model`, `deep_model`, `copilot`,
  `api_key`). **Not reachable from the binary.**
- **Referenced in FEATURES.md already:** Yes (`offline-local-model`, `bitnet-local-model`,
  both marked FAIL with the #673 filing). No change needed to the ledger for this item —
  confirms the existing finding, doesn't add a new one.
- **Recommendation:** `crates/core/src/model_chain.rs:12` — wire a `ModelChain::new(...)`
  construction path into `Commands::Serve`/`Commands::Tui` handling in
  `crates/cli/src/main.rs` (around the existing model-client construction near line
  2138-2415), gated by a new `--bitnet-model-path`/`--offline` CLI flag that mirrors the
  removed `crates/bitnet` FFI runner's old flag name. Until wired, downgrade README/CHANGELOG
  claims — this is already being tracked correctly per #673, no further ledger action needed
  beyond what's there.

## 2. Distributed BitNet-over-Hyperswarm expert pool — the whole crate was deleted, not partially wired
**New finding, not in FEATURES.md.** This is a step deeper than item 1: the distributed
inference crate that `29ffb21`/`83f5c40`/`2641806` built was removed from the workspace
entirely, not merely left unwired.

- **Attempted in:** `29ffb21` `[WIP] Add distributed BitNet inference across Hyperswarm
  cluster (#596)` — added `crates/inference/src/distributed.rs` (220 new lines: node expert
  routing + host capability advertisement), plus `crates/inference/src/expert_pool.rs`,
  `error.rs`, `lib.rs` (`git show --stat 29ffb21` confirms these paths and line counts).
  Preceded by `83f5c40` (CPU BitNet expert pool + KV cache) and `2641806` (GPU multi-model
  pool).
- **Removed in:** `f6660ab` `Remove 12 dead crates, rename migrate→cli, inline trainer
  types` — deletes `crates/arca`, `crates/autoresearch`, `crates/bitnet-sys` (later
  re-added), `crates/bitnet` (later re-added), and (per the same commit's broader diff)
  `crates/inference`. Confirmed: `Test-Path crates\inference` on current HEAD → `False`;
  current `Cargo.toml` workspace `members` (line 3) has no `crates/inference` entry — only
  `crates/bitnet-sys` and `crates/bitnet` survive from that inference-family cleanup, and
  those two contain only `classifier_backend.rs`, `model_client.rs`, `runner.rs`, `error.rs`,
  `lib.rs` (no `distributed.rs`, no expert-pool/Hyperswarm-routing code).
- **Current state:** `git --no-pager grep -n "expert_routing|ExpertRouting|distributed.*bitnet|host_capability" -- crates/`
  returns zero hits anywhere in the current tree. The distributed-expert-pool feature does
  not exist in any form today — this is a clean, intentional deletion (`f6660ab` is an
  explicit "remove dead crates" commit, not an accidental regression), so it does NOT belong
  in FEATURES.md as a "partially built" row.
- **Referenced in FEATURES.md already:** No, and it should not be added as a gap — it's
  fully absent, which is the honest state. Flagging here only so a future archaeologist
  doesn't rediscover `29ffb21` and assume live code should exist.
- **Recommendation:** No action needed. This is a correctly-completed removal, not a stub.
  If distributed BitNet inference becomes a real roadmap item again, it should be
  re-designed against the current `crates/bitnet`/`ModelChain` shape (item 1) rather than
  resurrected from `29ffb21`'s deleted `crates/inference`.

## 3. Commitment/promise detection — regex fallback, TODO explicitly says route through PxBridge (NOT in FEATURES.md)
**New finding — this is the most concrete "half-built" item, and it's currently invisible
in the feature ledger.**

- **Attempted/intended in:** design intent lives in a `commitment-detection.px` procedure
  (referenced by comment, not found as a file in the current tree — `git --no-pager grep
  -n "commitment-detection.px"` only matches the comment itself in `agent.rs`, no such `.px`
  file exists under `crates/` or a praxis directory in this checkout). PxBridge itself is
  real and used elsewhere (see below), so the intended integration point exists in principle.
- **Current state — file:line evidence:**
  - `crates/core/src/agent.rs:1907` — `async fn detect_and_store_promises(&self, _user_msg: &str, agent_reply: &str)`
  - `crates/core/src/agent.rs:1908-1915` — comment block, verbatim:
    ```
    // Decision logic lives in commitment-detection.px (via PxBridge).
    // This Rust function is the IO boundary: ...
    //
    // TODO: Route through PxBridge.call("detect_commitments", {response: agent_reply})
    // and PxBridge.call("create_tasks_from_commitments", {commitments: ...})
    //
    // Until PxBridge is wired here, use a minimal Rust fallback
    // that mirrors the .px logic (commitment-detection.px).
    ```
  - `crates/core/src/agent.rs:1917-1968` — the actual fallback: a hardcoded
    `commitment_patterns` array (`"i'll "`, `"i will "`, `"let me "`, `"going to "`) and an
    `action_verbs` array (25 verbs), doing substring/`starts_with` matching per line of the
    agent's own reply. This is a heuristic, not the PxBridge-driven design.
  - `crates/core/src/agent.rs:1207` — `self.detect_and_store_promises(content, &reply).await;`
    confirms this fallback IS called on every turn (reachable, just not the intended
    implementation).
  - Downstream consumers of the resulting `agent_promises` state key are real: `crates/core/src/heartbeat.rs:269`
    (`if let Some(promises) = self.state.get("agent_promises").await`) and
    `crates/core/src/cerebellum/actions.rs:564` (mirrors task-steering.px as a Rust fallback
    "until PxBridge wires fully" — same pattern, same caveat, in a second location).
  - PxBridge itself is real and wired elsewhere for comparison: `crates/core/src/cerebellum/px_bridge.rs`
    defines it; `crates/agens-plugin/src/agent_commands/runtime.rs:316,5273` construct
    `PxBridge::new(...)`; `crates/core/src/cerebellum/mod.rs:186,236` thread it through
    `Cerebellum`. So the bridge mechanism works — it's just not called from
    `detect_and_store_promises` or from `cerebellum/actions.rs:558` (which has the same
    "Mirrors task-steering.px logic as a Rust fallback until PxBridge wires fully" caveat at
    `crates/core/src/cerebellum/actions.rs:558`).
  - `crates/agens-plugin/src/agent_commands/runtime.rs:4785-4786` — a THIRD instance of the
    identical pattern: `// TODO: Route through PxBridge.call("evaluate_dispatch", {tick}) once // PxBridge is available in the serve path. Until then, this is a minimal ...`
- **Referenced in FEATURES.md already:** **No.** `Select-Object`/grep against `FEATURES.md`
  for "commitment", "promise", "TaskManager", "task-manager" returns zero rows. The
  task/commitment-detection system (and its two sibling "Rust fallback until PxBridge
  wires" instances) is entirely absent from the feature ledger despite being live,
  reachable, user-facing code (it feeds the 30-second heartbeat check per the doc-comment
  at `agent.rs:1904-1906`).
- **Recommendation:**
  1. Add a new FEATURES.md row, e.g. `commitment-detection` / "Task/promise detection from
     agent replies, feeds heartbeat follow-up" — status `shipped` (fallback path IS live and
     reachable), but flag explicitly that it is a **regex heuristic substituting for the
     intended PxBridge-routed design**, with a QA note that false positives/negatives in the
     substring matcher (e.g. `agent.rs:1943-1946`'s naive dedup-by-25-char-prefix) haven't
     been exercised.
  2. Either (a) actually wire `crates/core/src/agent.rs:1907` through the already-working
     `PxBridge` (construction pattern is proven at `runtime.rs:316`/`5273` — a
     `PxBridge` instance would need to be threaded into `Agent` the same way it's threaded
     into `Cerebellum` at `cerebellum/mod.rs:186`), or (b) if the regex fallback is judged
     good enough long-term, delete the three stale TODO comments (`agent.rs:1911-1912`,
     `cerebellum/actions.rs:558`, `runtime.rs:4785-4786`) so they stop reading as
     "known-incomplete" when they're actually the accepted implementation. Leaving
     TODO-with-no-ticket in three places is itself the smell the QA pilot's own guidance
     (C-NOSTUB-001) would flag if it were an internal ledger claim of "PxBridge-driven".

## 4. Self-update — fully implemented, consolidated, and wired (no gap found)
Checked specifically per the task brief's instruction to inspect
`crates/agens-plugin/src/self_update.rs`.

- **History:** `234df62` (`fix: make self-update resilient to dirty trees and wrong package
  names`), `d544a2b` (`fix: bootstrap-safe self-update via external script`), `b1edf6c`
  (`fix: restore NixOS rebuild path for self-update`), `2e62d7f` (`refactor: extract
  self-update into shared module (ADR-0010)`), `13d6cdd` (`feat: add NixOS self-update flow
  via scheduler and Telegram /update (#552)`).
- **Current state:** `crates/agenda/src/self_update.rs` (6033 bytes) is the canonical
  implementation (`build_self_update_task`, `self_update_task_from_env`, `resolve_agens_dir`,
  `build_update_command`, `DEFAULT_SELF_UPDATE_INTERVAL_SECS`). `crates/agens-plugin/src/self_update.rs`
  (1292 bytes) is a pure re-export (`pub use pares_agens_agenda::self_update::{...}` at
  lines 20-23) per ADR-0010, explicitly documented as "no logic lives here — only the
  re-export" (module doc-comment, lines 1-18). It is genuinely wired:
  `crates/agens-plugin/src/agent_commands/runtime.rs:4741` —
  `scheduler.add(crate::self_update::self_update_task_from_env()).await;` — this registers
  it on the live scheduler in the serve path. Tests reference it too
  (`runtime.rs:5876,5881`).
- **Referenced in FEATURES.md already:** No row exists for self-update specifically, but
  unlike item 3, there is no discrepancy to report — the feature is real, tested, and
  reachable. Not adding a row is a minor ledger completeness gap, not a functionality gap.
- **Recommendation:** Optional: add a `self-update` row to FEATURES.md for ledger
  completeness (status `shipped`, evidence `crates/agenda/src/self_update.rs` +
  `runtime.rs:4741`). No code change needed — this is the one candidate from the task brief
  that checked out clean.

## 5. Other WIP-language commits checked and found to be completed, not abandoned
Spot-checked because their commit-message language ("[WIP]", "Initial plan") matched the
grep patterns but their actual diffs turned out to be finished features, to avoid
false-positive reporting:

- `02f606f` `[WIP] Add reasoning toggle for deep model escalation (#597)` — multi-commit
  squash merge ending in `feat(telegram): add reasoning toggle for deep model escalation`;
  not investigated further beyond confirming the merge commit is a completed PR (own commit
  history shows Initial plan → implementation → done), consistent with `94faff2`
  (`feat(models): real /models command + routing-aware /status`) landing afterward in the
  same area. No dangling half-feature found.
- `5d1f5cd` `wip: save work in progress` — a raw, unsquashed "wip" commit on `main` (not a
  PR title) sitting in history near the `AgensRuntime`/event-spine work
  (`b84b5ea`, `3a403e9`, `fe411c4`, `5200c99`). Immediately followed by
  `fe411c4 fix: wire event spine into TelegramAdapter + spawn heartbeat runner` and
  `5200c99 fix: remove --no-event-spine flag — spine is mandatory infrastructure` — the
  event spine is mandatory/always-on today per `5200c99`'s own message, so this WIP commit
  was absorbed into completed work, not left dangling. No further action.
- `2a2a950` `WIP on main: 685d6c4 ...` — this is a `git stash`-style auto-generated WIP
  commit message (stash-on-branch pattern), not a feature branch; artifact of local
  workflow, not a shipped-but-incomplete feature. Ignore.

## 6. Terms with zero hits (explicitly checked, explicitly absent)
Per the task brief's suggested search terms, the following returned **zero commits** in
`git log --all` and zero source hits — not fabricating gaps for them:
`umbra`, `honn`, `shadow-learning`, `self-improve` (as a literal token; `self-update` is the
real, implemented feature per item 4, and is a different concept — no "self-improvement"/
autonomous-code-modification feature was ever attempted in this repo's history).

---

## Summary Table

| Feature | Attempted (commit) | Current state | In FEATURES.md? | Recommendation |
|---|---|---|---|---|
| ModelChain/BitNet/offline | `2e24fd0`,`91c21f7`,`83f5c40`,`29ffb21`,`ae47596` | Unit-tested only, never constructed by CLI (`model_chain.rs:12`, zero hits in `crates/cli`) | Yes (2 rows, #673 filed) | No new action — already tracked |
| Distributed BitNet/Hyperswarm expert pool | `29ffb21`,`83f5c40`,`2641806` | Crate `crates/inference` deleted wholesale in `f6660ab`; zero trace in current tree | No (correctly absent) | None — clean deletion, not a stub |
| Commitment/promise detection via PxBridge | comment-only intent, no commit adds the missing wiring | Live regex fallback (`agent.rs:1907-1968`), reachable (`agent.rs:1207`), 3 separate TODO-PxBridge stubs across `agent.rs`/`cerebellum/actions.rs`/`runtime.rs` | **No — missing entirely** | Add FEATURES.md row; wire PxBridge or retire the TODOs |
| Self-update | `234df62`,`d544a2b`,`b1edf6c`,`2e62d7f`,`13d6cdd` | Fully implemented, consolidated (ADR-0010), wired at `runtime.rs:4741` | No row (but no gap) | Optional ledger row only |

Generated 2026-07-26 by read-only git-history audit subagent. No source files modified.
