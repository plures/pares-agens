# ADR-0010: No Duplicated Operational Logic Across Crates

**Status:** Accepted  
**Date:** 2026-05-13  
**Context:** Self-update command builder was copy-pasted in both `crates/channels/src/telegram.rs` and `crates/cli/src/main.rs`. A bug (wrong package name + no dirty-tree handling) had to be fixed in both places — and was initially only fixed in one, leaving the other broken. This is the textbook failure mode of code duplication.

## Decision

**Operational logic that is used by more than one crate must live in exactly one place.**

The canonical location for shared operational infrastructure is `pares-agens-agenda` (scheduling, self-update, task management). Consumer crates delegate to it via thin wrapper functions that add no logic.

### Rules

1. **One source of truth.** If two crates need the same shell command, validation logic, or operational procedure, it goes in a shared crate. No exceptions.

2. **Consumers delegate, not duplicate.** A consumer function may adapt the shared function's signature for local ergonomics (e.g., matching an existing callback shape), but it must not re-implement any logic. The body is a single function call.

3. **Tests live with the logic.** Behavioral tests belong in the crate that owns the logic. Consumer tests only verify that delegation works (i.e., the output contains an expected marker).

4. **If you're fixing the same bug twice, you've already violated this ADR.** The fix is to extract first, then fix once.

## Consequences

- Self-update logic now lives in `pares_agens_agenda::self_update`.
- Both `telegram.rs` and `cli/main.rs` delegate to it.
- Future operational commands (backup, health check, diagnostics) follow the same pattern.
- Adding a new update step means changing one file, not hunting for copies.

## Evidence

- **2026-05-13:** `build_nixos_update_command` existed in two crates. The `telegram.rs` copy was updated to use `pares-agens-cli` (wrong name). The `cli/main.rs` copy still used the old NixOS flake path. Neither handled dirty `Cargo.lock`. The bug was reported by an end user who had to be told to run manual commands — exactly the failure mode automation is supposed to prevent.
