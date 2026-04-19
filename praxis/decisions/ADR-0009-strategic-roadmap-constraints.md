# ADR-0009: Strategic Roadmap as Executable Constraints

**Status:** Accepted
**Date:** 2026-03-25
**Author:** mswork (automated)
**Context:** Cross-repo dependency management, CI health enforcement

## Decision

Strategic and tactical objectives are encoded as live data in the PluresDB
graph — not as dead-document roadmaps. Praxis constraints evaluate the graph on
every GitHub event and enforce:

1. **Build-order gate** — Feature issues are blocked when upstream `@plures/`
   dependencies have unpublished changes. The dependency layering is:
   ```
   Layer 0: pluresdb, pares-agens, praxis-business, netops-toolkit, qa, knowledge-base, development-guide
   Layer 1: praxis (→ @plures/pluresdb)
   Layer 2: design-dojo, chronos, unum (→ @plures/praxis)
   Layer 3: FinancialAdvisor (→ @plures/praxis, @plures/design-dojo, @plures/pluresdb)
   ```

2. **CI health gate** — Repos with CI pass rate < 80% only receive CI-fix
   issues. Feature work is redirected until CI is green.

3. **Version-drift alarm** — Repos with > 3 unpublished versions trigger a
   publish issue. Consumers depending on `^x.y.z` won't see improvements until
   they're actually published.

4. **Objective dependency ordering** — Work queues are sorted by strategic
   objective priority, respecting `blockedBy` edges.

5. **Cascade bumps** — When a package publishes to npm, automatic PRs bump all
   downstream consumers.

## Rationale

- Roadmaps in markdown files are write-once, read-never.
- Copilot SWE agents don't read roadmaps — they read issue queues.
- Encoding strategy as constraints means the automation _physically cannot_
  work on the wrong thing in the wrong order.
- The decision ledger provides effectiveness metrics for free.

## Current Strategic Objectives (Q1 2026)

| ID                         | Priority | Title                                                        | Blocked By                                |
| -------------------------- | -------- | ------------------------------------------------------------ | ----------------------------------------- |
| `fix-plumbing`             | P0       | CI green everywhere, publish pending versions, cascade bumps | —                                         |
| `pares-praxis-integration` | P1       | Wire pares-agens into praxis constraint engine               | fix-plumbing                              |
| `mcp-distribution`         | P2       | Ship praxis as MCP server for any AI agent                   | pares-praxis-integration                  |
| `effectiveness-dashboard`  | P2       | Decision ledger → metrics dashboard                          | pares-praxis-integration                  |
| `fa-showcase`              | P3       | FinancialAdvisor demonstrates full stack                     | mcp-distribution, effectiveness-dashboard |

## Consequences

- Cross-repo-manager must hydrate `ConstraintContext` with `ciPassRate`,
  `driftingUpstream`, `unpublishedVersions` from GitHub API + npm registry.
- New constraint check kinds added to `pluresdb-praxis-types.ts`.
- The `seedSnapshot()` in `agents/praxis-db/engine.ts` includes strategic
  constraints.
- Decision ledger entries are created for every strategic gate evaluation.
- Level-driven evaluation runs every 6h, measures 13 health dimensions per repo,
  and generates improvement issues for any dimension below its target.
- Targets ratchet upward when met — no repo is ever "done."
- Zero momentum on a repo with open gaps is treated as a defect.

## Level-Driven Architecture

The system is level-driven, not event-driven. Instead of waiting for events
(push, PR, release), it periodically measures the **desired state** of every
repo across 13 health dimensions:

| Dimension         | Target | Floor | Description                |
| ----------------- | ------ | ----- | -------------------------- |
| ci-pass-rate      | 95%    | 80%   | Recent CI success rate     |
| ci-speed          | 90%    | 50%   | CI runs completing < 10min |
| version-published | 100%   | 100%  | All registries synced      |
| lint-clean        | 100%   | 90%   | Zero lint violations       |
| type-safety       | 100%   | 95%   | Strict types, no errors    |
| test-coverage     | 80%    | 50%   | Code under test            |
| test-exists       | 100%   | 100%  | At least one test file     |
| readme-quality    | 100%   | 60%   | Key sections present       |
| changelog-current | 100%   | 50%   | Releases documented        |
| deps-current      | 95%    | 80%   | Within 1 major of latest   |
| no-known-vulns    | 100%   | 100%  | Zero CVEs in dep tree      |
| api-documented    | 90%    | 50%   | Public API has docs        |
| momentum          | 100%   | 1%    | Commits in last 14 days    |

The gap between current and target generates work. Below floor = P0 critical.
When targets are met, they ratchet upward by 5 points (max 100).

## Evidence

- Initial observation (2026-03-24): FinancialAdvisor 0% CI, praxis-business 0%
  CI, pares-agens 50% CI, praxis npm 13 versions behind repo.
- All consumers pinned to `^1.x` while praxis is at `2.4.13`.
