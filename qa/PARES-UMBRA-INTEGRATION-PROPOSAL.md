# pares-umbra ↔ pares-agens Integration Proposal

**Status:** Read-only investigation, no code changed.
**Author:** subagent investigation, 2026-07-26
**Scope:** How self-improvement/shadow-learning (pares-umbra) should be exposed to pares-agens.

---

## 1. What pares-umbra actually does (evidence)

Source: `C:\Projects\pares-umbra\README.md` (full read).\n\n- Workspace members (`C:\Projects\pares-umbra\Cargo.toml:4-10`): `umbra-core`, `umbra-shadow`,
  `umbra-honn`, `umbra-fitness`, `umbra-bitnet`, `umbra-cli`.
- **Shadow learning**: a live procedure serves the user; shadow procedures run silently on the
  same input; a `Fitness Evaluator` compares outputs; a shadow promotes to live when it
  consistently outperforms (README "Architecture" + "Promotion Protocol" sections).
- **HONN (Higher-Order Neural Network)**: instead of scalar float weights, "neurons" are `.px`
  Praxis expressions. Training = evolutionary search (mutation/crossover/selection) over `.px`
  AST, not gradient descent (README "Key Concepts").
- **umbra-bitnet crate** (`crates\umbra-bitnet\src\lib.rs:11-35`) re-exports:
  - `BitLinearLayer` (ternary-weight matmul, `bitlinear.rs`)
  - `EvolvableProcedure`, `evolve_procedure` (`evolve.rs:1-390`) — evolves the **inference
    algorithm** (activation fn, layer order, skip probabilities, normalization, attention temp,
    residual scale, output strategy) while keeping BitNet **weights fixed**
    (`evolve.rs:9-13`: "What evolves: the PROCEDURE ... What stays fixed: the WEIGHTS").
  - `ExpertBank`, `evolve_expert_bank` (`expert_bank.rs`)
  - `corpus_cross_entropy`, `corpus_perplexity_fitness`, `split_corpus` (`fitness.rs`)
  - `BitNetInference`, `InferenceInput/Output` (`inference.rs`)
  - `load_model`/`save_model`(+`_auto`,`_json`), `load_expert_bank`/`save_expert_bank`
    (`model_io.rs`)
  - `train_on_text`, `TrainingConfig`, `TrainableModel` (`training.rs`)
  - `BitNetConfig`, `BitNetModel`, `TransformerLayer` (`transformer.rs`)
  - `TernaryMatrix`, `TernaryWeight` (`weights.rs`)
  - Each `EvolvableProcedure` can serialize itself to a **readable `.px` procedure** via
    `to_px()` (`evolve.rs:305-393`) — this is the literal artifact that later becomes a `.px`
    file shipped downstream.
- **CLI surface** (`crates\umbra-cli\Cargo.toml:8-10`: `[[bin]] name = "umbra"`,
  `path = "src/main.rs"`). Confirmed subcommands via `clap::Subcommand` enum `Commands` in
  `crates\umbra-cli\src\main.rs`, including at minimum: `EvolveClassifier`, `EvolvePriority`,
  `EvolveRouter`, plus a nested bitnet subcommand group with `Evolve` and `TrainEvolve`
  ("Train weights, then evolve the procedure — the full self-improvement loop").
- **umbra-shadow crate**: `ShadowArena` (`crates\umbra-shadow\src\arena.rs`) — `pub struct
  ShadowArena`, `pub fn new(config: ShadowConfig)`, plus `ArenaStatus`/`TickOutcome` types.
  This is the actual "run shadow procedures against real traffic, accumulate fitness" engine
  referenced by pares-radix's shadow.rs doc comment (see §3). I did **not** fully enumerate its
  tick/feed API surface (uncertain — file is 15KB, only grepped for `pub fn`/`pub struct`
  signatures, not read line-by-line); further inspection would be needed before designing a
  live integration against it.

## 2. How it's invoked today on praxisbot (evidence)

Source: `C:\Projects\nixos-config\hosts\praxisbot\umbra-train.nix` (full read).\n\n- **PRAXISBOT-ONLY** — comment block at top explicitly forbids adding this module to any other
  host (surface, air, wsl-*, other pares-radix/pares-agens machines) (lines 1-5).
- Nightly systemd timer `umbra-train-offload`, `OnCalendar = "05:30"`, `RandomizedDelaySec =
  "30min"` (near bottom of file).
- The offload script (`offloadScript`, `pkgs.writeShellScript`) invokes the `umbra` binary from
  the pares-umbra flake package with EXACTLY three evolve commands and one bitnet-train command:
  ```
  umbra evolve-router    --generations 300 --population 120 --export-px .../route_message.px
  umbra evolve-priority  --generations 300 --population 120 --export-px .../score_priority.px
  umbra evolve-classifier --generations 300 --population 120 --export-px .../classify_intent.px
  umbra bitnet train-text --size small --file corpora/proverbs.txt --epochs 20 --save .../bitnet-small-$TS.bin
  ```
- **Output flow (as documented in the file's own header comment, lines 22-27):** evolved `.px` +
  model snapshots + run metadata are committed to the **PRIVATE** `plures/pares-umbra-data` repo
  (NOT pares-agens, NOT pares-radix directly). The header explicitly states: "The evolved .px
  that this produces flow back to the running pares-radix service via the normal package path:
  commit evolved .px into pares-umbra/pares-radix -> autoUpgrade (--update-input) rebuilds the
  package -> preStart syncs ~/praxis. **This module's job is the DATA/TRAINING side; deployment
  of procedures into the live runtime is handled by the pares-radix package sync, not by this
  timer.**"
- **Conclusion: there is a manual/separate step (not visible in this file) that must copy `.px`
  files from `pares-umbra-data` into `pares-umbra`/`pares-radix`'s `praxis/shadow/` tree before
  they reach a running agent.** This file only produces artifacts in a private data repo; it does
  not itself commit into pares-radix or pares-agens. (Marked uncertain: I did not find that
  promotion-copy step in this investigation — it is out of scope of the files I was asked to
  read. It may be manual, or may be another nix module/script not covered here.)

## 3. Existing overlap in pares-agens — CONFIRMED, not speculation

There IS already a live wiring point, but it is entirely **passive/inert** on the pares-agens
side. Two independent, non-overlapping things exist:

### 3a. `praxis/shadow/` loader — genuinely wired to umbra's output format

- `C:\Projects\pares-agens\crates\agens-plugin\src\agent_commands\runtime.rs:4396-4420` loads
  `praxis/shadow/*.px` into a `ShadowProcedures` holder from
  `pares_radix_core::spine::shadow::ShadowProcedures` — **this type lives in the pares-radix
  repo**, not pares-agens (`C:\Projects\pares-radix\crates\radix-core\src\spine\shadow.rs`).\n- The pares-radix `shadow.rs` doc comment (lines 1-27) is explicit and self-aware about the gap:
  > "The evolutionary arena and fitness accounting live in **umbra** (`umbra_shadow::ShadowArena`,
  > `umbra_fitness`), not here. pares-radix must not host a second evolutionary engine. This
  > holder is intentionally thin: it only *loads* candidates and exposes them. **The eventual
  > integration wires these loaded candidates into a `umbra_shadow::ShadowArena`** (fed the same
  > real traffic the live classifier sees) which accumulates fitness and signals promotion.
  > **Until that wiring lands**, the holder is the stable seam."
- `runtime.rs:4404` binds this into `let _shadow_procedures = { ... }` — note the **leading
  underscore**, i.e. Rust convention for "constructed but (mostly) unused." It is loaded, logged
  (`"loaded umbra-evolved shadow candidates from praxis/shadow/ (inert; not live)"`,
  `runtime.rs:4416`), and then dropped/discarded — no fitness scoring, no arena feed, no
  promotion path exists on the pares-agens side. This confirms the pares-radix doc comment: the
  loader is real, the arena wiring is NOT implemented anywhere in either repo I inspected.
- **This is NOT duplication with umbra — it is a genuine, intentional integration seam that was
  built but left half-finished.** The consuming side (feeding real traffic into
  `umbra_shadow::ShadowArena`, scoring fitness, signaling promotion) does not exist in
  pares-agens.

### 3b. `agens-plugin/src/agent_commands/bitnet_classifier.rs` — separate, unrelated system

- `BitNetClassifier` (`bitnet_classifier.rs:1-40`) wraps `pares_agens_bitnet::BitNetRunner` (crate
  `pares-agens-bitnet`, `C:\Projects\pares-agens\crates\bitnet`, description: "Safe Rust wrapper
  for bitnet.cpp - local BitNet CPU inference for Pares Agens",
  `crates\bitnet\Cargo.toml`).
- It does single-token classification prompts (intent/complexity/needs-tools) against a
  **loaded, static, already-trained** BitNet model via FFI to `bitnet.cpp` (native C++ inference,
  feature-gated `inference`/`model-client`/`classifier`, `crates\bitnet\src\lib.rs:33-49`).
- There is **no evolutionary/HONN logic anywhere in `pares-agens-bitnet`** — no `evolve`, no
  `.px` output, no fitness/mutation/crossover. It is a plain forward-inference wrapper.
- `pares-umbra`'s `umbra-bitnet` crate is architecturally similar in one respect only (both work
  with BitNet ternary-weight models), but umbra's crate additionally hosts the evolutionary
  procedure-search layer (`evolve.rs`) that has **no counterpart** in `pares-agens-bitnet`.
- **Confirmed: these are two independent, non-integrated BitNet usages.** No shared code, no
  shared crate dependency (`pares-agens-bitnet` does not depend on any `umbra-*` crate — checked
  `crates\bitnet\Cargo.toml`; only dependency direction possible would be umbra depending on
  pares-agens crates, and umbra's `Cargo.toml` workspace deps list no `pares-agens-*` crate
  either). This is NOT a duplication that needs deduplicating (ADR-0010 sense) — it is simply two
  systems that happen to both touch "BitNet" and have never been connected.
- Grep evidence: searching all `*.rs` under `pares-agens\crates` for the literal string `umbra`
  found exactly one file with hits — `runtime.rs` (the shadow loader in §3a). No other file in
  pares-agens references umbra, shadow-learning, or HONN.

## 4. Concrete integration proposal

Given the above, the natural, lowest-risk next step is **not** a new subsystem — it's **finishing
the wiring that pares-radix's own `shadow.rs` doc comment already specifies**, then exposing it
to pares-agens users via a thin command surface. Proposed phases:

### Phase A — Finish the seam that already exists (highest priority, least new surface)
- Add a real consumer in `pares-agens` (likely in `agens-plugin/src/agent_commands/runtime.rs`,
  replacing the `let _shadow_procedures = ...` dead-end at line 4404) that:
  1. Constructs a `umbra_shadow::ShadowArena` (new dependency: `pares-agens` would need to depend
     on the `umbra-shadow` crate — currently ZERO umbra crates are a dependency of any
     pares-agens `Cargo.toml`; this must be added and evaluated for license/repo-boundary
     implications, since `pares-umbra` is BSL-1.1 licensed per its README).
  2. Feeds each loaded `ShadowCandidate` (from `ShadowProcedures::candidates()`,
     `shadow.rs:157-165`) into the arena alongside real user turns already flowing through the
     live procedure registry.
  3. Persists fitness/promotion signals somewhere durable (PluresDB, per the umbra README's
     "Relationship to Plures Ecosystem" table) rather than discarding them — currently there is
     no accumulation at all.
  4. Exposes arena status via a new `/shadow-status` (or similar) chat command so users/operators
     can see "N shadow candidates loaded, fitness scores X, closest to promotion Y" instead of
     the current silent inert log line.
- **Uncertain / needs design decision:** whether promotion should be fully automatic (write a new
  `.px` into the live `praxis/` tree and hot-reload) or require an explicit approval step (a
  `/promote-shadow <name>` command). Given this workspace's "NO STUBS" and safety-first
  governance culture (AGENTS.md), an explicit-approval promotion command is the safer default
  until the promotion protocol's constraints (README "Promotion Protocol": fitness threshold,
  no catastrophic failures, complexity bounds, Praxis safety constraints) have test coverage.

### Phase B — Expose evolve-on-demand to pares-agens operators (medium priority)
- A `pares-agens` CLI/chat command (e.g. `agens self-improve evolve-router`) that shells out to
  (or FFI-links, if brought in-process) the `umbra` binary's existing `EvolveRouter` /
  `EvolvePriority` / `EvolveClassifier` subcommands (confirmed to exist,
  `crates\umbra-cli\src\main.rs`) against the operator's own recent traffic, rather than only ever
  running nightly on praxisbot against the shared corpus. This turns self-improvement from
  "something that happens to the fleet centrally" into "something a user can trigger for their
  own instance," matching the umbra README's stated design goal: "Distributed across consumer
  hardware — each node contributes shadow cycles."
- This requires either (a) vendoring/depending on `umbra-cli`/`umbra-honn` as a library instead of
  shelling out to a separate binary, or (b) a documented subprocess contract (find `umbra` on
  PATH, run with `--export-px`, then trigger the Phase A shadow-loader to pick up the new file).
  Option (b) is lower-risk and requires zero new build-time coupling between the two Cargo
  workspaces.

### Phase C — BitNet unification (lowest priority, exploratory)
- Given §3b's confirmed finding that `pares-agens-bitnet` and `umbra-bitnet` are fully separate
  with no evolutionary logic in the former, there is a longer-term opportunity (not a near-term
  requirement) to let `pares-agens-bitnet`'s classifier consume an `EvolvableProcedure` evolved by
  umbra (via `evolve_procedure`/`to_px()`, `evolve.rs:305-393`) instead of a fixed hand-written
  inference path. This is speculative and would need its own design spike — flagging it as
  future work, not a proposal to build now.

## 5. What is NOT proposed
- No duplication of umbra's evolutionary engine inside pares-agens (would violate the
  architecture seam pares-radix's own `shadow.rs` doc explicitly warns against: "pares-radix must
  not host a second evolutionary engine" — the same constraint should apply to pares-agens).
- No change to `bitnet_classifier.rs` or `self_update.rs` proposed here — both are out of scope
  for this integration (self_update.rs is unrelated: it is a re-exported package/binary
  self-update mechanism per its own doc comment, `self_update.rs:1-18`, nothing to do with
  learning/evolution).

## 6. Explicit uncertainty ledger
- **Unconfirmed:** the exact mechanism/script that promotes `.px` files from the private
  `pares-umbra-data` repo into `pares-umbra`/`pares-radix`'s tracked `praxis/shadow/` directory —
  not found in the files reviewed for this task.
- **Unconfirmed:** full API surface of `umbra_shadow::ShadowArena` beyond the `pub fn new` /
  struct signatures grepped (`ArenaStatus`, `TickOutcome` fields not read in detail).
- **Unconfirmed:** whether any manual promotion/copy step already exists elsewhere in the plures
  org (e.g. a GitHub Action) that I did not have access to check within scope.
- **Confirmed with high confidence:** the `praxis/shadow/` load-and-discard seam in
  `runtime.rs:4396-4420`/`shadow.rs` is real, deliberate, and the single existing integration
  point — every other claim of "zero integration" in the task prompt is accurate for the rest of
  the pares-agens codebase (verified via full-repo grep for `umbra`).
