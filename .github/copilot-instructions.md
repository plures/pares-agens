# Copilot Instructions — Pares Agens

## You Are Working On a Reactive Agent System

Pares Agens is NOT a traditional agent framework. It is a reactive system where:
- **PluresDB procedures** define agent behavior (not imperative code)
- **PluresLM** provides memory/context
- **Praxis** validates decisions via a typed logic engine
- The runtime is a thin event loop connecting channels to the database

Read the full architecture: https://github.com/plures/development-guide/blob/main/design/PARES-AGENS.md

## Architecture Rules (NON-NEGOTIABLE)

### 1. Procedures Over Code
Agent behavior belongs in PluresDB procedures, not in `.rs` files with match statements.
- Routing logic → procedures
- Tool orchestration → procedures
- Context assembly → PluresLM procedures
- Decision gates → Praxis ledger

### 2. Reactive Over Polling
Every state transition is event-driven. No cron for core lifecycle logic.
See: https://github.com/plures/development-guide/blob/main/practices/reactive-architecture.md

### 3. Structured Observability Is Mandatory
Every module MUST have structured logging with `tracing`:
- **Model calls**: log URL, model name, message count, response status, body preview
- **Token exchanges**: log endpoint, status, expiry, API base URL
- **State transitions**: log from-state, to-state, trigger event
- **Errors**: log full context (status code, response body, request URL)

One `tracing::debug!` line is NOT observability. Use `tracing::info!` for operational events.
Future: all state transitions will emit Chronos causal diffs for full traceability.

### 4. Praxis Gates on Every Decision
No bare `if/else` for business logic. Decisions go through Praxis rules:
```rust
// WRONG: bare conditional
if ci_passed && approved { merge(); }

// RIGHT: Praxis rule
engine.rule("auto-merge-ready", {
    when: { "ci-status": "passing", "approvals": { gte: 1 } },
    then: { derive: "auto-merge-eligible", value: true }
});
```

## Organization Standards

### Source of Truth
- **Development guide**: https://github.com/plures/development-guide
  - `standards/` — commit conventions, CI/CD, PR workflow, code style
  - `practices/` — copilot delegation, reactive architecture, automation-first
  - `design/` — PARES-AGENS.md, THREE-AGENT-COGNITIVE-ARCHITECTURE.md, DEVELOPMENT-COORDINATOR.md
  - `lessons-learned/` — past mistakes to avoid (read these!)
  - `best-practices/praxis-adoption.md` — how to integrate Praxis

### Conventional Commits (REQUIRED)
```
<type>[optional scope]: <description>
```
Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `chore`, `ci`, `build`
Breaking changes: add `!` after type or `BREAKING CHANGE:` in footer.

### PR Titles — use conventional commit format (they become the squash commit).

### Squash merge — always. Clean single commit on `main`.

### Tests required — all new features need tests. Bug fixes need a failing test first.

## Plures Stack Reference

| Crate/Package | Purpose | Docs |
|---|---|---|
| `pluresdb` | Graph DB + vector search + reactive procedures | https://github.com/plures/pluresdb |
| `@plures/praxis` | Typed logic engine (facts → rules → events → state) | https://github.com/plures/development-guide/blob/main/tools/praxis.md |
| `plureslm` | Memory recall/capture with native embeddings | PluresLM crate in this repo |
| `chronos` | Graph-native state chronicle (causal diffs) | https://github.com/plures/chronos |
| `design-dojo` | UI component library (Svelte 5) | https://github.com/plures/design-dojo |
| `unum` | Svelte 5 reactive bindings for PluresDB | https://github.com/plures/unum |

## What NOT To Do

- Do NOT add `#[allow(...)]` to suppress warnings — fix the underlying issue
- Do NOT create sub-PRs that depend on other PRs
- Do NOT touch files outside the requested scope
- Do NOT manually bump version numbers
- Do NOT add bare `println!` or `dbg!` — use `tracing` macros
- Do NOT write imperative routing logic — use PluresDB procedures
- Do NOT skip structured logging on any I/O boundary (HTTP calls, DB queries, file ops)
- Do NOT add `eslint-disable` or `clippy::allow` without a comment explaining why

## Release Pipeline

Reusable release workflow from `plures/.github`. Do NOT manually bump versions.
Version bumps are automatic from conventional commits.
