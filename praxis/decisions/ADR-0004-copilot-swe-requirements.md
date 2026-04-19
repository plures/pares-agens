# ADR-0004: Copilot SWE Agent — Requirements (v2)

**Status:** Accepted (supersedes v1)
**Date:** 2026-04-01
**Context:** v1 documented observed requirements. v2 updates based on operational lessons: issue-reporter bypass caused 6-PR pileup (ADR-0005), lifecycle only merged Copilot PRs leaving human-authored PRs stuck, and nudge comments created noise without evidence of helping.

## Changes from v1

| Area                 | v1                                                   | v2                                                       |
| -------------------- | ---------------------------------------------------- | -------------------------------------------------------- |
| PR merge scope       | Copilot PRs only                                     | All PRs (CI green + review complete)                     |
| Default reviewer     | Manual / org ruleset                                 | Copilot is default reviewer for all PRs                  |
| Issue creation       | Reporter sets labels+type; gaps cause silent failure | Lifecycle ensures body, label, and type before assigning |
| Stalled issues       | Nudge with `@copilot` comment                        | Unassign after 5min with no PR; re-queue silently        |
| Comments on issues   | `@copilot` comments on assignment                    | No comments. Assignment only.                            |
| Assignment authority | Queue-advance + issue-reporter (dual path)           | Queue-advance only (issue-reporter fixed in qa@d663655)  |

## Evidence

### Tested Facts (carried from v1 + new)

| Condition                         | Label | Type | Result                        | Evidence                            |
| --------------------------------- | ----- | ---- | ----------------------------- | ----------------------------------- |
| label=enhancement, type=Feature   | ✅    | ✅   | **Success**                   | Issue #310 → PR #312                |
| no label, no type                 | ❌    | ❌   | **Cancelled**                 | Issues #247-#256                    |
| Multiple simultaneous assignments | ✅    | ✅   | **6 PRs, rebase chaos**       | pares-agens 2026-03-31              |
| issue-reporter direct assignment  | ✅    | ✅   | **Bypassed queue guards**     | qa issue-reporter pre-d663655       |
| Human PR, CI green, no review     | ✅    | ✅   | **Stuck — lifecycle skipped** | pluresdb PR #281                    |
| @copilot nudge comment            | ✅    | ✅   | **No evidence of effect**     | Multiple repos, no observed trigger |

### Observed Copilot Actor Identities

| Context                       | Login                   |
| ----------------------------- | ----------------------- |
| PR author                     | `Copilot`               |
| Issue assignee (API)          | `Copilot`               |
| Issue assignee (GraphQL node) | `BOT_kgDOC9w8XQ`        |
| workflow_run actor            | `app/copilot-swe-agent` |
| PR review author              | `Copilot`               |

## Decision

### Hard Constraints (violations cause failure)

1. **Issues MUST have at least one label before Copilot assignment** — without labels, coding agent run silently cancels.
2. **Issues MUST have a type set (Bug, Feature, or Task)** — without type, coding agent run silently cancels.
3. **Issues MUST have a non-empty body (≥20 chars)** — empty issues give Copilot nothing to work with.
4. **Max 1 Copilot-assigned issue per repo** — mass assignment causes queue saturation.
5. **Max 1 open Copilot PR per repo** — multiple PRs cause rebase conflicts and token burn.

### Lifecycle Behavior

6. **All PRs get Copilot as default reviewer** — when CI is green and no review exists, lifecycle requests `copilot-pull-request-reviewer[bot]`.
7. **All PRs auto-merge when CI green + review complete** — not just Copilot PRs. Human PRs benefit from the same pipeline.
8. **Lifecycle is sole assignment authority** — no other workflow, script, or reporter assigns Copilot. Issue-reporter creates issues with labels/type but does NOT assign.
9. **ensureIssueReady before assignment** — lifecycle adds missing labels, type, and body before assigning Copilot. Never assign an incomplete issue.
10. **No comments on issues** — no `@copilot` nudges, no assignment notifications. Assignment alone triggers the coding agent. Comments are noise.

### Stall Detection

11. **5-minute stall threshold** — if Copilot is assigned to an issue for >5 minutes with no PR created, unassign silently. The issue returns to the queue for re-pickup on the next cycle.
12. **No retry comments** — stalled issues are unassigned, not nudged. If the issue is valid, Copilot will pick it up again on reassignment.

## Gaps Remaining

- [ ] Does label alone (without type) allow Copilot to start?
- [ ] Does type alone (without label) allow Copilot to start?
- [ ] Optimal stall threshold (5min chosen pragmatically; may need tuning)
- [ ] Does body content quality affect Copilot success rate?
