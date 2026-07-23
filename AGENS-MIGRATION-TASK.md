# M6.5→AGENS-MIGRATION: agens↔radix v1.49.2→v1.55.13 surface migration

## Why
Bumping pares-radix-praxis to v1.55.13 (needed for PX-L010/L012 lint fix) forces the
ENTIRE pares-radix pin surface to move (split-version otherwise). But v1.55.13 removed
pares-radix-cli / pares-radix-cli-api via the breaking refactor:
  3172cfa refactor(radix-core)!: B1 S-B ... de-cognition cli-runtime/cli-api
and replaced the CLI CommandProvider model with the capability-plugin host contract
(ADR-0022 capability-host-contract, ADR-0024 canonical-plugin-format).
agens-plugin currently implements pares_radix_cli_api::{CommandProvider, ProviderOutcome,
CommandError} — none of which exist at v1.55.13.

## Ground truth (v1.55.13 checkout)
C:\Users\kbristol\.cargo\git\checkouts\pares-radix-e42d2bf425d083f6\78a4936\
  .praxis/decisions/ADR-0022-capability-host-contract.md
  .praxis/decisions/ADR-0024-canonical-plugin-format.md
  docs/PLUGIN-AUTHOR-GUIDE.md
  docs/architecture/plugin-system.md
Crates at v1.55.13: pares-omniscient, radix-{agenda,audit,bitnet-sys,core,marketplace,
  mcp-client,praxis,privacy,sync}, pares-rector. (NO cli, NO cli-api.)

## Stages (GATED — do not skip)
- [x] S1 ANALYZE (read-only): produce MIGRATION-PLAN.md — DONE (30.6 KB, 9 sections). Verdict: contained; Option B (drop registry indirection); 4 files in agens-plugin, 0 elsewhere; AuthorizationGate fixed by pin-unification alone; one hard gap = drop dead `Migrate` command (backing crate `pares_radix_migrate` deleted upstream, no replacement). #613 folded in.
- [ ] S2+S3 PIN+CODE (coupled): fresh branch off main; fold #613 px-AST deltas; bump all pares-radix-* v1.49.2→v1.55.13 + keep pluresdb-px 0ec9523; drop cli/cli-api + Migrate; Option-B agens-plugin edits per plan §3.1/§7. cargo check --workspace green.
- [ ] S4 TEST: cargo test --workspace green; clippy -D warnings (CI is authority if local blocked).
- [ ] S5 COMMIT: commit on branch m6.5-repin-pluresdb-px-radix-praxis; push; gate CI green.
- [ ] S6 VERIFY: gh pr checks green on the agens PR. Then M6→M7 close-out.

## Gate rule
S1 plan reviewed before S3 code. S4 test MUST pass before S5 commit. No stubs (C-NOSTUB-001).
