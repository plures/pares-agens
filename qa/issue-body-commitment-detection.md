**Problem:** Commitment/promise detection (the mechanism that turns an agent reply like "I'll fix X" into a real task) is implemented as a fragile, hand-written regex heuristic instead of the intended Praxis-routed (PxBridge) design.

**Evidence (file:line):**
- `crates/core/src/agent.rs:1907` — `async fn detect_and_store_promises(...)`.
- `crates/core/src/agent.rs:1908-1915` — comment states the real design intent verbatim: "Decision logic lives in commitment-detection.px (via PxBridge)... TODO: Route through PxBridge.call(\"detect_commitments\", ...) ... Until PxBridge is wired here, use a minimal Rust fallback."
- `crates/core/src/agent.rs:1917-1968` — the actual fallback: a hardcoded 4-pattern list (`"i'll "`, `"i will "`, `"let me "`, `"going to "`) combined with a fixed 25-verb allowlist, matched via `starts_with`/substring on each line of the agent's own reply. No fuzzy matching, no semantic detection — phrasing that doesn't exactly match is silently dropped with no fallback and no user-visible signal.
- No `commitment-detection.px` file exists anywhere in the repo (`git grep -n "commitment-detection.px"` only matches the comment referencing it — the intended .px procedure was never written).
- Two sibling instances of the identical "Rust fallback until PxBridge wires fully" pattern exist elsewhere with the same caveat, never resolved: `crates/core/src/cerebellum/actions.rs:558` and `crates/agens-plugin/src/agent_commands/runtime.rs:4785-4786`.
- PxBridge itself is real, tested, and wired elsewhere (`crates/core/src/cerebellum/px_bridge.rs`; constructed at `runtime.rs:316,5273`; threaded through `Cerebellum` at `cerebellum/mod.rs:186,236`) — so the intended integration point is provably reachable, it's just never connected to promise detection.

**Impact:** Real-world phrasing variance causes silent, undetected task-creation failures (confirmed live: natural phrasing like "I'm going to need you to do three things..." produced zero detected promises, while only exact "I will <verb>..." phrasing matched). This compounds the task-manager wiring bug (#675) — even after #675's fix, many genuine commitments will still be silently dropped by this heuristic.

**Proposed fix:** Either (a) write the intended `commitment-detection.px` procedure and route `detect_and_store_promises` through the already-working `PxBridge` (construction pattern proven at `runtime.rs:316`/`5273`), threading a `PxBridge` into `Agent` the same way it's threaded into `Cerebellum`; or (b) if the regex fallback is accepted as the permanent design, remove the three stale "TODO: route through PxBridge" comments so they stop reading as known-incomplete work, and add a FEATURES.md ledger row disclosing the heuristic nature and its known false-negative risk.

**Evidence source:** git-history feature audit subagent report, `qa/GIT-HISTORY-FEATURE-AUDIT.md`, section 3.

**Priority:** P1 — directly compounds the user-reported task-forgetting bug (#675) and has no ledger visibility today.
