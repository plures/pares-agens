# ADR-0019: Shadow-Arena Integration (pares-umbra → pares-agens)

**Status:** Accepted\
**Date:** 2026-07-30\
**Author:** copilot (automated)\
**Context:** ADR-0008 (Self-Improving Loop), issue #677

## Context

pares-umbra's nightly shadow-learning training produces evolved `.px` procedures
(router, priority, intent classifiers) that are committed to `pares-umbra-data`
and synced to praxisbot via the normal package path. pares-radix-core provides a
`ShadowProcedures` holder (`spine::shadow`) that loads these candidates from
`praxis/shadow/`, keeping them out of the live reactive registry.

However, the consuming side in pares-agens was never finished: `runtime.rs`
loaded the shadow candidates into `let _shadow_procedures` (leading underscore)
and immediately dropped them. No fitness scoring, no arena feed, no promotion
path existed — self-improvement was a one-way, inert data drop.

## Decision

### Phase A (this ADR): Retain and expose shadow candidates

1. **Retain loaded shadow candidates** in `Arc<ShadowProcedures>` shared state
   rather than discarding them. The holder now outlives the initialization block
   and is accessible to the procedure registry.

2. **Register a `shadow_status` procedure** that reports loaded candidate names,
   trigger kinds, and arena readiness to operators via the standard tool surface.

3. **Do NOT add `umbra-shadow` as a dependency yet.** The `umbra-shadow` crate
   (BSL-1.1 licensed) provides `ShadowArena` for fitness scoring, but license
   compatibility with pares-agens (MIT) has not been confirmed. The integration
   seam is ready; the dependency addition awaits explicit license review.

4. **Promotion requires explicit operator approval.** Per ADR-0008's constraint
   ("results are logged but never auto-applied without review") and this repo's
   safety-first governance, shadow procedures will never auto-promote to live
   without an explicit command.

### Future phases (not implemented here)

- **Phase B:** Expose `evolve-*` triggers to operators (shell out to `umbra`
  binary's `EvolveRouter`/`EvolvePriority`/`EvolveClassifier` subcommands).
- **Phase C:** Let `bitnet_classifier.rs` consume an umbra-evolved
  `EvolvableProcedure` instead of its fixed inference path (exploratory).

## Consequences

- Shadow candidates are now retained and inspectable at runtime — operators can
  verify what evolved procedures are loaded on any instance.
- The `shadow_status` procedure provides the foundation for monitoring once
  arena wiring is added.
- No new external dependencies are introduced by this change.
- pares-radix's `ShadowProcedures::candidates()` seam is now actively consumed
  (not dead code) on the pares-agens side.

## Risks

- Until `umbra-shadow` is wired in (Phase A completion), loaded candidates still
  do not accumulate fitness — this ADR closes the "discard" gap but does not yet
  provide the evaluation loop.
- License compatibility of BSL-1.1 (`umbra-shadow`) with MIT (pares-agens) must
  be resolved before the arena dependency is added.

## Related

- ADR-0008: Self-Improving Analysis-Research-Experiment Loop
- `qa/PARES-UMBRA-INTEGRATION-PROPOSAL.md` (full investigation)
- pares-radix `crates/radix-core/src/spine/shadow.rs` (holder implementation)
- Issue #677
