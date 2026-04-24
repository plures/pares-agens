# Pares Agens Roadmap

## Role in OASIS
Pares Agens is the agent runtime that powers OASIS’s decentralized multi-agent orchestration: task decomposition, capability-based routing, and execution across local + cloud resources. It is the control plane that connects PluresDB procedures, PluresLM memory, and tool execution into a coherent, autonomous system.

## ✅ v0.5.0 — Dogfood Ready (10/10 closed)
## ✅ v0.6.0 — Cognitive Architecture (7/7 closed)
## ✅ v0.6.0 — Production Hardening (10/10 closed)
## ✅ v0.7.0 — Multi-Host Sync (8/8 closed)

## v0.8.0 — Ecosystem
- [ ] Audit trail — Chronos decision logging (#515, partial)

## v0.9.0 — Desktop Experience
- [ ] Hotkey activation (#516, PR #537 in progress)
- [ ] Clipboard integration (#517)
- [ ] Notification actions (#518)

## v1.0.0 — The Replacement (OpenClaw feature parity + superiority)

### Critical Path (blocks daily workflow)

| # | Feature | Leverages | Priority |
|---|---|---|---|
| #538 | **crates/agenda — task scheduler** | automation-infrastructure scripts, pares-rector patterns | 🔴 P0 |
| #539 | **pares-manus integration — browser/GUI control** | pares-manus v0.5.0 (7 crates, cross-platform) | 🔴 P0 |
| #540 | **Session management — multi-session, compaction** | pares-saxum memory module | 🔴 P0 |

### Core Features

| # | Feature | Status |
|---|---|---|
| #502 | NixOS service on praxisbot | Blocked (needs praxisbot rebuild) |
| #503 | Context management — topic detection | Open |
| #504 | Proactive org monitoring | Depends on #538 (scheduler) |
| #505 | Direct code fixes | Open |
| #506 | Self-update via NixOS rebuild | Open |
| #508 | Production hardening — error recovery | Open |
| #578 | Telegram: /approve | Open (PR #601 in progress) |
| #579 | Telegram: /cron | Open |
| #580 | Telegram: /sessions | Open |
| #581 | Telegram: /tools | Open |
| #582 | Telegram: /web | Open |
| #602 | Fix CI failures | Open |

### Polish & Ecosystem

| # | Feature | Status |
|---|---|---|
| #519 | Opt-in telemetry | Open |
| #520 | Plugin API stable | Open |
| #521 | Cross-platform installers | Open |
| #522 | Default procedures library | Partial (6 exported) |
| #541 | Telegram rich formatting (buttons, reactions, reply_to) | New |

## Summary

| Phase | Total | Done | Remaining |
|-------|-------|------|-----------|
| v0.5.0–v0.7.0 | 35 | 35 | 0 ✅ |
| v0.8.0 | 1 | 0 | 1 |
| v0.9.0 | 3 | 0 | 3 |
| v1.0.0 | 20 | 0 | 20 |
| **Total** | **59** | **35** | **24** |
