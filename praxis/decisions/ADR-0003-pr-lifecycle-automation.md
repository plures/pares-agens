# ADR-0003: PR Lifecycle Automation

**Status:** Accepted
**Date:** 2026-03-23
**Context:** Copilot creates PRs but can't shepherd them through review, approval, and merge. Automation handles the full lifecycle.

## Evidence

### Tested Facts

- Copilot PR actor login is `Copilot` (not `copilot-swe-agent[bot]`): **confirmed** across 30+ PRs
- `workflow_run` actor for Copilot is `app/copilot-swe-agent`: **confirmed** — different from PR context
- Ready-for-review → request Copilot review → wait → merge on APPROVED: **works** (24 PRs merged 2026-03-22)
- Auto-format (deno fmt) before review request: **works** — prevents format-only review comments
- Merge without waiting for APPROVED review: **caused bad merges** — reverted to require APPROVED
- Merge on any non-draft status without review: **caused premature merges** — reverted
- `action_required` from org policy blocks first-party workflows: **confirmed** — chicken-and-egg with auto-approve

### Lifecycle Phases (tested, validated)

```
Phase 1: PR opened/ready → mark ready for review
Phase 2: Request review from Copilot → STOP (wait for review)
Phase 3: Review returns changes_requested → Copilot addresses → re-request review → STOP
Phase 4: Check for merge conflicts → rebase if needed
Phase 5: Review status = APPROVED → merge (squash)
```

### Unknown

- Whether `auto_merge` GitHub API is more reliable than workflow-based merge
- Optimal delay between review request and merge check
- Whether parallel PR lifecycle runs on same repo cause race conditions

## Decision

1. 5-phase lifecycle: ready → review → address feedback → conflict check → merge on APPROVED
2. Only merge when review decision is APPROVED — no exceptions
3. Auto-format before review request to reduce noise
4. Each phase is a separate workflow run triggered by PR events — not polling
5. `action_required` handled by Deno Deploy webhook handler (not cron)

## Constraints

- NEVER merge without APPROVED review status
- PR author check uses `Copilot` login, not bot app name
- workflow_run author check uses `app/copilot-swe-agent`
- Lifecycle workflow uses concurrency group per PR number to prevent races
