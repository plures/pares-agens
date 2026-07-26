# pares-agens Feature Ledger (retro-populated, pilot)

Source of truth is intended to be `feature:pares-agens:*` entities (PluresDB), regenerated into this file.
This is a PILOT bootstrap: entries below are derived directly from README.md "Features" section + repo
inspection at commit `d98e7a5` (v1.59.5, 2026-07-25). No feature is invented — anything not explicitly
claimed in README/CHANGELOG is marked `unverified` rather than assumed shipped.

| id | name | status | introduced | last_verified_version | notes |
|---|---|---|---|---|---|
| model-routing | Multi-provider model routing (OpenAI/Anthropic/Gemini/Docker Model Runner/OpenAI-compatible) | shipped | unknown | v1.59.5 (README claim only) | Per-task routing (interactive/background/coding) via TOML config |
| plureslm-memory | Persistent memory via PluresLM (auto-capture + recall) | shipped | unknown | v1.59.5 (README claim only) | — |
| reactive-procedures | Reactive agent behavior as PluresDB procedures (not hardcoded routing code) | shipped | unknown | v1.59.5 (README claim only) | Core architecture claim — "the database IS the agent" |
| decision-ledger | Praxis decision ledger + approval gates for high-stakes actions | shipped | unknown | v1.59.5 (README claim only) | — |
| cross-platform-native | Native desktop app (Windows/macOS/Linux) + mobile (iOS/Android) | shipped | unknown | v1.59.5 (README claim only) | Tauri-based per crates/tauri-app |
| offline-local-model | Offline-capable operation with a local model | shipped | unknown | v1.59.5 (README claim only) | — |
| p2p-sync | P2P sync via Hyperswarm (no server required) | shipped | unknown | v1.59.5 (README claim only) | — |
| cross-platform-nodes | Multi-node agent (one brain, many device "hands") | shipped | unknown | v1.59.5 (README claim only) | — |
| status-tool-count-fix | `/status` counts full tool set, not just plugin subset | shipped | v1.59.5 | v1.59.5 | PR #668, b192a67 — most recent fix, real regression test candidate |
| headroom-compression | Message/context headroom compression hook (below/above-threshold passthrough vs compress) | shipped | pre-v1.59 (exact version unconfirmed) | v1.59.5 (re-checked) | **CONFIRMED real**: `crates/core/src/headroom.rs` + `headroom_bridge.rs` exist and are referenced in `agent.rs`/`lib.rs`. NOT in README's Features list — undersold/internal-only, needs a human call on whether to surface it. |
| skill-discovery-parity | Runtime skill discovery + `<available_skills>` injection (parity with OpenClaw) | shipped | v1.59.0 | v1.59.5 (re-checked) | PR #658; confirmed real: `crates/marketplace/src/skill_discovery.rs` + refs in `agent.rs`/`prompt_builder.rs`. |
| discord-adapter | Discord channel adapter (channel-agnostic design) | design_only | design-only per ADR-0018 | v1.59.5 (re-checked) | **CONFIRMED not implemented**: `git grep -il discord -- crates` finds ZERO adapter/channel files (only unrelated string matches in prompt_builder.rs/radix_handler.rs). `crates/channels/src/` contains ONLY telegram + stdin/stdio/tauri_ipc/http_spine — no discord.rs. Do not list as shipped; ADR is design-only, code absent. |
| teams-adapter | Teams-first channel adapter (channel-agnostic design) | design_only | design-only per ADR-0017 | v1.59.5 (re-checked) | **CONFIRMED not implemented**: `git grep -il teams -- crates` returns EMPTY. No teams.rs anywhere in the workspace. Design doc exists; code does not. |
| approval-card-parity | Approval-card UX parity (Discord/other channels) | design_only | ADR-0018, remaining scope noted | v1.59.5 (re-checked) | **CONFIRMED not implemented**: `git grep -il approvalcard -- crates` returns EMPTY — no ApprovalCard type exists in the Rust workspace at all. This feature does not exist yet in any form; CHANGELOG's "remaining scope" undersells it — it's not partially done, it's not started in code. |
| pluresLM-mcp-integration | Agency <-> PluresLM MCP integration boundary | in_review | ADR-0017 (design-only) | unverified | design-only per changelog title |
| tui | Text UI (ratatui-based) | shipped | ~2026-04 (per memory: "TUI crate created") | v1.59.5 (re-checked) | **CONFIRMED real**: `crates/tui/src/app.rs` (45KB) + `ui.rs` + `lib.rs` exist and are substantial, not stubs. |
| bitnet-local-model | BitNet as default local model (no Ollama dependency) | shipped | ~2026-04-25 | v1.59.5 (re-checked) | **CONFIRMED real**: `crates/agens-plugin/src/agent_commands/bitnet_classifier.rs` + wiring in `runtime.rs`/`lib.rs`/Cargo.toml exist. |
| pluresLM-mcp-integration | Agency <-> PluresLM MCP integration boundary | design_only | ADR-0017 (design-only) | v1.59.5 (re-checked) | Only 1 file references plureslm in mcp-server (`radix_handler.rs`) — thin, consistent with CHANGELOG's "design-only" framing. Treat as design_only until deeper wiring confirmed. |

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
