# ADR-0005: Tech Doc Writer Pipeline

**Status:** Accepted
**Date:** 2026-03-24
**Context:** Merged PRs change code but not docs. Stale docs poison Copilot and human reasoning on future work. Docs must be updated reactively on merge, with higher priority than new feature work.

## Evidence

### Tested Facts

- Copilot reads repo docs (README, docs/) when implementing issues: **confirmed** across multiple repos
- Stale docs cause Copilot to produce incorrect implementations: **observed** (references old API signatures, deleted modules)
- Doc-only PRs don't trigger further doc issues (title prefix `[docs]`): **untested** — needs validation
- **Race condition (2026-03-24):** Tech Doc Writer + queue-advance both fire on `pull_request.closed/merged`. If both assign Copilot, Copilot opens 2 PRs simultaneously (netops-toolkit #20 + #21). **Fix:** Doc writer creates issue but does NOT assign Copilot. queue-advance is the single assignment authority.

### Design Decisions

#### Doc priority: BEFORE next feature issue

- **Rationale:** Wrong docs → wrong Copilot output → wasted cycles. Docs are upstream of all feature work.
- **Implementation:** queue-advance checks for unresolved `documentation`-labeled issues FIRST. Only assigns feature work when no doc debt exists.
- **Hard rule:** Doc issues block feature queue.

#### Single assignment authority: queue-advance only

- **Rationale:** Race condition when two workflows assign Copilot simultaneously → two PRs → violates one-PR-per-repo (ADR-0003).
- **Implementation:** Tech Doc Writer creates issue with `documentation` label but does NOT assign Copilot or comment `@copilot`. queue-advance picks it up in priority order.
- **Evidence:** netops-toolkit #20/#21 created at same second (2026-03-24T07:18:14Z).

#### Reactive only, no cron/schedule

- **Rationale:** Architecture is event-driven (ADR-0001). Timers don't compose with reactive pipelines.
- **Trigger:** `pull_request.closed` + `merged == true` only.

#### Source of truth: PR diff, not existing docs

- **Rationale:** Existing docs may already be wrong. The doc writer must reference the merged PR's changed files and diff.
- **Implementation:** Issue body includes changed files list and code diff.
- **Guard:** Issue body includes `> ⚠️ Do NOT assume existing docs are correct.`

#### Loop prevention

- **Guard:** PRs titled `[docs]` or `chore(docs)` do NOT trigger further doc issues.
- **Guard:** Skips if only docs/config/workflow files changed.

### Unknowns

- Can Copilot reliably produce accurate doc updates from a diff alone?
- Will doc issues create noisy churn on high-velocity repos?

## Constraints

- One PR per repo (ADR-0003)
- Label + type required (ADR-0004)
- Doc issues: label `documentation`, type `Task`
- **Only queue-advance assigns Copilot** — never the doc writer workflow
