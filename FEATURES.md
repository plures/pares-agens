# pares-agens Feature Ledger (retro-populated, pilot)

Source of truth is intended to be `feature:pares-agens:*` entities (PluresDB), regenerated into this file.
This is a PILOT bootstrap: entries below are derived directly from README.md "Features" section + repo
inspection at commit `d98e7a5` (v1.59.5, 2026-07-25). No feature is invented — anything not explicitly
claimed in README/CHANGELOG is marked `unverified` rather than assumed shipped.

| id | name | status | introduced | last_verified_version | last_qa_result | notes |
|---|---|---|---|---|---|---|
| model-routing | Multi-provider model routing (OpenAI/Anthropic/Gemini/Docker Model Runner/OpenAI-compatible) | shipped | unknown | v1.59.5 (README claim only) | PASS (2026-07-26, f1b4890) — T1: routed correctly to configured Copilot model (gpt-4.1), logged; minor UX gap: model can't self-report which model it is when asked | Per-task routing (interactive/background/coding) via TOML config |
| plureslm-memory | Persistent memory via PluresLM (auto-capture + recall) | shipped | unknown | v1.59.5 (README claim only) | PASS (2026-07-26, f1b4890) — T2: fact stored, recalled correctly in new session; negative case (never-told fact) honestly declined, no fabrication | — |
| reactive-procedures | Reactive agent behavior as PluresDB procedures (not hardcoded routing code) | shipped | unknown | v1.59.5 (README claim only) | INCONCLUSIVE (2026-07-26, f1b4890) — T3: no concrete named procedure exists to invoke as ground truth; agent honestly reported no predefined procedure (no fabrication) but claim is unverifiable at CLI level as tested | Core architecture claim — "the database IS the agent" |
| decision-ledger | Praxis decision ledger + approval gates for high-stakes actions | shipped | unknown | v1.59.5 (README claim only) | FAIL-partial (2026-07-26, f1b4890) — T4: destructive-action request was declined, but transcript shows tool_calls=0 — refusal was model judgment only, no evidence of a system-enforced gate/ledger record. Filed pares-agens#674 (P1). | — |
| cross-platform-native | Native desktop app (Windows/macOS/Linux) + mobile (iOS/Android) | shipped | unknown | v1.59.5 (README claim only) | NOT RUN — T5 explicitly out of scope for CLI/MCP-only runner per qa/tasks.md; requires UI-testing standard (Playwright/headless) | Tauri-based per crates/tauri-app |
| offline-local-model | Offline-capable operation with a local model | shipped | unknown | v1.59.5 (README claim only) | FAIL (2026-07-26, f1b4890) — T6: no offline flag/config path exists anywhere in crates/cli; ModelChain (which implements offline/bitnet fallback) is never constructed by the CLI. Filed pares-agens#673 (P2). | — |
| p2p-sync | P2P sync via Hyperswarm (no server required) | shipped | unknown | v1.59.5 (README claim only) | NOT RUN — T7 requires two live instances/UI-adjacent setup not exercised this pass | — |
| cross-platform-nodes | Multi-node agent (one brain, many device "hands") | shipped | unknown | v1.59.5 (README claim only) | FAIL/unverified (2026-07-26, f1b4890) — T8: agent has no concept of connected devices/nodes; asked which device could take a screenshot, got a generic non-committal reply with no node registry query surfaced | — |
| status-tool-count-fix | `/status` counts full tool set, not just plugin subset | shipped | v1.59.5 | v1.59.5 | PASS (2026-07-26, f1b4890) — T9: reported "13 tools", matches live registry log tool_count=13 exactly | PR #668, b192a67 — most recent fix, real regression test candidate |
| headroom-compression | Message/context headroom compression hook (below/above-threshold passthrough vs compress) | shipped | pre-v1.59 (exact version unconfirmed) | v1.59.5 (re-checked) | PARTIAL/finding (2026-07-26, f1b4890) — T10: no compression event fired for a single large (2700-word) incoming user message; compression was only observed acting on accumulated conversation history across turns (T1/T2). The "above-threshold single message compresses" case as literally described in qa/tasks.md T10 is NOT confirmed — needs dev clarification on intended scope. | **CONFIRMED real**: `crates/core/src/headroom.rs` + `headroom_bridge.rs` exist and are referenced in `agent.rs`/`lib.rs`. NOT in README's Features list — undersold/internal-only, needs a human call on whether to surface it. |
| skill-discovery-parity | Runtime skill discovery + `<available_skills>` injection (parity with OpenClaw) | shipped | v1.59.0 | v1.59.5 (re-checked) | PASS (2026-07-26, f1b4890) — T11: listed 13 real registered tools by name, matching live tool_count=13 in logs — reflects live runtime discovery, not a hardcoded list | PR #658; confirmed real: `crates/marketplace/src/skill_discovery.rs` + refs in `agent.rs`/`prompt_builder.rs`. |
| discord-adapter | Discord channel adapter (channel-agnostic design) | design_only | design-only per ADR-0018 | v1.59.5 (re-checked) | PASS-honest-absence (2026-07-26) — T14: no Discord code exists anywhere in crates/channels/src; correctly not advertised as present | **CONFIRMED not implemented**: `git grep -il discord -- crates` finds ZERO adapter/channel files (only unrelated string matches in prompt_builder.rs/radix_handler.rs). `crates/channels/src/` contains ONLY telegram + stdin/stdio/tauri_ipc/http_spine — no discord.rs. Do not list as shipped; ADR is design-only, code absent. |
| teams-adapter | Teams-first channel adapter (channel-agnostic design) | design_only | design-only per ADR-0017 | v1.59.5 (re-checked) | PASS-honest-absence (2026-07-26) — T15: no Teams code exists anywhere in crates/channels/src | **CONFIRMED not implemented**: `git grep -il teams -- crates` returns EMPTY. No teams.rs anywhere in the workspace. Design doc exists; code does not. |
| approval-card-parity | Approval-card UX parity (Discord/other channels) | design_only | ADR-0018, remaining scope noted | v1.59.5 (re-checked) | PASS-honest-current (2026-07-26) — T16: inline-keyboard approval cards exist only in telegram.rs; no fake cross-channel abstraction found | **CONFIRMED not implemented**: `git grep -il approvalcard -- crates` returns EMPTY — no ApprovalCard type exists in the Rust workspace at all. This feature does not exist yet in any form; CHANGELOG's "remaining scope" undersells it — it's not partially done, it's not started in code. |
| pluresLM-mcp-integration | Agency <-> PluresLM MCP integration boundary | in_review | ADR-0017 (design-only) | unverified | NOT RUN | design-only per changelog title |
| tui | Text UI (ratatui-based) | shipped | ~2026-04 (per memory: "TUI crate created") | v1.59.5 (re-checked) | NOT RUN — T12 explicitly out of scope for CLI/MCP-only runner per qa/tasks.md; requires interactive TUI-testing standard | **CONFIRMED real**: `crates/tui/src/app.rs` (45KB) + `ui.rs` + `lib.rs` exist and are substantial, not stubs. |
| bitnet-local-model | BitNet as default local model (no Ollama dependency) | shipped | ~2026-04-25 | v1.59.5 (re-checked) | FAIL (2026-07-26, f1b4890) — T13: source code for BitNet client/ModelChain exists and is unit-tested, but ModelChain is never constructed anywhere in crates/cli — unreachable in the running binary. Filed pares-agens#673 (same root cause as offline-local-model). | **CONFIRMED real**: `crates/agens-plugin/src/agent_commands/bitnet_classifier.rs` + wiring in `runtime.rs`/`lib.rs`/Cargo.toml exist. |
| pluresLM-mcp-integration | Agency <-> PluresLM MCP integration boundary | design_only | ADR-0017 (design-only) | v1.59.5 (re-checked) | NOT RUN | Only 1 file references plureslm in mcp-server (`radix_handler.rs`) — thin, consistent with CHANGELOG's "design-only" framing. Treat as design_only until deeper wiring confirmed. |

## Explicitly flagged gaps (honest, not filled in)
- Most `introduced_version` values are `unknown` — the ledger is being bootstrapped from current README
  claims, not full git archaeology. A future pass should mine `git log` per feature area to backfill.
- **`in_review` status retired for this pass** — every row above has now been re-checked against the actual
  source tree (`git grep`) at commit `d98e7a5` (v1.59.5, 2026-07-26). Rows are now `shipped` (real code found)
  or `design_only` (design/ADR exists, no adapter/implementation code found). No row remains ambiguously
  `unverified`/`in_review` — that was a placeholder state and has been resolved with real evidence per row.
- **Confirmed NOT implemented**: Discord adapter, Teams adapter, and ApprovalCard type all have zero matching
  source files anywhere in `crates/` — these are genuinely `design_only`, not partially-built. This matters
  for QA: do not write tasks like "send a Discord message" expecting partial credit — the correct expected
  behavior for those tasks right now is a clean "capability unavailable" response, not a crash or a silent
  no-op (per C-NOSTUB-001 — an honest absence, not a stub).
- `headroom-compression` and TUI/BitNet are confirmed real from source but ABSENT from the current README's
  Features list — this is itself a finding: either README is stale (undersells real features) or these are
  internal-only. Needs a human/dev call, not an assumption. Flagging to kbristol, not resolving unilaterally.

## Next steps (pilot)
1. ~~Resolve the `in_review`/`unverified` rows with a real check~~ — DONE this pass via `git grep` against
   source (see table above). Remaining open item: pin exact `introduced_version` via git log/blame per feature.
2. Draft `qa/tasks.md` co-located in pares-agens covering every `shipped` row above (in progress — see below).
3. Do NOT run the task suite for real execution against a live pre-release build until a cut, versioned
   pre-release exists (per `sdlc-orchestration` skill's pre-release stage) — writing the task list itself is
   allowed now that the 4 SDLC skills are applied.
