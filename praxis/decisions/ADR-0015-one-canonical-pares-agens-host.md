# ADR-0015: `pares-agens` is a Plugin, Not a Host Binary

**Status:** Accepted (rewritten 2026-07-07; supersedes the 2026-07-06 "one canonical `pares-agens` host binary" framing, which was built on an inverted architecture model)
**Date:** 2026-07-06 (original), 2026-07-07 (corrected)
**Deciders:** kbristol (confirmed the corrected model 2026-07-07, msg #40457/#40510)

## Correction notice

The original ADR-0015 asked *"which binary becomes THE canonical `pares-agens` host?"* and offered
Option A (fold `serve-spine` into `crates/cli`) vs Option B (ship the plugin host as `pares-agens`).
**That entire framing was wrong.** It assumed `pares-agens` is a runnable host — the inverted model.

The corrected, canonical architecture (see `plures/development-guide` → `design/REPO-CATALOG.md`
and `PLURES-FOUNDATION.md`):

- **`pares-radix`** is the **host** — the public Tauri + Svelte desktop application, scaffolded from
  `svelte-tauri-template`, shipping the Praxis / PluresDB / Unum / Chronos baseline. It is the
  plugin *host*.
- **`pares-modulus`** is the **plugin/extension library** (the SDK plugins are written against).
- **`pares-agens`** is a **private plugin** loaded by radix via modulus. It provides the agent/AI
  functionality (the innovative, IP-protected core). It is **hosted**, not a host.

This crate already knows this: `crates/agens-plugin/Cargo.toml` describes itself as *"thin plugin
over pares-radix-as-a-library (CommandProvider + agens-brought IP: headroom, model/bitnet)."* Only
the ADR and one binary name lagged behind the corrected model.

## Context (the real problem underneath the wrong framing)

Two crates in this workspace both declared `[[bin]] name = "pares-agens"`:

- `crates/cli` (package `pares-agens-cli`, `src/main.rs`) — owns the **OpenClaw→radix migration**
  command (`migrate --from ~/.openclaw`, lib `pares_agens_migrate` = `openclaw.rs` + `migrate.rs`)
  plus a `serve`. This is the binary CI builds (`-p pares-agens-cli`) and deploys
  (`cp target/release/pares-agens ~/.local/bin/pares-agens`; `systemctl restart pares-agens`).
- `crates/agens-plugin` (`src/bin/pares-agens.rs`) — composes the radix host runtime
  (`run_with_providers(AgensProvider::new())`) contributing `serve-spine, serve, tui, ask,
  classify`. **Not** built or deployed by name — only a workspace `members` entry.

Two link jobs emitting `pares_agens.exe` collided (`LNK1104`), breaking `cargo build --workspace`.
(All ~360 library crates compile — purely a bin-name collision.) The *original* fix renamed the
un-deployed plugin bin to **`praxisbot`** — which is **the hostname of kbristol's home machine**.
Reusing a personal host's name as a binary identity inside the repo is a naming smell and a source
of exactly the radix/agens confusion this ADR now corrects.

## Decision

### 1. `pares-agens` is a plugin; radix hosts it. There is no "canonical `pares-agens` host binary."

The end-state is **not** a single `pares-agens` host binary. agens ships its capability as a
plugin/provider (`AgensProvider` / `CommandProvider`) that the **radix** host loads via modulus.
The strategic north star (kbristol, 2026-07-07): bring radix+agens to OpenClaw parity so the agent
runtime *runs inside agens-on-radix*, hosted on the home machine. The host is radix, not agens.

### 2. Binary renamed `praxisbot` → `agens-host` (applied 2026-07-07)

The un-deployed `agens-plugin` bin is renamed **`praxisbot` → `agens-host`** — a name that says
what it is (the agens plugin's host-composition entrypoint that boots radix runtime +
`AgensProvider`), and does **not** collide with the home hostname. This keeps `cargo build
--workspace` linking cleanly and remains **deployment-neutral** (CI/systemd still build+ship
`pares-agens-cli` as `pares-agens`).

> **Scope guard (important):** only the *binary identity* was renamed — `crates/agens-plugin/Cargo.toml`
> `[[bin]] name` and its doc comment. Every other `praxisbot` string in the tree
> (`DEFAULT_NIX_HOST`, `PARES_NIX_HOST` default, `nixos-rebuild switch --flake .#'praxisbot'`,
> shadow-training paths, deploy docs) legitimately refers to the **real home host** and was left
> untouched. A blind rename would have broken the nixos deploy-target references.

### 3. The `serve-spine` + `migrate` surface belongs to the radix-migration path, not an agens host

The OpenClaw→radix migration (`migrate --from ~/.openclaw`) and the spine runtime (`serve-spine`)
are steps on the path to running the agent inside radix. They do **not** justify a standalone
`pares-agens` host binary. As radix reaches parity, this capability converges into the
radix-hosted-agens plugin surface (tracked in EPIC-RADIX-MIGRATION), and the deployed
`pares-agens-cli` bin becomes a transitional migration/serve tool — not the long-term host.

## Enforcement (foundational-engineering.px `adr_requires_enforcement`)

- **CI (to add):** `cargo build --workspace` in agens CI, so any future duplicate `[[bin]]` name
  re-introducing the `LNK1104` collision fails the build (today CI only builds `-p
  pares-agens-cli`, which is why the collision slipped in).
- **Grep guard (to add):** assert **zero** occurrences of `praxisbot` as a `[[bin]]` name (or, more
  generally, that no `[[bin]]` name equals a known home hostname), and assert exactly one
  `[[bin]] name = "pares-agens"` in the workspace.
- **Doc guard:** the radix/agens/modulus direction is recorded authoritatively in
  `development-guide/design/REPO-CATALOG.md`; any code/ADR asserting "agens is the host / radix is a
  library" is drift and must be fixed against that file.

## Consequences

- `cargo build --workspace` links cleanly (`agens-host` + `pares-agens` no longer collide).
- The migration binary path (`pares-agens migrate`) is unchanged and deployable.
- The architecture record now matches reality: **radix hosts, agens is a plugin.** The Option A/B
  "which becomes the host" question is **void** — neither; radix is the host.
- No personal hostname is used as a binary identity.

## Evidence

- `crates/agens-plugin/Cargo.toml`: package desc "thin plugin over pares-radix-as-a-library
  (CommandProvider + agens-brought IP…)"; `[[bin]] name` now `agens-host` (was `praxisbot`).
- `crates/agens-plugin/src/bin/pares-agens.rs`: composes `run_with_providers(AgensProvider::new())`.
- `crates/cli/Cargo.toml`: `[[bin]] name = "pares-agens"`, `[lib] name = "pares_agens_migrate"`.
- `.github/workflows/build-all.yml`: builds `-p pares-agens-cli`, ships `pares-agens` — deploy path
  unchanged by this rename.
- Home-host `praxisbot` references (correct, untouched): `crates/channels/src/telegram.rs`,
  `crates/cli/src/main.rs`, `crates/agens-plugin/src/self_update.rs`, `crates/agenda/src/self_update.rs`,
  `crates/agens-plugin/src/agent_commands/runtime.rs`, `docs/NIXOS-DEPLOY.md`, `docs/SYSTEM-PROMPT.md`.
- Corrected model source of truth: `development-guide/design/REPO-CATALOG.md` (commit 4fdd77a).
- Related: ADR-0010 (no duplicated operational logic).
