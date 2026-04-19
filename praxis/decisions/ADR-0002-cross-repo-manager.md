# ADR-0002: Cross-Repo Manager Design

**Status:** Accepted
**Date:** 2026-03-23
**Context:** praxis-business needs to manage automation across all plures repos without per-repo manual setup.

## Evidence

| Mechanism                          | Cross-repo access      | Identity            | Rate limit  | Evidence                                              |
| ---------------------------------- | ---------------------- | ------------------- | ----------- | ----------------------------------------------------- |
| `GITHUB_TOKEN`                     | Repo-scoped only       | github-actions[bot] | 1000 req/hr | GitHub design — always scoped to triggering repo      |
| PAT (`PRAXIS_BUSINESS`)            | Configurable           | User-tied           | 1000 req/hr | Failed — secret was empty or lacked scope             |
| GitHub App (`praxis-business-bot`) | All installed repos    | App identity        | 5000 req/hr | App ID 3158960, Installation 118270264 — **works**    |
| Org secrets                        | Available to all repos | N/A                 | N/A         | `PRAXIS_BOT_PRIVATE_KEY`, `PRAXIS_WEBHOOK_SECRET` set |

### Tested Facts

- GitHub App with `actions/create-github-app-token@v1` → installation token: **works** for cross-repo operations
- Cross-repo manager pushed `auto-approve-copilot-runs.yml` to pares-agens via App token: **works** (PR #234 auto-merged)
- Cross-repo manager closed 7 duplicate issues in pares-agens: **works**
- Cross-repo manager reran 13 stuck `action_required` runs: **works**
- App auto-merges its own onboarding PRs (not via Copilot lifecycle): **works**

### Tiering

| Tier | Repos                                                                     | Management Level                           |
| ---- | ------------------------------------------------------------------------- | ------------------------------------------ |
| 1    | pares-agens, praxis-business, FinancialAdvisor, design-dojo, qa, + 5 core | Full lifecycle: CI, issues, PRs, workflows |
| 2    | 6 repos                                                                   | Monitoring only: CI health, stale PRs      |
| 3-4  | Remaining                                                                 | Excluded from active management            |

## Decision

1. GitHub App is the sole cross-repo identity — no PATs for automation
2. `actions/create-github-app-token@v1` in every workflow needing cross-repo access
3. Org secrets for App credentials — no per-repo secret configuration
4. Cross-repo manager auto-merges its own onboarding PRs
5. Tiering determines management scope — tier 1 gets full lifecycle, tier 2 monitoring only

## Constraints

- Cross-repo manager MUST use App token, never `GITHUB_TOKEN` or PAT
- Onboarding PRs MUST NOT modify existing workflows — only add new ones
- Auto-merge of own PRs requires passing CI checks
