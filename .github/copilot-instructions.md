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

### 3. Structured Observability Is Mandatory
Every module MUST have structured logging with `tracing`:
- **Model calls**: log URL, model name, message count, response status, body preview
- **Token exchanges**: log endpoint, status, expiry, API base URL
- **State transitions**: log from-state, to-state, trigger event
- **Errors**: log full context (status code, response body, request URL)

One `tracing::debug!` line is NOT observability. Use `tracing::info!` for operational events.

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

### 5. Design-Dojo for ALL UI
All UI components MUST come from `@plures/design-dojo`. No raw HTML elements in application code. If a component doesn't exist, build it in design-dojo first, then import.

### 6. Praxis-Composed Applications
Apps are wholly composed of praxis primitives:
- Every decision = a Rule with a Contract
- Every state change = an Event processed by the Engine
- Every UI component = design-dojo, generated from schemas
- Every data operation = PluresDB graph write

## Organization Standards

### Source of Truth
- **Development guide**: https://github.com/plures/development-guide
  - `standards/` — commit conventions, CI/CD, PR workflow, code style
  - `practices/` — copilot delegation, reactive architecture, automation-first
  - `design/` — PARES-AGENS.md, THREE-AGENT-COGNITIVE-ARCHITECTURE.md
  - `lessons-learned/` — past mistakes to avoid (READ THESE)

### Automation Rules (ABSOLUTE)

**Automation changes go straight to code.** Never create GitHub issues for workflow/CI/release pipeline/lifecycle changes. Implement directly — commit and push. Issues are for feature work and bugs only.

**Zero nudges.** No `@copilot` comments, no retry comments. If stalled: close → recreate → reassign.

**Single assignment authority.** Only the lifecycle workflow assigns Copilot.

### Conventional Commits (REQUIRED)
```
<type>[optional scope]: <description>
```
Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `chore`, `ci`, `build`
Breaking changes: add `!` after type or `BREAKING CHANGE:` in footer.

### Squash merge — always. Tests required — all new features need tests.

## Plures Stack Reference

| Crate/Package | Purpose | Docs |
|---|---|---|
| `pluresdb` | Graph DB + vector search + reactive procedures | https://github.com/plures/pluresdb |
| `@plures/praxis` | Typed logic engine (facts → rules → events → state) | https://github.com/plures/development-guide/blob/main/tools/praxis.md |
| `plureslm` | Memory recall/capture with native embeddings | PluresLM crate in this repo |
| `chronos` | Graph-native state chronicle (causal diffs) | https://github.com/plures/chronos |
| `design-dojo` | UI component library (Svelte 5) | https://github.com/plures/design-dojo |
| `unum` | Svelte 5 reactive bindings for PluresDB | https://github.com/plures/unum |
| `pares-radix` | Application shell + plugin loader | https://github.com/plures/pares-radix |
| `pares-modulus` | Plugin registry (gated, manifest-validated) | https://github.com/plures/pares-modulus |

## What NOT To Do

- ❌ NO `#[allow(...)]` to suppress warnings — fix the underlying issue
- ❌ NO creating GitHub issues for automation/workflow/CI changes — implement directly
- ❌ NO sub-PRs that depend on other PRs
- ❌ NO touching files outside the requested scope
- ❌ NO manually bumping version numbers
- ❌ NO bare `println!` or `dbg!` — use `tracing` macros
- ❌ NO imperative routing logic — use PluresDB procedures
- ❌ NO skipping structured logging on any I/O boundary
- ❌ NO raw HTML elements — use design-dojo components
- ❌ NO nudging Copilot with comments — close and recreate if stalled
- ❌ NO cron jobs for orchestration — use reactive procedures
- ❌ NO bare if/else business logic — use Praxis expectations

## Release Pipeline

Reusable release workflow from `plures/.github`. Supports `target_version` for milestone-driven releases. Do NOT manually bump versions.

### When in Doubt
1. Check the development guide
2. Look for existing ADRs in `.praxis/decisions/`
3. Ask before breaking established patterns
