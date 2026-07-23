# ADR-0016: Unified Permission-Mode + Exec-Approval Layer

**Status:** Proposed (design only; no code changes in this pass; per C-DEV-001)
**Date:** 2026-07-23
**Deciders:** TBD

## Context

pares-agens currently gates risky actions ad hoc:

- `crates/channels/src/telegram.rs` renders Approve/Reject inline keyboards
  (`gate:approve:*`, `approval:yes|no:{request_id}`) and owns the only wiring
  from a user tap back into a decision.
- `crates/channels/src/turn_ux.rs` has a **pure, channel-agnostic**
  `ControlAction::{Stop,Approve,Reject}` parser for Telegram callback_data —
  good precedent (C-TEST-002: logic is unit-testable with zero transport),
  but it is only ever *invoked* from the Telegram adapter today. Teams/Slack/
  stdio channels have no equivalent entry point.
- `crates/core/src/cerebellum/mod.rs` defines `ApprovalRequest` for a
  different purpose (cerebellum action classification), not exec-command
  gating — naming collision risk with the design below.
- The actual block-and-await approval primitive already exists and is
  **channel-agnostic by construction**: `pares-radix-core::approval`
  (`ApprovalRegistry` / `PendingApproval` / `ApprovalDecision`). Its own
  module docs state the intended contract explicitly: "No adapter is
  required to exercise it... Adapters only render the card and route the
  callback token back into `ApprovalRegistry::resolve`." This is the
  reference primitive to build on, not reinvent.
- `pares-radix-core::tool_governance::ToolGovernor` evaluates every tool call
  and returns `GovernanceVerdict::{Allow, AllowWithApprovalWarning, Blocked}`.
  Its own comment is explicit about the gap: *"Approval gates — tools marked
  `approval_required` log a warning and proceed (full approval UI is Phase
  5+)."* — i.e. `AllowWithApprovalWarning` is currently **logged, not
  enforced**. `run_command_actions.rs` confirms this: on
  `AllowWithApprovalWarning` it just `debug!`s and continues execution.
- There is no `PermissionMode` concept anywhere in pares-agens or
  pares-radix-core today (`git grep` for `PermissionMode` / `enum.*Mode`
  gating exec returns nothing). Elevated-tier semantics do not exist.

**This means the approval *primitive* (registry, fail-closed await) already
meets the channel-agnostic bar. The gap is entirely upstream/downstream of
it: (a) nothing computes whether a given command needs approval as a
function of a per-session/per-user *permission mode*, (b) `ToolGovernor`
never actually calls into `ApprovalRegistry` — the warning path is a no-op,
and (c) there is no "elevated" tier at all.**

## Reference model: OpenClaw's approval contract

Observed directly in this operating environment (the exec tool + gateway
this agent runs under) — this is the target UX/behavior to mirror, not
guess at:

1. **Every exec call declares an `ask` policy** (effectively a per-call
   permission tier) and an `elevated` boolean, separate concerns:
   - `ask` governs whether the *specific command* requires a human
     approval gate before running at all.
   - `elevated` governs whether the command runs with escalated host
     permissions once approved.
2. **Approval is synchronous and blocking from the agent's point of view.**
   A gated call returns an "approval-pending" state; the agent must present
   the exact command to the human and wait. It does not fabricate success,
   retry silently, or assume approval.
3. **Approval is granted through one explicit, unambiguous mechanism**
   (the `/approve <token>` slash command in chat) that is bound to exactly
   one pending request. There is no ambient "yes" that this agent can
   interpret on its own, and the agent is explicitly instructed never to
   run the approval command itself — only the human issues it.
4. **Approval scope is single-command, not session-wide.** Approving one
   elevated/gated command does not implicitly authorize a different command
   later, even in the same turn. (Mirrors "treat allow-once as
   single-command only" already codified in this agent's own operating
   rules.)
5. **The channel is irrelevant to the contract.** The same block-and-wait /
   token/approve shape must work whether the surface is a chat UI, a CLI,
   or an API caller — the gate lives at the tool-execution boundary, not in
   any one channel's transport code.

Mapping this back onto pares-agens's existing primitives: `ask`/gated ↔
`GovernanceVerdict::AllowWithApprovalWarning` (needs to actually block, not
warn) + `ApprovalRegistry`; `/approve <token>` ↔ `ApprovalRegistry::resolve`
already keyed by token; single-command scope ↔ each `ApprovalRequest` is
already single-token/single-call by construction. `elevated` is the one
tier missing entirely and needs a new concept (see below).

## Decision

### 1. `PermissionMode` enum (new, in `pares-radix-core`)

```rust
pub enum PermissionMode {
    /// Read-only / dry-run: no run_command, no file mutation. Everything
    /// that would mutate state or execute a command is rejected outright
    /// (not queued for approval — Plan mode never executes).
    Plan,
    /// Normal operation: ToolGovernor policy decides per-tool whether a
    /// call requires approval (`approval_required`); default policies stay
    /// unattended for read-only/low-risk tools.
    Default,
    /// Every gated tool call still requires an explicit approval, but the
    /// caller has pre-declared willingness to review promptly (UX hint
    /// only — does not change what gets gated).
    Supervised,
    /// Explicit, time-boxed "auto-approve" grant for a specific tool +
    /// scope (e.g. "auto-approve read_file for the next 10 minutes").
    /// Never a blanket, indefinite bypass — every AutoApprove grant is a
    /// PluresDB record with an expiry and an audit trail of what it
    /// authorized.
    AutoApprove { tool_name: String, expires_at: DateTime<Utc>, granted_by: String },
}
```

`PermissionMode` is **per-session** (keyed by the same session/conversation
id already used for turn state), not global and not per-adapter. It answers
"what tier is this session running under," distinct from "elevated," which
answers "does *this specific call* need escalated host permissions."

### 2. `Elevated` is a per-call flag, not a mode

Mirroring the reference model's separation of `ask` vs `elevated`:
`elevated: bool` travels on the individual tool-call request (alongside the
existing `ExecRequest`), not on the session's `PermissionMode`. A call can
be `Default` mode + `elevated: true` (a normally-unattended session asking
for one escalated action) — these are orthogonal axes and must stay
orthogonal in the implementation; do not collapse `elevated` into a mode
variant.

- `elevated: true` **always** forces `GovernanceVerdict::AllowWithApprovalWarning`
  at minimum, regardless of the tool's default policy — elevation can only
  ever *raise* the approval bar, never lower it.
- Granting an elevated request is a **single-command grant** (mirrors the
  reference model's "allow-once is single-command only" rule) — it never
  implicitly covers a later command, even an identical one, and even in
  `AutoApprove` mode. `AutoApprove` grants (§1) apply only to
  non-elevated calls; `elevated` always requires a fresh approval.

### 3. Where the gate hooks in (channel-agnostic, C-TEST-002)

The gate lives at **exactly one seam**: inside `ToolGovernor::check` /
its caller in `run_command_actions.rs` (and the equivalent seam for
non-`run_command` tools going through the same governance path) —
**not** in any channel adapter. Concretely:

1. `ToolGovernor::check` gains access to the session's current
   `PermissionMode` (passed in, not looked up ad hoc) and the call's
   `elevated` flag, and returns a verdict that **actually blocks** rather
   than the current log-and-continue `AllowWithApprovalWarning`.
2. When the verdict requires approval, the governance layer calls
   `ApprovalRegistry::register` (already channel-agnostic — no code
   change needed to that primitive) and **awaits** `PendingApproval::wait()`
   before proceeding, exactly like the existing unit tests already
   exercise it with zero transport in the loop.
3. Each channel adapter's ONLY job is: render the `ApprovalRequest` as a
   card/message in its native UI (Telegram inline keyboard, Teams adaptive
   card, Slack block, or a stdio prompt), and route the user's decision
   back into `ApprovalRegistry::resolve(token, decision)`. Adapters MUST
   NOT contain any decision logic, timeout logic, or default-outcome
   logic of their own — that would silently reintroduce a
   Telegram-only gate. `turn_ux.rs`'s existing pure
   `ControlAction::{Approve,Reject}` parser is the template: adapter-side
   code is parse + render + forward only.
4. A dropped/expired registration **fails closed to Deny** — this already
   holds in `pares-radix-core::approval` today and must not regress.

This directly satisfies the requirement that Telegram/Teams/Slack/stdio all
trigger the same approval flow: they all call the same
`ApprovalRegistry::register/resolve`, none of them decide anything.

### 4. Elevated request lifecycle (request → grant → audit)

1. **Request**: a tool call sets `elevated: true` on its `ExecRequest` (or
   equivalent). `ToolGovernor` sees this and forces an approval gate
   regardless of `PermissionMode`.
2. **Grant**: identical mechanism to a normal approval — a single token,
   single command, human-in-the-loop `Allow`/`Deny` via whichever adapter
   is live. No separate "elevated-grant" side channel; elevation only
   changes *whether* a gate fires, never *how* it's granted.
3. **Audit**: every elevated request/grant/deny is written to PluresDB
   (see §5) as an immutable append record — who requested it, the exact
   command, who granted/denied it, timestamp, and the resolved decision.
   This is separate from and in addition to whatever transient logging
   `crates/audit/src/store.rs` already does for tool calls generally; the
   elevated-audit record must be queryable on its own (e.g. "show me every
   elevated grant in the last 7 days") without depending on adapter-side
   chat history.

### 5. PluresDB integration (C-PLURES-003 / C-PLURES-004)

No ad-hoc structs for durable state — following existing `.px` procedure patterns
(e.g. `praxis/procedures/dev-lifecycle.px` + `praxis/procedures/session-continuity.px`), the new keys are:

```
approvals:session:{session_id}:mode          current PermissionMode for a session
approvals:autoapprove:{session_id}:{tool}    active AutoApprove grant (with expiry)
approvals:pending:{token}                    mirror of an in-flight ApprovalRequest
                                              (registry itself stays in-memory/oneshot;
                                              this record exists so a restart doesn't
                                              silently vanish a pending request without
                                              a trace, and so cross-process/cross-node
                                              adapters can observe pending approvals)
approvals:audit:{token}                      immutable record: tool, command/summary,
                                              elevated flag, requested_at, decision,
                                              decided_by, decided_at
```

Per C-PLURES-004, the actions that read/write these keys are named Rust
boundary actions invoked from `.px` (mirroring `dev-lifecycle.px`'s separation of orchestration in `.px` from side effects in Rust action handlers), not raw structs scattered through adapter code.
A companion `.px` procedure (e.g. `praxis/procedures/approvals.px`) should
be authored in the **implementation** pass to define the decision flow — out of scope for this ADR,
but the key layout above is designed to be `.px`-addressable from the
start.

### 6. Naming collision to resolve during implementation

`crates/core/src/cerebellum/mod.rs::ApprovalRequest` already exists for an
unrelated purpose (cerebellum action classification). The new
exec-approval `ApprovalRequest` type already lives in
`pares-radix-core::approval` under a different crate, so there is no
compile collision, but human readers will conflate the two. Implementation
should either rename the cerebellum type or add crate-qualified
re-exports/docs at the pares-agens boundary to disambiguate.

## Consequences

- `GovernanceVerdict::AllowWithApprovalWarning` changes from a logged
  no-op to an actual blocking gate — this is a **behavior change** for any
  existing tool policy with `approval_required: true` and needs a
  migration note / changelog entry when implemented (currently zero
  built-in policies set `approval_required: true`, so no default policy is
  affected today — but any custom `ToolPolicy` records already stored in
  PluresDB with `approval_required: true` will newly start blocking).
- Every channel adapter (Telegram now; Teams/Slack/stdio later) gets a
  strictly smaller responsibility: render + forward, never decide. This is
  a net simplification versus today's Telegram-only logic.
- `PermissionMode` and `elevated` are orthogonal from day one, preventing
  the common design trap of conflating "how cautious is this session" with
  "does this one call need escalation."

## Open Questions (need a human call before implementation)

1. **Plan mode's exact boundary.** Should `Plan` reject write/exec tools
   outright (as drafted above) or queue them as always-pending approvals
   that never auto-resolve? OpenClaw's own reference behavior wasn't fully
   observable from this environment for a "plan-only" tier — needs an
   explicit decision.
2. **AutoApprove grant surface.** Who is authorized to create an
   `AutoApprove` grant — any user the adapter allows, or only allow-listed
   admins (mirrors the existing `PARES_TELEGRAM_UPDATE_ALLOWED_USERS`
   gate)? This determines whether `AutoApprove` needs its own permission
   check layered on top of the mode itself.
3. **Approval timeout/expiry policy.** `PendingApproval` currently blocks
   indefinitely until resolved or the registry is dropped. Should there be
   a default timeout after which a pending approval auto-denies (fail
   closed) even without a process restart? OpenClaw's own model didn't
   surface an explicit numeric timeout to mirror here — needs a decision
   (proposal: default deny after N minutes, configurable per tool policy).
4. **Retroactive audit backfill.** Should the `approvals:audit:*` PluresDB
   trail start capturing pre-existing Telegram `gate:approve:*` history, or
   only new records from the implementation cut-over forward? Affects
   whether a migration script is in scope for the implementation pass.

## Implementation scope (deferred — NOT this pass)

Per C-DEV-001 / the pares-radix-dev-lifecycle hard gate, no code changes
land with this ADR. The implementation pass should follow the staged
orchestration (analyze → fix → test → deploy → verify) and produce, in
order: (a) `PermissionMode` + `elevated` types in `pares-radix-core`, (b)
the actual blocking hook in `ToolGovernor`/`run_command_actions.rs`, (c)
the PluresDB-backed key layout in §5 plus a `.px` procedure, (d) adapter-side
render/forward wiring for Telegram (existing) then Teams/Slack/stdio, (e)
tests exercising the full flow with **zero adapter code in the loop**
(per C-TEST-002), matching the style of `pares-radix-core::approval`'s own
existing unit tests.
