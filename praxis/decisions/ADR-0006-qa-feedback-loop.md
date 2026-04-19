# ADR-0006: QA Feedback Loop

**Status:** Accepted
**Date:** 2026-03-24
**Context:** QA validation must run after every merge to catch regressions. Failures file issues assigned to Copilot, closing the loop.

## Evidence

### Tested Facts

- QA issue reporter (`lib/issue-reporter.ts`) deduplicates by title: **confirmed**
- QA issue reporter assigns Copilot via GraphQL: **confirmed** (BOT_kgDOC9w8XQ)
- Playwright test suites exist for design-dojo and pares-agens: **confirmed**
- Issue filing from test failures works end-to-end: **confirmed** (2026-03-22)

### Design Decisions

#### Reactive trigger only, no scheduled backstop

- **Rationale:** Event-driven architecture (ADR-0001). Cron backstops are a crutch that masks broken event chains.
- **Trigger:** `repository_dispatch` type `pr-merged` from lifecycle workflow only.
- **Removed:** 6-hour cron schedule (was added as backstop, violates reactive principle).

#### Test suites written by Copilot, per-repo

- **When:** After the FIRST feature PR merges for a repo that has no test suite in `plures/qa`.
- **How:** queue-advance in `plures/qa` creates a scaffolding issue: "Create test suite for {repo}" when a dispatch arrives for a repo with no `suites/{repo}/` directory.
- **Who:** Copilot, assigned via standard queue mechanism.

#### QA failures block feature queue in target repo

- **Rationale:** Shipping new features on top of known regressions compounds tech debt.
- **Implementation:** QA-filed issues get label `qa` + `bug`. queue-advance in target repo checks for open `qa` bugs before assigning feature work.

### Unknowns

- Test suite scaffolding quality — can Copilot write meaningful tests from repo inspection alone?
- False positive rate — will flaky tests create noise issues?
- Cross-repo auth — does COPILOT_PAT have dispatch permissions to `plures/qa` from all repos?

## Constraints

- QA issues use label `qa` + `bug`, type `Bug`
- One PR per repo (inherited from ADR-0003)
- Suites live in `plures/qa/suites/{repo-name}/`
