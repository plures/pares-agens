# ADR-0017: Channel-Agnostic Teams-First Adapter (Slack Follow-On)

**Status:** PROPOSED (design-only, no implementation in this PR)
**Date:** 2026-07-23
**Epic:** `pares-agens:channels-teams-slack` (P1)
**Enforcement:** Governed by `plures-dev-guide` procedures — `resolve-change-context`,
`development-lifecycle` (px-first, no-stub-completion-honesty), `merge-gate`.
No stub code, no mocked channel behavior, no fake "done" claims. This ADR contains
design only; a follow-up implementation epic will produce the actual crate.

## Context

`pares-agens` currently ships one production channel adapter: Telegram
(`crates/channels/src/telegram.rs`, `pares_radix_core::renderers::telegram`,
`pares_radix_core::channel_contract`). The adapter is bot-token-based
(long-polling via `teloxide`), assumes a single 1:1/group chat model, and
bakes Telegram-specific concerns (MarkdownV2 escaping, inline keyboards,
`ChatId`/`MessageId` types) directly into the adapter and a dedicated
renderer module.

The reactive architecture (`design/PARES-AGENS.md`) requires that channels
be "thin bridges" — all intelligence lives in PluresDB procedures via the
`EventSpine`. The existing `ChannelAdapter` trait
(`crates/channels/src/adapter.rs`) and `ChannelContract`
(`crates/core/src/channel_contract.rs`) already express the intended
separation:

- `ChannelAdapter` — async run-loop trait, `Event`-in/`Event`-out.
- `ChannelContract` — declarative per-channel rendering/rate-limit/feature
  capabilities consumed by `renderers::*` and the event spine's delivery path.
- `GroupChatPolicy` — declarative multi-user participation rules.

However, the abstraction is **not yet channel-agnostic in practice**:
1. Telegram-specific approval-button wiring (`approval_keyboard`,
   `is_approval_prompt`, inline keyboard construction) is inlined in the
   adapter rather than expressed as a contract-driven capability.
   `Praxis` approval-gate delivery (`crates/core/src/praxis/ledger.rs`,
   `GateStatus`) has no channel-neutral rendering contract today.
2. `Event::Message` carries only `{id, channel, sender, content}` — no room
   for Teams/Slack constructs (thread/conversation IDs, tenant ID, team/channel
   ID, adaptive-card payloads) without another ad hoc widening.
3. Auth model is implicit: Telegram uses a single long-lived bot token
   (`TelegramConfig.token`) fetched from `SecretStore`
   (`crates/core/src/secrets.rs`). Teams requires an Entra ID (Azure AD) app
   registration with OAuth2 client-credentials or bot-framework auth, which is
   a materially different secret/lifecycle shape (tenant-scoped, rotating
   tokens, webhook signature validation) that the current trait does not
   anticipate.
4. There is no test seam for verifying channel behavior without a live
   external service — `nsc_test_double_only_at_test_seam` requires any test
   double to live at an explicit seam, and today the Telegram adapter has no
   such seam (tests exercise pure functions like `chunk_message`,
   `escape_markdown_v2`, not the adapter's `run()` loop).

This ADR defines the design for a **Teams-first, Slack-follow-on**
implementation that generalizes the channel abstraction rather than cloning
`telegram.rs` a second time.

## Decision

Introduce a channel-agnostic core (auth, event mapping, approval rendering,
delivery contract) shared by all chat-platform adapters, and implement Teams
as the first adapter built against it. Slack follows using the same core with
a second adapter crate/module. No stub/mock adapter ships as "supported" —
Teams is only marked complete when it passes live integration tests against a
real (dev-tenant) Bot Framework/Graph endpoint, per `nsc_done_requires_binary_proof`.

### 1. Channel-agnostic core additions (crates/core)

**a. Generalize `Event::Message` and add channel identity fields**

Add an optional, structured `ChannelIdentity` payload rather than overloading
`channel: String` / `sender: String`:

```rust
pub struct ChannelIdentity {
    pub channel: String,           // "telegram" | "teams" | "slack"
    pub tenant_id: Option<String>, // Teams: AAD tenant; Slack: workspace/team ID
    pub conversation_id: String,   // chat/channel/DM thread identifier
    pub thread_id: Option<String>, // Teams reply-chain / Slack thread_ts
    pub user_id: String,           // stable platform user ID (not display name)
    pub display_name: Option<String>,
}
```

`Event::Message` keeps existing fields for backward compatibility (Telegram
call sites unchanged) and gains an additional `identity: Option<ChannelIdentity>`
field populated by newer adapters. This avoids a breaking change to the
Telegram adapter while giving Teams/Slack room to carry tenant/thread context
through the event spine and back out through delivery.

**b. `ChannelAuth` trait — pluggable, tenant-aware auth**

```rust
#[async_trait]
pub trait ChannelAuth: Send + Sync {
    /// Human-readable auth mode, e.g. "bot-token", "aad-client-credentials".
    fn mode(&self) -> &str;
    /// Acquire (or refresh) a bearer/access token for outbound API calls.
    async fn access_token(&self) -> Result<String, ChannelAuthError>;
    /// Validate an inbound request's signature/JWT (webhook auth).
    async fn verify_inbound(&self, raw_body: &[u8], headers: &HeaderMap) -> Result<(), ChannelAuthError>;
}
```

Telegram keeps its existing single-token model as a trivial `ChannelAuth`
implementation (no-op `verify_inbound`, static token). Teams implements this
with Microsoft Entra ID app-only auth (client-credentials grant against
`https://login.microsoftonline.com/{tenant}/oauth2/v2.0/token`) plus Bot
Framework JWT validation on inbound activities. All tokens/secrets go through
`SecretStore` (`crates/core/src/secrets.rs`) exactly as Telegram's bot token
does today — **never** environment variables in production, consistent with
ADR-0014's "MUST NOT USE ... JSON config files / env vars" rule.

**c. Extend `ChannelContract` for approval/interactive-action rendering**

Add fields needed by Praxis approval gates and richer platform features,
generalizing the ad hoc Telegram inline-keyboard logic:

```rust
pub struct ChannelContract {
    // ...existing fields...
    pub supports_interactive_actions: bool, // buttons/adaptive cards/block actions
    pub interactive_action_kind: InteractiveActionKind, // InlineKeyboard | AdaptiveCard | BlockKit | None
}
```

A new `renderers::approval` module renders a channel-neutral
`ApprovalPrompt { question: String, actions: Vec<(String /*label*/, String /*action_id*/)> }`
into the platform-specific representation:
- Telegram → `InlineKeyboardMarkup` (existing `build_inline_keyboard`, moved
  here unchanged).
- Teams → Adaptive Card with `Action.Submit` buttons.
- Slack (follow-on) → Block Kit `actions` block with `button` elements.

This removes `is_approval_prompt`/`approval_keyboard` from being
Telegram-adapter-private and makes Praxis gate delivery
(`GateStatus::Pending` → user prompt → `Approved`/`Rejected`) channel-agnostic,
addressing the current gap where approval UX only exists for Telegram.

**d. `ChannelAdapter` trait — no change to signature, tightened contract**

The existing trait in `crates/channels/src/adapter.rs` is already
channel-agnostic (`Event`-in/`Event`-out, `name()`, `run()`). Teams and Slack
adapters implement it as-is. `ChannelError` gains no new Telegram-specific
variant; a generic `ChannelError::Auth(String)` variant is added (shared by
all adapters that need token/webhook-signature failures) rather than adding
`ChannelError::Teams(...)` / `::Slack(...)` — keeping the error type from
re-accumulating per-channel variants the way it does for `Telegram(String)`
today. (Existing `Telegram(String)` variant stays for compatibility.)

### 2. Teams adapter design (`crates/channels/src/teams.rs`, new)

**Transport & events model**

Microsoft Teams bots use the Bot Framework REST API over HTTPS webhooks
(not long-polling like Telegram). Two supported inbound paths, ADR chooses
the first as primary:

- **Primary: Bot Framework webhook.** Requires a publicly reachable HTTPS
  endpoint (or Azure Relay/ngrok in dev) that Azure Bot Service posts
  `Activity` JSON payloads to. This means `TeamsAdapter::run()` does not
  itself poll; it registers an HTTP handler that the pares-agens runtime's
  existing HTTP surface (if any) or a small embedded `axum`/`hyper` listener
  exposes. This is architecturally different from Telegram's self-contained
  polling loop and must be reflected in the adapter's `run()` implementation
  (owns a bound listener) rather than assumed away.
- Rejected alternative: Microsoft Graph `chatMessage` change notifications
  (subscription + webhook) — higher complexity, requires Graph API
  permissions beyond bot messaging; deferred, not part of this design.

**Auth**

- Azure AD (Entra ID) **multi-tenant bot app registration** created in Azure
  Bot Service, with:
  - App (client) ID + client secret (or cert) — stored via `SecretStore`.
  - Bot Framework token endpoint used to fetch outbound access tokens
    (`https://login.microsoftonline.com/botframework.com/oauth2/v2.0/token`).
  - Inbound JWT validation against Bot Framework's OpenID metadata
    (`https://login.botframework.com/v1/.well-known/openidconfiguration`) to
    verify `Authorization: Bearer` headers on incoming activities — this is
    the `ChannelAuth::verify_inbound` implementation.
- Teams app manifest (`manifest.json`) registered via Teams Developer Portal
  or `TeamsFx`, referencing the same AAD app ID, declaring bot scopes
  (personal, team, groupChat).

**Event mapping**

`Activity` (type=`message`) → `Event::Message` with
`identity.tenant_id = activity.channelData.tenant.id`,
`identity.conversation_id = activity.conversation.id`,
`identity.user_id = activity.from.aadObjectId` (stable) with
`activity.from.name` as `display_name`. Adaptive Card `Action.Submit` payloads
(`activity.value`) map to a new lightweight `Event::InteractiveAction { action_id, payload }`
variant (channel-agnostic — Slack block actions map to the same variant),
routed by the event spine to Praxis gate resolution instead of `on_message`.

**Rendering / delivery**

Outbound replies use Bot Framework `conversations/{conversationId}/activities`
POST with Adaptive Card or plain-text `Activity`. Teams contract:
`max_message_len` ~ 28KB (Adaptive Card body limit is the practical
constraint, not raw text), `preferred_format = "AdaptiveCard"`,
`fallback_format = "plain"`, `supports_message_edit = true` (PATCH existing
activity), `typing_indicator` via `type: "typing"` activity.

**Group/tenant policy**

`GroupChatPolicy` reused as-is for team-channel participation
(`respond_on_mention` maps to `<at>Bot</at>` entity detection in Teams
activities). Additionally, Teams requires tenant allow-listing: only
messages from configured tenant IDs are processed (multi-tenant bot
registrations otherwise accept traffic from any tenant that installs the
app) — this is a new, Teams-specific policy field
(`TeamsConfig.allowed_tenant_ids: Vec<String>`), not part of the generic
contract, since Telegram/Slack have no tenant concept.

### 3. Testable core API (no stubs — real seam only)

Per `no-stub-completion-honesty.px` (`nsc_test_double_only_at_test_seam`,
`nsc_fixture_only_is_not_proof`), the test strategy is:

1. **Pure-function unit tests** (no network): `ChannelContract` construction,
   `Activity` ↔ `Event` mapping functions, Adaptive Card
   builder-from-`ApprovalPrompt`, JWT-claims parsing logic — all as free
   functions taking/returning plain data, testable without any HTTP.
2. **Explicit test seam at the HTTP boundary**: `TeamsAdapter` is
   constructed with an injectable `BotFrameworkClient` trait
   (`send_activity`, `fetch_token`) — production implementation uses `reqwest`
   against real Bot Framework endpoints; the *only* substitute permitted at
   this seam is a `wiremock`/`httpmock` HTTP server bound to `localhost` in
   `#[cfg(test)]`, which is a real HTTP server receiving real requests, not an
   in-process mock of the trait — satisfying "test double only at test seam"
   while keeping "fixture-only is not proof" honest (the double still speaks
   real HTTP/JSON wire format Bot Framework requires).
3. **Live integration test, feature-gated**: `#[ignore]`-by-default test
   requiring `PARES_TEAMS_TEST_APP_ID`/`SECRET`/`TENANT_ID` env vars, run
   manually or in a dedicated CI job with real dev-tenant credentials. This is
   the actual "done" proof (`nsc_done_requires_binary_proof`) — CI green on
   unit tests alone does **not** constitute "Teams support works."
4. No adapter is marked supported in `docs/decisions` or README until step 3
   passes against a live Azure Bot Service registration in a dev tenant.

### 4. Slack follow-on (design sketch only, separate epic)

Once Teams ships, Slack reuses:
- `ChannelIdentity` (workspace `team_id` as `tenant_id`, `channel_id` as
  `conversation_id`, `thread_ts` as `thread_id`).
- `ChannelAuth` — Slack OAuth v2 bot token (`xoxb-...`) + request signing
  secret (`X-Slack-Signature` HMAC verification) as `verify_inbound`.
- `renderers::approval` — new `BlockKit` variant.
- `Event::InteractiveAction` — Slack `block_actions` payloads.
- Transport: Slack **Events API** webhook (same "owns an HTTP listener"
  shape as Teams) or Socket Mode (outbound WebSocket, no public endpoint
  needed — likely preferred for parity with Telegram's "no public IP
  required" operational model). This choice is deferred to the Slack epic's
  own design note but Socket Mode is flagged as the probable pick to avoid
  requiring inbound webhook infrastructure, unlike Teams which effectively
  requires one (Bot Framework has no long-poll/socket alternative).

## External Prerequisites (honest accounting — not yet in place)

The following are **required before Teams implementation can start** and are
**not** things this ADR or pares-agens can provision unilaterally:

1. **Azure AD (Entra ID) app registration** in a tenant with rights to
   create Azure Bot Service resources — needs an Azure subscription/tenant
   admin or delegated Application Administrator role. See
   `~/.agents/skills/entra-app-registration/SKILL.md` for the registration
   procedure once a tenant is designated.
2. **Azure Bot Service resource** (Bot Channels Registration or multi-tenant
   Bot resource) bound to the AAD app, with the Teams channel enabled.
3. **A publicly reachable HTTPS endpoint** for the Bot Framework webhook
   (production: real domain + TLS; dev: ngrok/dev tunnel or Azure Dev
   Tunnels). pares-agens currently has no long-running public HTTP surface —
   this is new operational infrastructure, not just new Rust code.
4. **Teams app manifest + sideloading/publishing** — a Teams app package
   (manifest.json + icons) must be uploaded to a tenant's Teams admin center
   (org install) or personally sideloaded for dev testing. This requires
   Teams admin consent in the target org for anything beyond personal
   sideloading.
5. **Dev-tenant test credentials** for the live integration test in step 3
   above (`PARES_TEAMS_TEST_APP_ID/SECRET/TENANT_ID`) — must be provisioned
   in a real (ideally disposable/dev) Microsoft 365 tenant and stored via
   `SecretStore`/CI secrets, never committed.
6. **Slack workspace + app registration** (follow-on) — a Slack app must be
   created at api.slack.com, installed to at least one workspace, with bot
   token scopes (`chat:write`, `channels:history`, etc.) and, if webhook mode
   is chosen, a public HTTPS endpoint; if Socket Mode is chosen, an
   app-level token with `connections:write` scope instead.

None of these prerequisites exist in the current pares-agens repo or CI. This
ADR does not assume they exist; the implementation epic's first task must be
provisioning #1–#2 (and #6 for Slack) before any adapter code lands, and the
epic must not claim "Teams support" complete without live proof against them
(#5).

## Consequences

**Positive**
- `ChannelIdentity`, `ChannelAuth`, and `renderers::approval` are reusable by
  any future channel (Discord, Signal, WhatsApp — already named in
  ADR-0014's architecture diagram) without another full clone of
  `telegram.rs`.
- Praxis approval gates become deliverable on any channel, not
  Telegram-only.
- Auth model now explicitly supports tenant-scoped OAuth, closing a gap the
  single-bot-token model couldn't express.

**Negative / risk**
- `Event::Message` gains an optional field, increasing match-arm surface
  for every consumer (event spine, cerebellum, tests) — must be additive
  only (`Option<ChannelIdentity>`) to avoid breaking Telegram call sites.
- Teams' webhook-based transport requires new operational infrastructure
  (public HTTPS endpoint) pares-agens has not needed before; this is a real
  architectural addition, not just an adapter.
- External prerequisites (tenant, app registration, Bot Service, public
  endpoint) block any code-level progress and are outside engineering's
  unilateral control — must be tracked as blocking tasks, not assumed.

## Non-Goals (this ADR)

- No adapter implementation code, no stub/mock "TeamsAdapter" shipped as if
  functional.
- No Slack implementation — design sketch only, deferred to its own epic
  once Teams lands and the shared core proves out.
- No decision on hosting model for the public HTTPS endpoint (Azure App
  Service vs. tunnel vs. existing infra) — that is an ops/deployment epic.

## Open Questions (to resolve before implementation epic starts coding)

1. Where does the new HTTP listener (Bot Framework webhook receiver) run —
   inside the existing pares-agens process, or a separate lightweight
   ingress service? Affects `TeamsAdapter::run()`'s ownership model.
2. Multi-tenant vs. single-tenant AAD app registration — multi-tenant needs
   admin consent flow per installing org; single-tenant is simpler but
   limits distribution. Recommend starting single-tenant for the dev/test
   tenant, revisit for GA.
3. Does `EventSpineHandle` need a generic `emit_interactive_action` method
   parallel to `emit_inbound_message`, or does `Event::InteractiveAction`
   route through the existing `emit_inbound_message` path with a payload
   discriminator? (Leaning toward a dedicated emitter for parity with how
   `emit_tool_execution` already gets its own method.)

## References

- `design/PARES-AGENS.md` — reactive architecture, channel adapters as thin
  bridges.
- `docs/decisions/ADR-0014-full-plures-stack.md` — no external HTTP for core
  capabilities (channels are explicitly the exception — they are the
  external boundary by definition).
- `crates/channels/src/adapter.rs`, `telegram.rs`, `group_context.rs`.
- `crates/core/src/channel_contract.rs`, `event_spine.rs`, `event.rs`,
  `praxis/ledger.rs`, `secrets.rs`.
- `plures-dev-guide` procedures: `no-stub-completion-honesty.px`,
  `development-lifecycle.px`, `merge-gate.px`.
- `~/.agents/skills/entra-app-registration/SKILL.md` (AAD app registration
  procedure, to be used by the implementation epic).

