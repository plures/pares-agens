# MIGRATION-PLAN.md — pares-agens ↔ pares-radix v1.49.2 → v1.55.13

**Stage:** S1 ANALYZE (read-only). This document is the implementation spec for S3.
**Author:** S1 analyze subagent, 2026-07-01.
**Radix ground-truth checkout:** `C:\Users\kbristol\.cargo\git\checkouts\pares-radix-e42d2bf425d083f6\78a4936\` (tag `v1.55.13`).
**Breaking commit:** `3172cfa refactor(radix-core)!: B1 S-B … de-cognition cli-runtime/cli-api` — removed crates `pares-radix-cli`, `pares-radix-cli-api` (and `cli-runtime`, `migrate`, `mcp-server`).

---

## 0. TL;DR / verdict

**This is a real re-architecture of the command-host seam, NOT a 1:1 rename.** But the blast radius is small and well-contained:

- The removed CLI plugin model (`CommandProvider` / `ProviderOutcome` / `ProviderRegistry` / `CommandError` / `CommandResult` from `pares_radix_cli_api`) **has no successor trait**. ADR-0022/0024's new "capability plugin" model is a **TOML-manifest + TS/Svelte (or WASM) VSCode-style plugin** installed via `pares-modulus`; it is **not** a Rust CLI-subcommand-contribution trait. There is nothing to "port the trait to."
- **agens-plugin already IS the host** (Stage R3a relocated `run_with_providers` into agens; agens owns `src/bin/pares-agens.rs`). The `CommandProvider`/`ProviderRegistry` indirection existed only to plug into radix's *old* registry. Since agens owns the host, the fix is to **delete the dependency on the removed cli-api seam and call the agens command surface directly** (or via a ~40-line agens-local registry), not to adopt a new radix trait.
- **`AuthorizationGate` in pares-agens-core is NOT broken by an API change** — its v1.55.13 signature is byte-identical to what agens calls. The E0599/E0053/E0308 errors are a **pin-drift type-duplication artifact**: today every agens crate pins radix at `v1.49.2`; the moment agens-plugin is bumped to `v1.55.13` while agens-core stays at `v1.49.2`, two distinct `RuleResult`/`RuleContext`/`AuthorizationGate` types enter the graph. **Unifying all radix pins to `v1.55.13` fixes it with zero code change to the gate call sites.**
- **One hard gap:** `pares_radix_migrate` (the `Migrate` subcommand: `migrate::run`, `openclaw::auto_detect`) is deleted at v1.55.13 with **no replacement anywhere in the workspace**. The `Migrate` command must be dropped (recommended) or reimplemented in agens.

**Call sites to change:** 4 files in `agens-plugin` (agent_commands/mod.rs, host_runtime/mod.rs, bin/pares-agens.rs, lib.rs). **0** code changes in pares-agens-core / tui / any other crate (they are fixed by pin unification alone). **~14 Cargo.toml pin lines** across the workspace bump v1.49.2 → v1.55.13; **2 deps dropped** (`pares-radix-cli`, `pares-radix-cli-api`); **1 dep dropped or replaced** (`pares_radix_migrate`).

> **Prior art:** an incomplete migration already exists on branch `m6-pluresdb-px-pin-bump` (PR #613, commit `b2eb206`). It did the **pluresdb-px AST bump only** and did **NOT** start the cli-api→ADR-0022 work. **S3 targets a FRESH branch off `main` that supersedes #613** and folds #613's 4 code deltas in. See **§0.5**.

---

## 0.5. Prior art — PR #613 (`m6-pluresdb-px-pin-bump`, commit `b2eb206`) — FOLD IN, do not redo

A partial migration is already open. It is **orthogonal to the cli-api removal**: it migrated the **pluresdb-px AST shape** (rev `195c67b`→`0ec9523`, where the flattened per-kind vectors `doc.dataflow_procedures` / `doc.scenarios` / `doc.procedures` were replaced by a single `doc.statements: Vec<pares_radix_praxis::px::Statement>` enum). It did **NOT** touch any `pares-radix-*` pin (still `v1.49.2`), did **NOT** drop `pares-radix-cli`/`-cli-api`, and did **NOT** touch the `CommandProvider`/`ProviderRegistry` seam. So the cli-api→ADR-0022 migration is **STILL UNSTARTED**; #613's only code work is the 4 px-AST accessor deltas below.

**Exactly what #613 changed (per file, verified via `git diff main...origin/m6-pluresdb-px-pin-bump`):**

| File | Change |
|---|---|
| `crates/agens-plugin/Cargo.toml` | `pluresdb-px` rev `195c67b` → **`0ec9523`** |
| `crates/core/Cargo.toml` | `pluresdb-px` rev `195c67b` → **`0ec9523`** |
| `crates/mcp-server/Cargo.toml` | `pluresdb-px` rev `195c67b` → **`0ec9523`** |
| `crates/agens-plugin/src/agent_commands/runtime.rs` **(2 sites: ~L400 in `RuntimeAgentFactory`, ~L5532 in `run_tui`)** | `for proc in &doc.dataflow_procedures {` → `for proc in doc.statements.iter().filter_map(\|s\| match s { pares_radix_praxis::px::Statement::DataflowProcedure(p) => Some(p), _ => None }) {` |
| `crates/agens-plugin/src/host_runtime/mod.rs` **(1 site, ~L541, inside `run_with_providers`'s `Px::Test` arm)** | `if doc.scenarios.is_empty() { continue; }` → `let has_scenarios = doc.statements.iter().any(\|s\| matches!(s, pares_radix_praxis::px::Statement::Scenario(_))); if !has_scenarios { continue; }` |
| `crates/mcp-server/src/radix_handler.rs` **(1 site, ~L2600)** | `if doc.procedures.is_empty() { … }` → `let has_procedures = doc.statements.iter().any(\|s\| matches!(s, px::Statement::DataflowProcedure(_) \| px::Statement::LegacyProcedure(_))); if !has_procedures { … }` |
| `Cargo.lock` | relocked for `pluresdb-px 0ec9523` (105 lines) — regenerate, don't hand-merge |

**Interaction with THIS migration (no conflicts):**
- The `host_runtime/mod.rs` #613 delta is at **~L541** (the `Px::Test` scenario check), which is **inside the surviving body** of `run_with_providers` and **disjoint** from the parts this plan edits (the cli-api re-exports at L22–31, `use pares_radix_migrate` at L44, and the `Migrate` variant/arm). They **coexist**: keep #613's scenario-check rewrite verbatim; separately delete the cli-api/migrate lines. When `run_with_providers` is rewritten for Option B (§5), **preserve the #613 `has_scenarios` block** inside the retained `Px::Test` arm.
- `runtime.rs` and `radix_handler.rs` #613 deltas are **pure px-AST** and are **required regardless** of the cli-api work — they must survive into the fresh branch untouched.
- **`pluresdb-px` must stay at `0ec9523`** (not revert to `195c67b`) on the fresh branch — the v1.55.13 `pares-radix-praxis` re-exports the `Statement` enum AST that matches `0ec9523`. (S3 should confirm the exact `px::Statement` variant set at v1.55.13 `pares-radix-praxis` matches these `match`/`matches!` arms; grep-confirmed the variants `DataflowProcedure`, `Scenario`, `LegacyProcedure` are referenced by #613 against `0ec9523`, and v1.55.13 praxis is a **newer** px than `0ec9523`, so the enum is present — verify no additional required arms.)

**Branch strategy:** **Do NOT rebase/extend `m6-pluresdb-px-pin-bump`.** Cut a **fresh branch off `main`** (e.g. `m6-radix-v1.55.13-cliapi-migration`) that: (1) bumps every `pares-radix-*` pin to `v1.55.13` + keeps `pluresdb-px 0ec9523` (§4), (2) drops `pares-radix-cli`/`-cli-api` + removes `pares_radix_migrate` usage (§3), (3) **re-applies #613's 4 code deltas** (they will re-apply cleanly since they touch different lines than the cli-api edits, EXCEPT the host_runtime rewrite must fold the `has_scenarios` block in by hand). This fresh branch **supersedes #613** — close #613 in favor of it (or mark it merged-by-supersession). The `pares-radix-praxis` bump #613 skipped is done here as part of the full v1.55.13 unification.

---

## 1. Evidence: what exists at v1.55.13

`radix-core/src/lib.rs` (v1.55.13) still exports every non-CLI module agens uses:
`auth`, `chronos`, `classifier`, `commands`, `event`/`Event`, `model`, `praxis`, `procedure`, `procedures`, `state`/`StateStore`/`PluresDbStateStore`/`InMemoryStateStore`, `tool_governance`, `plugins` (`PluginRuntime`, `PluginCrudExecutor`), `task_manager`, `tools`, `px_adapter`, `shell_executor`, `spine` (all submodules: `bootstrap`, `channel`, `conversation`, `pipeline`, `procedures/*`, `reactive`, `dispatcher`, …), `CrdtStore`/`SledStorage`/`StorageEngine`/`MemoryStorage`.

Verified present (grep, v1.55.13):
- `tool_governance::{ToolGovernor, GovernanceVerdict}` ✅
- `procedure::{Procedure (trait), ProcedureRegistry}` ✅
- `auth::copilot::{CopilotAuth, CopilotModelClient}` ✅
- `px_adapter::AsyncActionHandler` ✅
- `commands::{CommandResult, SessionCommand, CommandRegistry, CommandContext}` ✅ (tui dep — **unaffected**)
- `praxis::constraints::AuthorizationGate` ✅ (`crates/radix-core/src/praxis/constraints.rs:93`)
- `pares_radix_praxis::rule::{Rule, RuleResult, RuleContext, RuleCategory}` ✅ (`crates/praxis/src/rule.rs:17/103/…`)
- `pares_radix_praxis::px::{parse, compiler::compile, scenario_runner::{run_scenarios, BuiltinChecker}}` ✅ (host_runtime `Px` subcommand)
- `pares_rector::{cluster, discovery::PluresDbDiscovery, node::{ClusterNode, NodeStatus}}` ✅ (host_runtime `Cluster` subcommand — `pares-rector` still shipped)

Confirmed **REMOVED** at v1.55.13 (dirs absent):
- `crates/cli` ❌ · `crates/cli-api` ❌ · `crates/cli-runtime` ❌ · `crates/migrate` ❌ · `crates/mcp-server` ❌
- Therefore GONE: `pares_radix_cli_api::{CommandProvider, ProviderOutcome, ProviderRegistry, CommandError, CommandResult}`, `pares_radix_cli`, `pares_radix_migrate::{migrate, openclaw}`.

`pares-agens-mcp-server` = **local agens crate** (`crates/mcp-server`, `path = "../mcp-server"`) — NOT the removed radix `mcp-server`. Unaffected.

**No `[patch]` section exists** in any agens `Cargo.toml` (verified). Comments in `core/Cargo.toml` and `bitnet/Cargo.toml` that reference a "temporary `[patch]` block in the workspace root" are **stale scaffolding text** — there is no such block; the crates resolve `v1.49.2` directly from git.

---

## 2. Per-symbol old → new mapping

| Old symbol (v1.49.2) | Crate | New at v1.55.13 | Migration action |
|---|---|---|---|
| `CommandProvider` (trait: `name`, `augment(Command)->Command`, `handle(&str,&ArgMatches)->ProviderOutcome`) | `pares_radix_cli_api` | **REMOVED — no successor trait** | Delete the trait impl. Move `augment`/`handle` logic into inherent methods on `AgensProvider` (or free fns) that the agens host calls directly. |
| `ProviderOutcome` (`Handled(Result<(),CommandError>)`, `NotHandled`) | `pares_radix_cli_api` | **REMOVED** | Replace with a plain return type. Recommended: `handle` returns `Option<Result<(), String>>` (`None` = not handled, mirrors old `NotHandled`). |
| `CommandError` (`::msg(String)`) | `pares_radix_cli_api` | **REMOVED** | Replace error payload with `String` (or a small agens-local `enum AgensCommandError`). Old only used `CommandError::msg(...)`. |
| `CommandResult` (alias `Result<(),CommandError>`) | `pares_radix_cli_api` | **REMOVED** | Replace with `Result<(), String>`. (Do **not** confuse with `pares_radix_core::commands::CommandResult`, which is a different, surviving enum used by tui.) |
| `ProviderRegistry` (`new`, `register(Box<dyn CommandProvider>)`, `augment_all(Command)->Command`, `dispatch(&str,&ArgMatches)->Option<CommandResult>`, `is_empty`) | `pares_radix_cli_api` | **REMOVED** | Either (A) inline a ~40-line agens-local `ProviderRegistry` in `host_runtime`, or (B) **drop the registry entirely** and call `AgensProvider` directly from `run_with_providers` (recommended — agens has exactly one provider). |
| `pares_radix_migrate::migrate::run(&src,&out,dry_run)` | `pares_radix_migrate` | **REMOVED — no replacement** (grep found no `migrate::run`/`auto_detect` anywhere in v1.55.13) | **GAP.** Drop the `Migrate` subcommand (recommended) or reimplement OpenClaw import in agens. |
| `pares_radix_migrate::openclaw::auto_detect()` | `pares_radix_migrate` | **REMOVED — no replacement** | Same gap; removed with `Migrate`. |
| `pares_radix_cli` (crate, pinned in Cargo.toml, **never `use`d in code**) | `pares_radix_cli` | **REMOVED** | Drop the dependency line only; no code references it. |

**Unchanged (fixed by pin bump only — no code edits):**

| Symbol | Crate/module | Status at v1.55.13 |
|---|---|---|
| `AuthorizationGate` + `.evaluate(&RuleContext)->RuleResult` | `pares_radix_core::praxis::constraints` | **Identical signature.** `pub struct AuthorizationGate;` impl `Rule`. Doc example shows the exact `gate.evaluate(&ctx)` pattern agens-core uses. |
| `RuleResult::{Pass, Fail{reason}, Warning{message}, Gate{action,rationale}}` | `pares_radix_praxis::rule` | Identical variants agens matches on. |
| `RuleContext::new(action, payload)` | `pares_radix_praxis::rule` | Identical. |
| `pares_radix_core::commands::{CommandResult, SessionCommand, CommandRegistry, CommandContext}` | `radix-core` | Present — tui unaffected. |
| All `pares_radix_core::*` used by `agent_commands/runtime.rs` (auth/model/plugins/procedure/shell_executor/tool_governance/Event/StateStore/px_adapter/chronos/spine::*/task_manager/tools/CrdtStore) | `radix-core` | All present (see §1). Bump-only. |
| `pares_rector::*` (cluster/discovery/node) | `pares-rector` | Present. Bump-only. |
| `pares_radix_praxis::px::*` | `praxis` | Present. Bump-only. |

---

## 3. Per-file / per-call-site change list

### 3.1 `crates/agens-plugin` — the only crate with real code changes

**A. `src/agent_commands/mod.rs`** (the `CommandProvider` impl)

- **:3** (doc) — reword: no longer "implements `pares_radix_cli_api::CommandProvider`". Describe as "provides the agens agent command surface consumed by the host."
- **:19** — `use pares_radix_cli_api::{CommandProvider, ProviderOutcome};` → **DELETE**. (No replacement import needed.)
- **:82** — `#[async_trait] impl CommandProvider for AgensProvider {` → change to inherent `impl AgensProvider {` (drop `#[async_trait]` on the impl unless a method stays `async fn` — `handle` does; keep `#[async_trait]` **only if** you keep a trait, otherwise `async fn` in an inherent impl is fine on current toolchain and needs no macro).
  - `fn name(&self)->&str` — keep as inherent (or drop; only used for logging). Low value; keep.
  - `fn augment(&self, cmd: Command)->Command` — keep body **verbatim** (the clap subcommand definitions for `serve-spine`/`serve`/`tui`/`ask`/`classify` are unaffected). Just now an inherent method.
- **:202** — `async fn handle(&self, name:&str, m:&ArgMatches) -> ProviderOutcome` → change return type to **`Option<Result<(), String>>`** (inherent async method). Body edits:
  - **:225** `Ok(()) => ProviderOutcome::Handled(Ok(()))` → `Ok(()) => Some(Ok(()))`
  - **:226–227** `Err(_) => ProviderOutcome::Handled(Err(pares_radix_cli_api::CommandError::msg(format!("agens command '{name}' panicked"))))` → `Err(_) => Some(Err(format!("agens command '{name}' panicked")))`
  - **:336** `_ => ProviderOutcome::NotHandled` → `_ => None`
- The `run_on_local_rt!` macro and all five `match name` arms (serve-spine/serve/tui/ask/classify → `runtime::run_*`) are otherwise **unchanged**.

**B. `src/host_runtime/mod.rs`** (the composition seam)

- **:22–26** (doc + `pub(crate) use pares_radix_cli_api as command_provider;`) → **DELETE** the re-export.
- **:29–31** `pub use pares_radix_cli_api::{CommandError, CommandProvider, CommandResult, ProviderOutcome, ProviderRegistry};` → **DELETE**.
  - If option (A) chosen: instead define an agens-local `ProviderRegistry` here (small struct holding `Vec<AgensProvider>` or `Vec<Box<dyn ...>>`) and `pub use` it so `bin/pares-agens.rs` still compiles.
  - If option (B) chosen (**recommended**): no registry type at all.
- **:44** `use pares_radix_migrate::{migrate, openclaw};` → **DELETE** (crate gone).
- **`Commands::Migrate { from, output, dry_run }`** variant (enum ~:78 + its match arm ~:340) — **REMOVE the variant and its match arm** (recommended), OR keep the variant and replace the body with a real agens implementation (out of scope for a mechanical migration — see gap §4.1). `migrate_data_dir` (the `~/.pares-radix` rename helper, ~:180) is **local** to this file and stays.
- **:192** `pub async fn run_with_providers(registry: command_provider::ProviderRegistry)`:
  - Option (A): signature keeps the agens-local `ProviderRegistry`; body's `registry.augment_all(base)` / `registry.dispatch(name, sub_matches)` / `registry.is_empty()` call the new local impl.
  - Option (B, recommended): change signature to `pub async fn run_agens_host()` (no arg) OR `run_with_providers(provider: AgensProvider)`. Replace the registry block:
    - `let augmented = provider.augment(base);`
    - after `get_matches()`, `if let Some((name, sub)) = matches.subcommand() { if let Some(res) = provider.handle(name, sub).await { match res { Ok(())=>return, Err(e)=>{ eprintln!("{e}"); std::process::exit(1);} } } }`
  - The rest of `run_with_providers` (tracing init, log dir, `Cluster`/`McpServe`/`Config`/`Px` arms) is **unchanged** except the removed `Migrate` arm.
- **NOTE (cosmetic, out of scope but flag it):** the `#[command(name = "pares-radix", …)]` and all `~/.pares-radix` paths still say "radix". Renaming to `pares-agens` is a deliberate deferred follow-up (documented in-file); **do not** change it in this migration.

**C. `src/bin/pares-agens.rs`**

- **:9** `use agens_plugin::host_runtime::{run_with_providers, ProviderRegistry};`
  - Option (A): keep both (now the agens-local registry).
  - Option (B): `use agens_plugin::host_runtime::run_agens_host;` (or `run_with_providers`) — drop `ProviderRegistry`.
- **:14** `let registry = ProviderRegistry::new().register(Box::new(AgensProvider::new())); run_with_providers(registry).await;`
  - Option (A): unchanged (local registry).
  - Option (B, recommended): `run_agens_host().await;` (host constructs `AgensProvider` internally), or `run_with_providers(AgensProvider::new()).await;`.

**D. `src/lib.rs`**

- **:16** (doc) `//! - It implements [`pares_radix_cli_api::CommandProvider`] ([`AgensProvider`]) to …` → reword: "It provides the agens agent command surface (`AgensProvider`) consumed by the host composition (`host_runtime::run_*`)." No functional change; keep `pub use` of `AgensProvider` and `host_runtime`.

### 3.2 `crates/core` (pares-agens-core) — **NO code changes**

- `src/orchestrator/mod.rs:42` `use pares_radix_core::praxis::constraints::AuthorizationGate;` — **unchanged**.
- `:442` `AuthorizationGate.evaluate(&gate_ctx)` — **unchanged** (signature identical at v1.55.13).
- `:445` `if let RuleResult::Fail { reason } = &gate_result` — **unchanged** (variant identical).
- test sites `:1123`, `:1136`, `:1151` (`RuleResult::{Pass, Gate{..}}`) — **unchanged**.
- **The E0599/E0053/E0308 errors disappear once `pares-radix-core` + `pares-radix-praxis` here are bumped to `v1.55.13`** so the whole graph shares one type identity. This crate's only fix is the Cargo.toml pin bump (§4).

### 3.3 Other crates — **NO code changes; pin bump only**

`channels`, `models`, `mcp-client`, `mcp-server`, `tauri-app`, `tui`, `bitnet`, `cli` — each pins `pares-radix-core` (and `channels` also `pares-rector`) at `v1.49.2`. Bump to `v1.55.13`. Their code uses only surviving radix-core symbols (spot-checked: tui `pares_radix_core::commands::*` ✅ present). If any hidden drift surfaces at compile time, treat as a follow-up S3 sub-task — but no removed cli-api symbol is referenced by any of them (grep-confirmed: the only `Command*`/`Provider*` hits outside agens-plugin are unrelated local types — `agenda::SchedulerCommandError`, `radix_core::commands::CommandResult`).

---

## 4. Cargo.toml pin / dependency changes

**On the FRESH branch off `main` (§0.5).** **Bump every `tag = "v1.49.2"` → `tag = "v1.55.13"` on radix-origin crates**, drop the removed deps, **and keep `pluresdb-px` at `0ec9523`** (the #613 rev — do NOT revert to `195c67b`; the v1.55.13 `pares-radix-praxis` px `Statement` AST matches `0ec9523`, not the old flattened-vector shape). Exact lines:

| File | Line | Current | Action |
|---|---|---|---|
| `crates/agens-plugin/Cargo.toml` | 25 | `pares-radix-cli-api = { … tag = "v1.49.2" }` | **DROP** (crate removed) |
| `crates/agens-plugin/Cargo.toml` | 26 | `pares-radix-praxis = { … tag = "v1.49.2" }` | bump → `v1.55.13` |
| `crates/agens-plugin/Cargo.toml` | 31 | `pares-radix-core = { … tag = "v1.49.2" }` | bump → `v1.55.13` |
| `crates/agens-plugin/Cargo.toml` | 32 | `pares-radix-cli = { … tag = "v1.49.2" }` | **DROP** (crate removed, never `use`d) |
| `crates/agens-plugin/Cargo.toml` | 34 | `pares-rector = { … tag = "v1.49.2" }` | bump → `v1.55.13` |
| `crates/agens-plugin/Cargo.toml` | (add) | *(pares_radix_migrate had no dep line? verify)* | see note ‡ |
| `crates/core/Cargo.toml` | 62 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/core/Cargo.toml` | 64 | `pares-radix-praxis … v1.49.2` | bump → `v1.55.13` |
| `crates/channels/Cargo.toml` | 23 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/channels/Cargo.toml` | 26 | `pares-rector … v1.49.2` | bump → `v1.55.13` |
| `crates/cli/Cargo.toml` | 32 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/mcp-client/Cargo.toml` | 18 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/mcp-server/Cargo.toml` | 37 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/mcp-server/Cargo.toml` | 42 | `pares-radix-praxis … v1.49.2` | bump → `v1.55.13` |
| `crates/models/Cargo.toml` | 18 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/tauri-app/Cargo.toml` | 26 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/tui/Cargo.toml` | 20 | `pares-radix-core … v1.49.2` | bump → `v1.55.13` |
| `crates/bitnet/Cargo.toml` | 29 | `pares-radix-core … v1.49.2` (optional) | bump → `v1.55.13` |
| `crates/agens-plugin/Cargo.toml` | 50 | `pluresdb-px … rev = "195c67b"` | **already `0ec9523` on #613 — KEEP `0ec9523`** (fold #613) |
| `crates/core/Cargo.toml` | (pluresdb-px) | `rev = "195c67b"` on `main` | **set `0ec9523`** (fold #613) |
| `crates/mcp-server/Cargo.toml` | (pluresdb-px) | `rev = "195c67b"` on `main` | **set `0ec9523`** (fold #613) |

‡ **`pares_radix_migrate` dependency line:** `host_runtime/mod.rs:44` `use pares_radix_migrate::…` implies a `pares-radix-migrate` dep must exist somewhere for the current (v1.49.2) build — but it is **not** in the grep of agens-plugin/Cargo.toml lines 25–34. **S3 must confirm** where `pares-radix-migrate` is declared (possibly a workspace dep or a line the §1 grep pattern missed) and **DROP it**. Action regardless: remove the `use` at :44 and the `Migrate` command, then remove any `pares-radix-migrate` dep line.

**RESOLVED (S1):** there is **NO `pares-radix-migrate` dependency line anywhere** in the agens workspace (full-file scan of every `Cargo.toml`; the only `radix-migrate` hit is a comment at agens-plugin/Cargo.toml:28). So `host_runtime/mod.rs:44` `use pares_radix_migrate::{migrate, openclaw};` references an **undeclared crate** — the crate does not cleanly build even at v1.49.2 without it resolving transitively, which reinforces that `Migrate` is dead weight. **Action: delete the `use` at :44 and the `Migrate` command; there is no dep line to remove.** No new radix crate dep needs to be ADDED for the migration (all surviving surfaces come from already-pinned `pares-radix-core`, `-praxis`, `pares-rector`).

---

## 5. Recommended approach: **Option B (drop the registry indirection)**

agens has exactly **one** provider. The `ProviderRegistry` (register / augment_all / dispatch / is_empty) existed to let *radix* host *N* external providers. agens is now the sole host of a single, known provider, so the registry is pure ceremony. Option B deletes ~one indirection layer and removes the last `pares_radix_cli_api` surface without inventing an agens-local clone.

**Resulting structure (Option B):**
- `agent_commands/mod.rs`: `AgensProvider` with inherent `augment(Command)->Command` + `async fn handle(&str,&ArgMatches)->Option<Result<(),String>>`. No trait, no async-trait on the impl.
- `host_runtime/mod.rs`: `pub async fn run_with_providers(provider: AgensProvider)` (keep the name for minimal churn at the call site) — builds base `Cli` command, `provider.augment(base)`, parse, give the provider first refusal via `provider.handle(...)`, else fall through to the host's own `Cluster`/`McpServe`/`Config`/`Px` dispatch. `Migrate` arm + `use pares_radix_migrate` deleted.
- `bin/pares-agens.rs`: `run_with_providers(AgensProvider::new()).await;` (drop `ProviderRegistry` import + construction).
- No `pares_radix_cli_api` / `pares_radix_cli` / `pares_radix_migrate` anywhere.

**Option A (keep a local registry)** is a valid fallback if you want to preserve the multi-provider seam for future non-agens providers: define `pub struct ProviderRegistry(Vec<Box<dyn AgensCommand>>)` + a local `trait AgensCommand` in `host_runtime`, re-implement `augment_all`/`dispatch`/`is_empty`/`register`. More code, keeps the extension point. **Not recommended** unless a second provider is imminent (none is).

---

## 6. Risks / gaps / unknowns

1. **HARD GAP — `Migrate` command has no successor.** `pares_radix_migrate::{migrate::run, openclaw::auto_detect}` is deleted with no replacement anywhere in v1.55.13 (grep-confirmed). **Recommendation: drop the `Migrate` subcommand** (praxisbot does not depend on OpenClaw import at runtime; it was a one-shot data-migration utility). If OpenClaw import is still needed, it must be **reimplemented in agens** as a separate task — do NOT stub it (HARD GATE: no stubs). Flag to kbristol before deletion only if `Migrate` is believed to still be in use; otherwise delete per the no-stub / dead-code policy.
2. **Undeclared `pares_radix_migrate` dep (see §4‡).** The current tree `use`s a crate with no Cargo.toml edge. This means the *current* `v1.49.2` build may itself be broken/uncompiled for agens-plugin. **S3 must actually run `cargo check -p agens-plugin` at the START** (before any edits) to capture the true baseline — the assumption "it builds at v1.49.2" is unverified and probably false for this crate.
3. **Transitive drift beyond the enumerated symbols (LOW).** `agent_commands/runtime.rs` touches a very large `pares_radix_core` surface (spine bootstrap/pipeline/procedures, model streaming, task_manager, tools, chronos, px_adapter). All top-level modules verified present at v1.55.13, but **method-level signatures inside those modules may have drifted** across 6 minor versions (v1.49→v1.55). This cannot be fully proven without compiling. **S3 mitigation: bump pins, then `cargo check --workspace` and triage any residual errors module-by-module.** Expect a handful of signature touch-ups in `runtime.rs`; none are architectural.
4. **`~/.pares-radix` naming + `#[command(name = "pares-radix")]` (COSMETIC, out of scope).** Deliberately deferred (documented in host_runtime). Do not touch during this migration; track as a separate rename task.
5. **`pares_radix_core::commands` (tui) shape (LOW).** Types present at v1.55.13; field/variant-level drift possible but tui compiles against the same core, so pin unification should suffice. Verify at `cargo check -p pares-agens-tui`.
6. **Other member crates (channels/models/mcp-client/mcp-server/bitnet/cli/tauri-app) (LOW).** No removed-symbol refs (grep-clean). Bump-only; residual signature drift triaged in S3 compile pass.
7. **`async fn` in inherent impl (Option B) (TRIVIAL).** `handle` stays `async fn` on an inherent `impl` — fine on the current toolchain, no `#[async_trait]` needed. If an older MSRV is enforced, keep `#[async_trait]` on an inherent impl instead.

**No API was invented in this plan.** Every "new" target is either (a) a surviving v1.55.13 symbol verified by grep/read, (b) a plain Rust type (`String`, `Option<Result<…>>`) replacing a removed cli-api type, or (c) an explicitly-flagged gap with a drop-or-reimplement decision.

---

## 7. Recommended S3 implementation order

1. **Cut a FRESH branch off `main`** (e.g. `m6-radix-v1.55.13-cliapi-migration`) that supersedes PR #613 (§0.5). Do NOT extend `m6-pluresdb-px-pin-bump`.
2. **Capture true baseline.** `cargo check -p agens-plugin` and `cargo check --workspace` **before any edit** (confirm the real v1.49.2 error set; validate the §6.2 undeclared-migrate hypothesis).
3. **Fold in #613's px-AST deltas (§0.5) FIRST** on the fresh branch: set `pluresdb-px` → `0ec9523` in agens-plugin/core/mcp-server; re-apply the 4 code deltas (runtime.rs ×2, host_runtime/mod.rs `has_scenarios`, radix_handler.rs `has_procedures`). `cargo check --workspace` (still on v1.49.2 radix) to confirm #613 reproduces green. *(Alternatively `git cherry-pick b2eb206` then hand-resolve, but the deltas are small enough to re-apply directly and avoid the pin-context conflict.)*
4. **Pin unification (Cargo.toml only).** Apply all §4 `pares-radix-*` bumps (v1.49.2→v1.55.13) and drops (`pares-radix-cli-api`, `pares-radix-cli`) across every member crate in one pass. Keep `pluresdb-px 0ec9523`. Do NOT edit code yet.
5. **`cargo check --workspace`.** Expect: (a) pares-agens-core `AuthorizationGate` errors **GONE** (validates the type-duplication root cause); (b) agens-plugin now failing on `pares_radix_cli_api` / `pares_radix_migrate` unresolved imports (expected — these are the code edits); (c) confirm `px::Statement` variant arms from #613 still compile against v1.55.13 `pares-radix-praxis` (§0.5 note).
6. **agens-plugin code edits (§3.1), Option B:**
   a. `agent_commands/mod.rs` — trait→inherent, `ProviderOutcome`→`Option<Result<(),String>>`, `CommandError::msg`→`String` (:19/:82/:202/:225-227/:336).
   b. `host_runtime/mod.rs` — delete cli-api re-exports (:22-31), delete `use pares_radix_migrate` (:44), delete `Migrate` variant+arm, rewrite `run_with_providers` to take `AgensProvider` and call `augment`/`handle` directly (:192). **Preserve #613's `has_scenarios` block (~:541) inside the retained `Px::Test` arm.**
   c. `bin/pares-agens.rs` — `run_with_providers(AgensProvider::new()).await;` (:9/:14).
   d. `lib.rs` — doc reword (:16).
7. **`cargo check -p agens-plugin`.** Triage residual `runtime.rs` signature drift (§6.3) module-by-module; fix minimally against v1.55.13 signatures (read the exact v1.55.13 fn sigs from the checkout — never guess).
8. **`cargo check --workspace` green**, then `cargo clippy --workspace -- -D warnings`.
9. **Build + run the binary** (test-first gate): `cargo build -p agens-plugin --bin pares-agens`, then smoke-run `pares-agens --help` (confirms clap surface: `serve-spine`/`serve`/`tui`/`ask`/`classify` present, `migrate` gone) and `pares-agens px check <a .px>` + `pares-agens px test <a .px with a scenario>` (confirms the folded #613 px-AST path). Recovery/error paths that changed (the `handle` panic→`Err(String)`) get an exercised run.
10. **Regenerate `Cargo.lock`** (relocks automatically on check for both the v1.55.13 tags and `pluresdb-px 0ec9523`; do not hand-merge #613's lock).
11. **Commit** per pares-agens conventions; open the fresh-branch PR **noting it supersedes #613** (close #613). Hand to S4/verify.

**Gate for S3→next:** workspace compiles + clippy-clean + `pares-agens` binary builds and `--help` shows the migrated command surface with no `pares_radix_cli_api` / `pares_radix_migrate` residue; #613's px-AST deltas folded in (px check/test both pass). No stubs, no `todo!()`, `Migrate` either cleanly removed or really reimplemented.