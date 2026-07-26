**Problem:** pares-agens has a real, deliberately-built integration seam for consuming pares-umbra's evolved shadow-learning `.px` procedures, but the consuming side was never finished — it loads and discards the data instead of feeding it into an evaluation arena.

**Evidence (file:line):**
- `crates/agens-plugin/src/agent_commands/runtime.rs:4396-4420` loads `praxis/shadow/*.px` into a `ShadowProcedures` holder (type defined in the pares-radix repo: `pares_radix_core::spine::shadow::ShadowProcedures`).
- `runtime.rs:4404` binds the result to `let _shadow_procedures = { ... }` (leading underscore = constructed but effectively unused) and logs `"loaded umbra-evolved shadow candidates from praxis/shadow/ (inert; not live)"` (`runtime.rs:4416`) — then the data is dropped. No fitness scoring, no arena feed, no promotion path exists anywhere in pares-agens.
- pares-radix's own `shadow.rs` doc comment (in the pares-radix repo, not pares-agens) is explicit about this exact gap: "The evolutionary arena and fitness accounting live in umbra (`umbra_shadow::ShadowArena`, `umbra_fitness`), not here... The eventual integration wires these loaded candidates into a `umbra_shadow::ShadowArena`... Until that wiring lands, the holder is the stable seam." That wiring does not exist in pares-agens (confirmed) or, per this investigation, in pares-radix either.
- Separately, pares-agens's own BitNet usage (`crates/agens-plugin/src/agent_commands/bitnet_classifier.rs` + `crates/bitnet`) is a plain static-model inference wrapper with **zero** evolutionary/HONN logic and **zero** dependency on any `umbra-*` crate — confirmed fully independent from pares-umbra's `umbra-bitnet` evolutionary system, not overlapping/duplicated work.
- No `pares-agens` crate depends on any `umbra-*` crate today (checked all `Cargo.toml` files) — the shadow-loader seam consumes only the `.px` text artifact, not umbra's Rust API.

**Impact:** pares-umbra's nightly shadow-learning training on praxisbot (`nixos-config/hosts/praxisbot/umbra-train.nix`) produces real evolved `.px` procedures, but pares-agens has no live mechanism to actually evaluate, score, or promote them — self-improvement is currently a one-way, inert data drop, not a functioning feedback loop.

**Proposed fix (see full phased proposal in `qa/PARES-UMBRA-INTEGRATION-PROPOSAL.md` in this repo for details):**
- **Phase A** (highest priority): replace the `let _shadow_procedures = ...` dead-end at `runtime.rs:4404` with a real consumer that constructs a `umbra_shadow::ShadowArena`, feeds loaded candidates real traffic, persists fitness/promotion signals, and exposes arena status via a new operator-facing command (e.g. `/shadow-status`). Requires adding `umbra-shadow` as a pares-agens dependency (note: pares-umbra is BSL-1.1 licensed — confirm license compatibility before adding the dependency).
- **Phase B**: expose an on-demand `evolve-*` trigger to pares-agens operators (shell out to the existing `umbra` binary's `EvolveRouter`/`EvolvePriority`/`EvolveClassifier` subcommands) instead of only running nightly on praxisbot against shared data.
- **Phase C** (exploratory, longer-term): let `bitnet_classifier.rs` consume an umbra-evolved `EvolvableProcedure` instead of its fixed hand-written inference path.
- Given this repo's safety-first governance (approval gates, no-stubs), promotion of a shadow procedure to live should require an explicit approval command, not fully automatic hot-reload, at least until the promotion protocol's safety constraints have test coverage.

**Uncertainty (explicitly flagged, not resolved by this issue):** the exact mechanism that promotes `.px` files from the private `pares-umbra-data` repo into `pares-radix`/`pares-umbra`'s tracked `praxis/shadow/` directory was not found in this investigation — may be manual or may exist in a script/Action not covered. Needs confirming before Phase A work starts.

**Evidence source:** integration-proposal subagent report, `qa/PARES-UMBRA-INTEGRATION-PROPOSAL.md`.

**Priority:** P2 — self-improvement is a strategically important feature per the user's direction, but the fix requires cross-repo design decisions (license check, arena API surface, promotion-approval UX) before implementation, so it's a design/planning priority, not an immediate code fix.
