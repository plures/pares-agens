# ADR-0018: Approval-Card Parity — Remaining Scope After PR #621

**Status:** PROPOSED (design-only, no implementation in this PR)

**Date:** 2026-07-23

**Epic:** `pares-agens:parity-approval-cards` (P1)

**Enforcement:** Governed by `plures-dev-guide` procedures —
`resolve-change-context`, `development-lifecycle` (px-first,
no-stub-completion-honesty), `merge-gate`. No stub code, no mocked
behavior, no fake "done" claims. This ADR is design only; each numbered
item below is its own follow-up implementation PR.

## Context

PR #621 (`feat/approval-card-wiring`, MERGED) wired the **resolve** half
of the block-and-await approval loop end-to-end for Telegram:

- Bumped all 15 `pares-radix` edges to `v1.55.33`/`v1.55.34` so
  `pares_radix_core::approval::ApprovalRegistry` (from pares-radix #472)
  is a single shared crate version across the workspace.
- Threaded one `Arc<ApprovalRegistry>` from `runtime.rs` into
  `ProcedureToolDispatcher` and into the `TelegramAdapter`.
- Telegram's `approval:yes|no:{token}` callback now calls
  `ApprovalRegistry::resolve(token, decision)`. If the token is a live
  pending-tool-approval, it wakes the waiter (channel-agnostic per
  C-TEST-002 — the decision logic lives in `radix-core::approval`, not
  in `telegram.rs`). If the id is not a live token (e.g. a turn-level
  approval prompt, not a tool-call token), `resolve()` is a documented
  no-op and the code falls through to the existing steer/dispatch path
  — zero regression, unit-tested (3 new tests in `turn_ux.rs`).

PR #621's own body is explicit about what is **not** done
(C-NOSTUB-001): the **register+resolve seam is live**, but the **block
half is honestly unbuilt** — `runtime.rs`'s
`GovernanceVerdict::AllowWithApprovalWarning` branch registers a pending
approval (so the token exists for a button press to resolve against),
then immediately self-resolves it `Allow` and proceeds — it never
actually calls `pending.wait()` to suspend the tool call. This ADR
inventories every remaining gap between that state and true "approval
card" parity across the product, and proposes design (not code) for
each.

## Gaps found by code inspection (ground truth, this pass)

1. **`runtime.rs:1063-1096`** (`ProcedureToolDispatcher::call_tool`,
   `AllowWithApprovalWarning` arm): registers a pending approval, logs
   the token, then unconditionally resolves it `Allow` and continues —
   comment: *"NOTE (honest scope): full mid-tool-call blocking...is
   gated on outbound-card seam."* **This is the P0 gap** — nothing a
   user does today can actually Deny a tool call before it runs; the
   Allow/Deny buttons on the *tool-approval* path are decorative until
   this is closed. (Turn-level approval prompts detected by
   `is_approval_prompt()` / rendered by `approval_keyboard()` in
   `telegram.rs` are a separate, already-functional text-prompt path —
   see Non-Gap below.)

2. **Single-channel wiring.** `approval_registry: Option<Arc<...>>` is
   a field only on `TelegramAdapter` (`telegram.rs:703`). No other
   adapter in `crates/channels/src/` (`stdin.rs`, `stdio_spine.rs`,
   `http_spine.rs`, `tauri_ipc.rs`, `telegram_spine.rs`) has any
   approval-registry field, callback route, or card-rendering
   equivalent. `runtime.rs:5170` explicitly stubs a registry for the
   TUI path with the comment *"TUI mode has no interactive-card adapter
   yet...Resolve routing is a no-op here"* — i.e. today TUI users have
   no way to Allow/Deny at all once block-and-await is enabled; every
   `AllowWithApprovalWarning` tool call in TUI mode would hang forever
   once #1 is fixed (or resolve against a registry no adapter ever
   reads from).

3. **No channel-agnostic approval-card render contract.** ADR-0017
   (Teams/Slack, also PROPOSED) independently identifies that
   Telegram-specific concerns (inline keyboards, `ChatId`/`MessageId`)
   are baked directly into `telegram.rs` with no
   `pares_radix_core::channel_contract` abstraction for interactive
   cards. `approval_keyboard()` builds a Telegram
   `InlineKeyboardMarkup` inline in the adapter; there is no
   `ApprovalCard` render trait/struct any other adapter could implement
   against. Full parity requires this abstraction to exist once, not
   be re-invented per channel (Teams/Slack, HTTP spine, Tauri IPC, TUI).

4. **No expiry / TTL on pending approvals.** `ApprovalRegistry` (in
   pares-radix) is a plain `HashMap<token, oneshot::Sender>` with no
   timeout. A card that is dismissed, lost (process restart drops the
   whole in-memory registry — no persistence), or simply never acted on
   leaves the entry in `waiters` forever (`pending_count()` never
   decrements) and — once #1 is fixed — leaves the *awaiting tool call*
   blocked forever. There is no fail-safe timeout-to-Deny.

5. **Process-restart / crash recovery.** `ApprovalRegistry` state is
   pure in-memory (`Arc<Mutex<HashMap<...>>>`) with zero persistence.
   A restart between "card rendered" and "user presses button" strands
   the token — the button, if pressed after restart, calls `resolve()`
   against a *new* empty registry and silently no-ops (falls through to
   the turn-level steer path per #621's own fallback, which is
   *misleading* once #1 makes registry-tokens meaningful — the user
   sees "approved"/"rejected" acked in Telegram but the original tool
   call, if it survived as an orphaned suspended future, never wakes).

6. **Multi-request / stale-press UX.** `resolve()` is idempotent
   (second press on an already-resolved token safely returns `false`),
   but the Telegram card is never edited to reflect resolution (no
   "✅ Approved" / "❌ Denied" edit-in-place), so a user can press the
   same button repeatedly, or two users in a group chat can race on the
   same card, with no visual feedback distinguishing "already resolved"
   from "still pending."

7. **Audit-trail integration.** `pares_radix_core` ships an
   audit/ledger concept (`crates/audit`, `crates/core` — see
   `channel_contract.rs`/`event.rs`) but nothing in the current
   approval flow emits an auditable event recording who
   approved/denied which tool call, when, or from which channel/user
   identity. Compliance/observability parity requires every
   `ApprovalRegistry::resolve` call to also emit a durable audit record
   (see `azure-compliance`/`appinsights-instrumentation` conventions
   used elsewhere in this org for the shape such an event should take).

## Non-gaps (already correct, do not re-litigate)

- Turn-level approval-prompt buttons (`is_approval_prompt` +
  `approval_keyboard`, pre-existing) already work today via the
  steer/dispatch fallback — no change needed there.
- `parse_callback` / `ControlAction` parsing is channel-agnostic, unit
  tested, and handles `Stop` vs `Approve`/`Reject` and colon-bearing
  ids correctly. No further work needed on the parsing layer itself.
- The resolve-seam contract in `pares_radix_core::approval` (register/
  resolve/pending_count, fail-closed-to-Deny on dropped sender) is
  solid and channel-agnostic (C-TEST-002 compliant); it is the correct
  foundation to build #1–#7 on top of, not something to replace.

## Proposed scope for follow-up implementation epics (design only)

Each item below is intentionally sized as an independent PR so no
future patch re-triggers a full 15-crate radix pin bump unless the
radix-core API itself changes.

### P0 — Close the block-and-await loop (blocks everything else)
- In `runtime.rs`'s `AllowWithApprovalWarning` arm, replace the
  immediate self-resolve-`Allow` with an actual
  `let decision = pending.wait().await;` gate, but **only after** an
  out-of-band path exists to surface the card *while the tool call is
  suspended* — today the dispatcher has no handle back into the active
  channel adapter mid-`call_tool` (the "stack-local for now" event-spine
  limitation #621 already flagged). This requires either:
  - (a) passing an `outbound: Arc<dyn ApprovalCardSink>` handle into
    `ProcedureToolDispatcher` alongside `approval_registry`, so
    `call_tool` can push a card-render request to the active channel
    before awaiting, or
  - (b) routing the card through the existing `EventSpineHandle` as a
    new `Event::ApprovalRequested { token, chat_id, tool_name, summary }`
    variant, consumed by whichever adapter owns that chat/session.
  (b) is preferred: it reuses the event-spine machinery ADR-0017
  already flags as the right layer for interactive-action routing, and
  avoids a bespoke sink trait per adapter.

### P1 — Channel-agnostic `ApprovalCard` render contract
- Add a render-agnostic `ApprovalCard { token, tool_name, summary }`
  type to `pares_radix_core::channel_contract` (or an adjacent module)
  with adapter-specific renderers (Telegram inline keyboard today; a
  future Teams Adaptive Card / Slack Block Kit per ADR-0017) so #621's
  Telegram-only `approval_keyboard()` becomes one implementation of a
  shared trait, not a one-off.
- Retrofit `stdin`/`stdio_spine`/`http_spine`/`tauri_ipc` with at least
  a minimal card representation (e.g. stdin: print
  `[APPROVE token] y/n?` and read the next line; HTTP spine: a
  `POST /v1/approvals/{token}` endpoint; Tauri IPC: a new IPC event) so
  no channel silently hangs forever once P0 lands.
- TUI: needs an actual interactive-card widget before it can safely
  enable block-and-await; until then, `runtime.rs`'s existing
  `ApprovalRegistry::new()` stub for TUI (currently a documented no-op)
  should stay a no-op and TUI should explicitly **skip** the block
  (auto-Allow, same as today) rather than deadlock — this must be a
  deliberate, logged decision per-channel, not an accidental hang.

### P2 — Expiry & fail-safe timeout
- Add an optional TTL to `ApprovalRegistry::register` (e.g.
  `register_with_ttl`) with a background sweep that resolves
  timed-out entries to `Deny` (fail-closed) and emits a tracing event.
  Default TTL should be configurable per-deployment (env var or config,
  consistent with existing `HttpSpineConfig`-style config structs).

### P3 — Card-state feedback (edit-in-place) + idempotent-press UX
- After a successful `resolve()`, the resolving adapter should edit the
  original card message (Telegram: `edit_message_reply_markup`/
  `edit_message_text`) to show the terminal state and remove the
  buttons, preventing repeat presses and clarifying race outcomes in
  group chats.

### P4 — Audit trail for approval decisions
- Every `ApprovalRegistry::resolve` call that returns `true` (a real
  decision on a real pending tool call) should also emit a durable
  audit record: who (channel + user id), what (tool name, summary),
  when, and the decision — feeding into whatever ledger/audit crate
  the org already uses (`crates/audit`), and instrumented per the
  `appinsights-instrumentation` skill conventions if telemetry export
  is in scope for this deployment.

### P5 — Persistence across restart (stretch, only if #2–#4 land first)
- If block-and-await tool calls can legitimately outlive a process
  restart (e.g. long-running approvals awaiting an offline reviewer),
  persist pending-approval metadata (token, tool_name, summary, chat
  context) to the existing CRDT store (`pares_radix_core::CrdtStore`,
  already used by `event_spine.rs`) so a restart doesn't silently
  orphan in-flight approvals. Defer until #2 (TTL) exists, since most
  real-world staleness should be caught by expiry rather than requiring
  full persistence.

## Verification plan for follow-up PRs

Each item above should land with:
- Unit tests exercising the specific gap (as #621 did for the resolve
  seam) with no external channel required (C-TEST-002).
- An explicit "honest scope" note in the PR body for anything still
  deferred, naming which numbered item(s) here it satisfies and which
  it does not.
- No `cargo build`/`clippy` regressions across the full 15-crate
  workspace; a radix-core pin bump is only required if the P0/P1 items
  need new `pares_radix_core::approval` or `event_spine` API surface —
  confirm via `cargo tree -p pares_radix_core` before bumping all 15
  edges again.

## Open questions (for epic owner before implementation starts)

1. Does P0's card-surfacing mechanism belong on `EventSpineHandle`
   (new `Event::ApprovalRequested` variant) or a bespoke
   `ApprovalCardSink` trait? This ADR leans event-spine for consistency
   with ADR-0017's own open question about `Event::InteractiveAction`
   — the two designs should converge on one interactive-event pattern,
   not diverge.
2. Should TUI get a real interactive-card widget (P1) before or after
   Teams/Slack (ADR-0017) ship, given TUI is local/single-user and
   Teams/Slack are net-new channels? Recommend Teams/Slack first if
   ADR-0017 is already in flight, since P1's shared `ApprovalCard`
   trait should be designed against at least two adapter
   implementations (Telegram + one more) to avoid over-fitting the
   trait to Telegram's inline-keyboard model.
3. Is a configurable default TTL (P2) a security-relevant decision that
   needs sign-off from whoever owns `ToolGovernor`/governance policy,
   given a too-long TTL effectively becomes "block forever" and a too-
   short one becomes "silently auto-deny most real approvals"?

## References

- PR plures/pares-agens#621 (`feat/approval-card-wiring`, MERGED) —
  the resolve-seam this ADR builds on.
- `crates/agens-plugin/src/agent_commands/runtime.rs` — dispatcher
  wiring, `AllowWithApprovalWarning` arm (P0 gap), TUI stub (P1 note).
- `crates/channels/src/telegram.rs`, `turn_ux.rs` — Telegram-only
  approval-callback wiring and parsing (channel-agnostic parsing layer,
  channel-specific rendering/resolve call site).
- `crates/channels/src/{stdin,stdio_spine,http_spine,tauri_ipc,telegram_spine}.rs`
  — channels with zero approval-card support today (P1 scope).
- pares-radix `crates/radix-core/src/approval.rs` (`ApprovalRegistry`,
  #472) — the channel-agnostic seam this whole epic hangs off.
- pares-radix `crates/radix-core/src/event_spine.rs` — candidate home
  for the P0 out-of-band card-surfacing event.
- `docs/decisions/ADR-0017-teams-slack-channel-adapters.md` — sibling
  PROPOSED ADR; its open question on `Event::InteractiveAction` should
  be resolved jointly with this ADR's P0 design.
- `plures-dev-guide` procedures: `no-stub-completion-honesty.px`,
  `development-lifecycle.px`, `merge-gate.px`.
