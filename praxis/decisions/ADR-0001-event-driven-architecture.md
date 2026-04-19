# ADR-0001: Event-Driven Architecture for Plures Automation

**Status:** Accepted
**Date:** 2026-03-23
**Context:** Migrated from cron-based Actions workflows to event-driven Deno Deploy webhook relay to reduce $200/day Actions budget burn.

## Evidence

| Approach                       | Cost                   | Latency  | Reliability                  | Evidence                                 |
| ------------------------------ | ---------------------- | -------- | ---------------------------- | ---------------------------------------- |
| Actions cron (15min × 5 repos) | 480 runs/day, ~$200/mo | 0-15 min | High (GitHub SLA)            | 373 runs in 24h observed 2026-03-23      |
| Deno Deploy webhook relay      | 0 Actions runs         | <5 sec   | Deno Deploy free tier uptime | Test issue #238 → dispatch in seconds    |
| Hybrid (cron + webhook)        | Reduced but nonzero    | Mixed    | Higher                       | Intermediate state before full migration |

### Tested Facts

- GitHub App webhook → Deno Deploy relay → `repository_dispatch` on praxis-business: **works** (validated with test issue #238)
- HMAC signature verification on Deno Deploy: **works**
- GitHub App JWT auth (PKCS#1→PKCS#8 conversion at runtime): **works** (commit `f00ecb9`)
- Installation token auto-refresh (1hr TTL, 5min buffer): **works**
- `handleActionRequired()` replaces auto-approve cron: **deployed, untested at scale**
- `handleCopilotPRClosed()` replaces queue-manager cron: **deployed, untested at scale**
- `handleCopilotFailure()` replaces retry-copilot cron: **deployed, untested at scale**

### Unknown

- Deno Deploy free tier limits under sustained webhook volume
- Webhook delivery reliability during GitHub incidents
- Whether `repository_dispatch` events are guaranteed delivery

## Decision

1. All automation logic moves to Deno Deploy webhook handlers (event-driven, zero Actions cost)
2. Actions workflows remain only for push/PR-triggered CI (inherently event-driven)
3. No cron workflows under 1h frequency (praxis expectation `no-cron-under-1h`)
4. `copilot-nudge` OpenClaw cron is the fallback safety net for gaps in webhook coverage

## Constraints

- Webhook relay MUST verify HMAC signatures on all incoming events
- GitHub App tokens (5000 req/hr) used for all API calls, never PATs (1000 req/hr)
- Event handlers MUST be idempotent (webhooks can be delivered multiple times)
