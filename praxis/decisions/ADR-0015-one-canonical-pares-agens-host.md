# ADR-0015: One Canonical `pares-agens` Host Binary

**Status:** Proposed (immediate fix applied; consolidation decision pending)
**Date:** 2026-07-06
**Context:** Two crates in this workspace both declared `[[bin]] name = "pares-agens"`:

- `crates/cli` (package `pares-agens-cli`, `src/main.rs`) — owns the **OpenClaw migration**
  command (`pares-agens migrate --from ~/.openclaw`, backed by the `pares_agens_migrate` lib
  = `openclaw.rs` + `migrate.rs`) **and** a `serve --telegram-token …`. This is the binary the
  CI (`.github/workflows/build-all.yml`) builds (`-p pares-agens-cli`) and deploys
  (`cp target/release/pares-agens ~/.local/bin/pares-agens`; `systemctl restart pares-agens`).
- `crates/agens-plugin` (`src/bin/pares-agens.rs`) — the **fuller host** that composes the
  radix host runtime (`run_with_providers`) with `AgensProvider`, contributing `serve-spine`,
  `serve`, `tui`, `ask`, `classify`. It is **not** built or deployed by name anywhere (only a
  workspace `members` entry).

Two link jobs emitting `pares_agens.exe` collide (`LNK1104`), so `cargo build --workspace`
fails to link the host binary. (All ~360 library crates compile — this is purely a bin-name
collision, not a code error.)

This is a **single-source-of-truth violation** (same class as ADR-0010, which extracted a
copy-pasted self-update builder out of `crates/cli/src/main.rs`). Worse than a name clash: the
two binaries have **diverged command surfaces** — the deployed one (`pares-agens-cli`) does
**not** expose `serve-spine`, while the un-deployed plugin host does; and OpenClaw `migrate`
lives only in the CLI (the plugin host's `Migrate` subcommand was previously removed). There is
no single binary that is both the deployed host AND has the full `serve-spine` + `migrate`
surface the OpenClaw→radix migration (EPIC-RADIX-MIGRATION) depends on.

## Decision

**There must be exactly one binary named `pares-agens`, and it is the canonical agent host.**

### Immediate fix (applied 2026-07-06, deployment-neutral)

Rename the **un-deployed** `agens-plugin` bin `pares-agens` → **`praxisbot`** (its own documented
identity: "the agens plugin binary (praxisbot)"). This resolves the `LNK1104` link collision so
`cargo build --workspace` succeeds, and changes **zero** deployment behavior (CI/systemd continue
to build+ship `pares-agens-cli` as `pares-agens`). It does not hide the duplication — the two
binaries are now named for what they are (`pares-agens` = deployed CLI host; `praxisbot` = plugin
host with `serve-spine`).

### Consolidation decision (PENDING — strategic, tracked in EPIC-RADIX-MIGRATION B0)

The end-state must be **one** host binary that owns the full surface (`serve` / `serve-spine` /
`migrate` / `tui` / `ask` / `classify`). Two candidate resolutions:

- **Option A — CLI becomes the one host.** Fold `serve-spine` + host-runtime composition into
  `crates/cli` (the deployed binary); drop the `agens-plugin` bin (keep the crate as a library if
  `AgensProvider` is consumed). Pro: no deployment change. Con: moves the richer host runtime.
- **Option B — plugin host becomes the one host.** Add the OpenClaw `migrate` subcommand to the
  plugin host's command surface (via `AgensProvider`/host), deprecate `crates/cli`'s bin, and
  repoint CI/systemd to build+ship the plugin host as `pares-agens`. Pro: keeps the fuller
  `serve-spine` runtime as canonical. Con: changes the CI build target + deploy path.

The choice determines the **migration target binary** for EPIC-RADIX-MIGRATION (B0/B2/B3) and is
therefore a strategic call, surfaced to kbristol rather than picked unilaterally on first touch.

## Enforcement (required by foundational-engineering.px `adr_requires_enforcement`)

- **CI check (to add):** a workspace-build step (`cargo build --workspace`) in agens CI so a future
  duplicate `[[bin]]` name re-introducing the collision fails the build (today CI only builds
  `-p pares-agens-cli`, which is why the collision slipped in undetected).
- **Grep guard (to add):** assert exactly one `[[bin]] name = "pares-agens"` across the workspace
  Cargo manifests.
- Until the consolidation option is chosen, `praxisbot` + `pares-agens` coexist with **distinct**
  names and this ADR records the debt.

## Consequences

- `cargo build --workspace` links cleanly; the full workspace is buildable + testable again.
- The OpenClaw-migration binary path (`pares-agens migrate`) is unchanged and deployable.
- A follow-up consolidates the two hosts into one canonical `pares-agens` (Option A or B) per the
  EPIC-RADIX-MIGRATION B0 decision.

## Evidence

- `crates/cli/Cargo.toml`: `[[bin]] name = "pares-agens"`, `[lib] name = "pares_agens_migrate"`.
- `crates/agens-plugin/src/bin/pares-agens.rs`: composes `run_with_providers(AgensProvider::new())`;
  doc lists `serve-spine, serve, tui, ask, classify`.
- `crates/agens-plugin/src/mod.rs`: the OpenClaw `Migrate` subcommand "was removed" from the host.
- `.github/workflows/build-all.yml`: builds `-p pares-agens-cli`, `cp target/release/pares-agens`,
  `systemctl restart pares-agens` — the deployed `pares-agens` is the CLI binary.
- Related: ADR-0010 (no duplicated operational logic) — prior duplication in the same `crates/cli`.
