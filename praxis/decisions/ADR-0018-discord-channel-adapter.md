# ADR-0018: Discord Spine Channel Adapter

**Status:** Proposed (design only; no code changes in this pass; per C-DEV-001)
**Date:** 2026-07-24

## Context

This ADR closes the highest-value remaining gap identified by the
`pares-agens:openclaw-parity` matrix (`tmp/pares-agens-openclaw-parity-matrix.md`,
generated 2026-07-20 @ pares-agens `fe57071`):

> **Channel: Discord** | ❌ | Only comment mentions (runtime.rs,
> prompt_builder.rs docstrings). NO adapter. → epic
> `parity-discord-adapter` (queued p2).

Of the four registered gaps in that matrix (approval cards, turn-UX/
steering, Discord, pluresLM memory backend), Discord is the only **fully
missing capability** — the other three are `🟡 in_progress` epics with
substantial existing plumbing. Discord is a `❌` with zero adapter code,
making it the cleanest, highest-leverage remaining unit of parity work:
closing it converts pares-agens's channel surface from "Telegram + local
dev channels" to "Telegram + Discord + local dev channels," matching
OpenClaw's own multi-platform channel story.

### Related prior art — do not duplicate

`docs/decisions/ADR-0017-teams-slack-channel-adapters.md` (epic
`pares-agens:channels-teams-slack`, P1) already designed a
channel-agnostic core (`ChannelIdentity`, `ChannelAuth`,
`ChannelContract` extensions, `renderers::approval`) built against the
**older** `ChannelAdapter` trait (`crates/channels/src/adapter.rs`) for
Teams (webhook/Bot-Framework-based) and Slack (Socket-Mode-based) —
platforms whose auth/transport model (tenant OAuth2, webhook signature
verification, adaptive cards / Block Kit) is materially heavier than a
simple bot-token gateway client. **Discord does not need any of that
machinery.** Discord's auth model is a single bot token (like Telegram's),
its transport is a persistent gateway WebSocket (conceptually closer to
Telegram long-polling than to Teams' webhook-only model), and its
richest interactive-UI need (buttons) is already served by the existing
`turn_ux::ControlAction` parser. This ADR therefore deliberately builds
Discord against the **newer, already-proven `SpineChannel` trait**
(`pares-radix-core::spine::channel`, used by Telegram/HTTP/stdio today),
not against ADR-0017's `ChannelAdapter`-based core — the two efforts are
independent, target different trait generations, and neither blocks the
other. Implementation planning should note this explicitly so a future
reader doesn't assume Discord depends on ADR-0017 landing first.

### What already exists (reuse, don't reinvent)

- **`SpineChannel` trait** (`pares-radix-core::spine::channel`, vendored at
  `v1.55.34`) is the single channel-adapter contract already proven by
  three independent implementations in this repo:
  - `crates/channels/src/telegram_spine.rs` — Telegram, thin (receive →
    `SpineEvent::Inbound`; deliver → subscribe to `DeliveryRequest`).
  - `crates/channels/src/http_spine.rs` — local REST API, thin.
  - `crates/channels/src/stdio_spine.rs` — stdin/stdout, thin.

  The trait is exactly two async methods plus an id getter:
  `channel_id()`, `start_receiving(emitter) -> Result<(), ChannelError>`,
  `deliver(event) -> Result<DeliveryResult, ChannelError>`. Every existing
  adapter is "thin" by construction — no model calls, no slash-command
  logic, no history, no tool dispatch. All of that lives in spine
  procedures and is channel-agnostic already (see module doc on
  `telegram_spine.rs`: *"This replaces the fat Telegram adapter... All
  logic... lives in spine procedures. This adapter is interchangeable."*).
  A Discord adapter must follow the exact same shape — this is not a
  discretionary style choice, it is what keeps `run_serve`'s channel
  dispatch (`agent_commands/runtime.rs::run_serve`, `match channel.as_str()`
  over `"stdio" | "telegram" | "http"`, confirmed at
  `crates/agens-plugin/src/agent_commands/runtime.rs` ~line 3577) a simple,
  closed match arm rather than a place where channel-specific business
  logic leaks in.

- **Approvals path (ADR-0016)**: that ADR establishes the rule that "each
  channel adapter's ONLY job is: render the `ApprovalRequest` as a
  card/message in its native UI... and route the user's decision back
  into `ApprovalRegistry::resolve`." Discord's adapter must honor this
  from day one (native Discord message-component buttons standing in for
  Telegram's inline keyboard), even though the approval *engine* work
  itself is tracked separately in ADR-0016 — this ADR only needs to leave
  the right seam for it.

- **Turn UX (`crates/channels/src/turn_ux.rs`)**: the pure,
  channel-agnostic `ControlAction::{Stop,Approve,Reject}` parser already
  exists and is explicitly designed to be callable from any channel's
  callback/interaction payload (Telegram `callback_data` today). Discord's
  message-component `custom_id` payloads should parse through this same
  function, not a Discord-specific reimplementation.

- **`run_serve` channel dispatch** (`agent_commands/runtime.rs`) is the
  one and only place a new `"discord"` arm needs to be added, mirroring
  the existing `"stdio" | "telegram" | "http"` arms: construct the
  channel, spawn its delivery loop against
  `pipeline.subscribe_deliveries()`, wire the heartbeat runner the same
  way the `"telegram"` arm does, then call `start_receiving` and block.

### What's missing entirely

- No Discord client dependency anywhere in the workspace (`Cargo.toml`
  has `teloxide` for Telegram; no `serenity` or `twilight`).
- No `crates/channels/src/discord_spine.rs` or any Discord-specific code
  — a recursive search across `crates/**` (excluding `target`) returns
  only comment/docstring mentions of "discord" in `runtime.rs` and
  `prompt_builder.rs`, no adapter.
- No CLI wiring: `--channel discord`, `--discord-token`, or equivalent
  flags do not exist in `agent_commands/mod.rs`'s `serve-spine` arg
  builder.

## Decision

### 1. Crate choice: `serenity`

Use [`serenity`](https://github.com/serenity-rs/serenity) as the Discord
client library, gated behind a `discord` feature flag on
`pares-agens-channels` (mirroring how `teloxide` is already a direct,
always-on dependency for Telegram — but Discord should start
feature-gated to avoid forcing every consumer of the channels crate to
pull in a gateway-websocket client they may not use, especially given
Discord's stricter gateway-connection/intents model versus Telegram's
simple long-polling).

Rationale over `twilight` (the other common Rust option): `serenity`
bundles a higher-level `Client`/`EventHandler` abstraction plus built-in
message-component (button) support, which maps directly onto what the
thin-adapter shape needs (receive events, send messages, render
components) without hand-rolling gateway-frame handling — `twilight` is
lower-level and would add adapter-side complexity that the thin-adapter
philosophy explicitly wants to avoid.

### 2. `DiscordSpineChannel` — new file, mirrors `telegram_spine.rs`

`crates/channels/src/discord_spine.rs`:

```rust
pub struct DiscordSpineConfig {
    pub token: String,
    /// Discord requires explicit gateway intents; scope to the minimum
    /// needed (GUILD_MESSAGES, MESSAGE_CONTENT, DIRECT_MESSAGES).
    pub intents: GatewayIntents,
}

pub struct DiscordSpineChannel {
    config: DiscordSpineConfig,
}

#[async_trait]
impl SpineChannel for DiscordSpineChannel {
    fn channel_id(&self) -> &str { "discord" }

    async fn start_receiving(&self, emitter: PipelineEmitter) -> Result<(), ChannelError> {
        // serenity::Client with an EventHandler whose message() callback
        // constructs SpineEvent::Inbound { source: "discord", chat_id:
        // <channel_id.to_string()>, sender: <author id/username>, content,
        // metadata: { guild_id, message_id, component interactions if any } }
        // and calls emitter.emit(event).await — same shape as
        // telegram_spine's start_receiving.
    }

    async fn deliver(&self, event: &SpineEvent) -> Result<DeliveryResult, ChannelError> {
        // Mirrors run_delivery_loop's per-event dispatch in telegram_spine,
        // but as the single deliver() call SpineChannel expects (http_spine
        // and stdio_spine already implement deliver() this way; telegram_spine
        // currently only exposes run_delivery_loop() as a raw broadcast
        // consumer — Discord should implement BOTH the trait's deliver()
        // and, if a persistent bot-side loop is preferred for streaming/
        // edit-in-place semantics [see §4], an equivalent run_delivery_loop
        // following telegram_spine's precedent).
    }
}
```

Key mapping decisions (same as existing adapters, made explicit so
implementation doesn't improvise):

| Spine concept | Discord mapping |
|---|---|
| `chat_id` | Discord channel ID (numeric snowflake, stringified) |
| `sender` | Discord user ID or username (prefer ID for stability; username can change) |
| Placeholder-message edit (`metadata.placeholder_id`, used by Telegram for progressive streaming) | Discord message ID from the initial "thinking..." send, edited via `ChannelId::edit_message` — same pattern as `telegram_spine::run_delivery_loop`'s `placeholder_id` handling |
| Approve/Reject inline keyboard | Discord message components (buttons), `custom_id` payload parsed through the existing `turn_ux::ControlAction` parser — **no new parser** |
| Telegram's per-chat allow-list gate (`PARES_TELEGRAM_UPDATE_ALLOWED_USERS`) | Equivalent `PARES_DISCORD_ALLOWED_USERS` / `PARES_DISCORD_ALLOWED_GUILDS` env-driven allow-list, checked in `start_receiving` before emitting `Inbound` — same security posture, new env vars |

### 3. CLI wiring — one new arm, one new flag pair

In `agent_commands/mod.rs`'s `serve-spine` command builder, add
`--discord-token` alongside the existing `--telegram-token`. In
`agent_commands/runtime.rs::run_serve`'s `match channel.as_str()`, add a
`"discord"` arm structurally identical to the existing `"telegram"` arm:
construct `DiscordSpineChannel`, spawn its delivery loop, wire the
heartbeat runner (`HeartbeatRunner::new(...).with_pipeline_emitter(...)`,
same as Telegram), then `start_receiving` and block. Update the
`Unknown channel. Supported: stdio, telegram, http` error message to
include `discord`.

### 4. Progressive streaming — deferred, not blocking

Telegram's adapter supports progressive token streaming via
`StreamDelta` broadcast + placeholder-message editing
(`TelegramSpineChannel::with_stream`). Discord's message-edit rate limits
are stricter than Telegram's (roughly 5 edits per message per 5 seconds
under standard rate limiting vs. Telegram's more permissive edit
allowance), so naively porting the same per-token edit cadence risks
429s. **Decision: ship Discord without progressive streaming in the first
cut** (`DiscordSpineChannel::new`, no `with_stream` constructor yet) —
send a single "thinking" placeholder, then edit it once with the final
response, matching the non-streaming path Telegram already falls back to
when `stream_tx` is `None`. Streaming support is an explicit fast-follow,
not scope-creep into this ADR, and needs its own rate-aware batching
design (e.g. edit every N tokens or every T ms, whichever is less
frequent) rather than "reuse Telegram's cadence unchanged."

### 5. Approval-card rendering — placeholder text, not blocking on ADR-0016

Per ADR-0016 §3, adapters must be pure render+forward with zero decision
logic. This ADR does not depend on ADR-0016 landing first: Discord's
adapter should render `ApprovalRequest`s as message-component buttons
using the same generic request/decision shape `turn_ux.rs` already
defines, so that whenever ADR-0016's blocking gate lands, Discord "just
works" identically to Telegram with no adapter changes required. If
ADR-0016 has not landed by the time Discord implementation starts, the
button UI can still be built and tested against the existing (currently
log-only) `GovernanceVerdict::AllowWithApprovalWarning` path with no
behavior change — the button renders, forwards to
`ApprovalRegistry::resolve`, and today's no-op governance simply means
nothing is actually gated yet, exactly as it is for Telegram today.

## Consequences

- Adds one new optional dependency (`serenity`, feature-gated) to
  `pares-agens-channels`. No impact on existing Telegram/HTTP/stdio
  builds when the `discord` feature is disabled.
- `run_serve`'s channel dispatch gains a fourth arm but stays a flat
  match — no new branching complexity, no channel-specific logic outside
  the new `discord_spine.rs` file (mirrors the "adapters are thin" rule
  already enforced for the other three channels).
- New env vars (`PARES_DISCORD_TOKEN`/`--discord-token`,
  `PARES_DISCORD_ALLOWED_USERS`, `PARES_DISCORD_ALLOWED_GUILDS`) need
  documenting in the same place Telegram's equivalents are documented
  (deployment/config docs — exact location to be identified in the
  implementation pass).
- Closes the last fully-missing (❌) capability in the parity matrix;
  remaining gaps after this are all upgrades to already-in-progress 🟡
  epics (approval cards, turn-UX/steering, pluresLM memory backend), and
  the separately-scoped ADR-0017 Teams/Slack effort — none of which are
  blocked by or blocking this work.
- No overlap/conflict with ADR-0017: that ADR extends the legacy
  `ChannelAdapter`/`ChannelContract` core for Teams/Slack; this ADR
  builds directly on the newer `SpineChannel` trait Telegram/HTTP/stdio
  already use. The two channel-core lineages currently coexist in the
  codebase (`adapter.rs` vs. `spine::channel`) — reconciling them, if
  ever needed, is out of scope here and should be its own ADR if the
  duplication becomes a maintenance problem.

## Open Questions (need a human call before implementation)

1. **Guild scope**: should the first cut support DMs only, guild channels
   only, or both? OpenClaw's own Discord-equivalent behavior wasn't
   observable from this environment to mirror directly — proposal:
   support both from day one since `serenity`'s `EventHandler::message`
   callback doesn't meaningfully distinguish implementation cost between
   the two, only the allow-list policy differs.
2. **Slash commands vs. plain messages**: Discord's native slash-command
   UI (`/ask`, `/status`, etc.) is a richer affordance than Telegram's
   text-prefixed `/command` convention. Should Discord register native
   application commands mirroring existing Telegram slash commands, or
   should it accept the same plain-text `/command` convention unchanged
   for parity-of-behavior (simpler, less Discord-specific surface, but
   loses Discord's autocomplete/native command-picker UX)? Proposal:
   plain-text parity first (matches Telegram, zero new command-routing
   logic), native slash commands as an explicit fast-follow.
3. **Rate-limit/backoff policy**: `serenity` has its own internal
   rate-limit handling, but the adapter still needs a policy for what
   happens when `deliver()` hits a sustained 429 (retry-with-backoff vs.
   fail the delivery and log, matching how `telegram_spine`'s
   `run_delivery_loop` currently falls back from edit-failure to
   send-new-message). Needs an explicit decision before implementation
   rather than ad hoc handling.
4. **Multi-guild identity**: unlike Telegram (one bot, effectively one
   "chat surface" concept per `chat_id`), a Discord bot can be installed
   into many guilds simultaneously. Does `PermissionMode`/session state
   (ADR-0016) key on `chat_id` alone (channel ID) or need
   `(guild_id, channel_id)` composite keys to avoid session bleed across
   guilds using the same channel-ID numbering space? (Snowflake IDs are
   globally unique, so this is likely a non-issue, but should be
   confirmed explicitly rather than assumed.)

## Implementation scope (deferred — NOT this pass)

Per C-DEV-001 / the pares-radix-dev-lifecycle hard gate, no code changes
land with this ADR. The implementation pass should follow the staged
orchestration (analyze → fix → test → deploy → verify) and produce, in
order: (a) `serenity` dependency + `discord` feature flag on
`pares-agens-channels`, (b) `discord_spine.rs` implementing `SpineChannel`
per §2, (c) CLI wiring (`--discord-token`, new `run_serve` match arm) per
§3, (d) approval-button rendering wired to the existing `turn_ux`
parser per §5, (e) tests exercising `start_receiving`/`deliver` against a
mocked Discord gateway/HTTP layer (matching the style of
`channels/tests/e2e.rs`'s existing spine-channel test harness, zero real
Discord API calls in CI), (f) docs update for the new env vars.
