# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial changelog

## [0.5.0] — 2026-03-22

- feat(praxis): adopt @plures/praxis for declarative logic management (#165) (ea405ba)
- test: add QA test suite — 17/17 pass (100%) (ed642bf)

## [0.4.1] — 2026-03-21

- fix: revert MemorySidebar to local component — design-dojo has duplicate export bug (cfe7214)

## [0.4.0] — 2026-03-21

- feat: desktop builds v0.3.0 — fix installer workflow + updater config (#163) (#170) (0227357)
- feat(ui): replace local MemorySidebar with design-dojo components (#161) (#169) (530bd16)
- feat(ui): MCP server management tab in Settings (#162) (#168) (e2c1cc6)
- feat(mcp): wire MCP tool execution into chat loop (#160) (#167) (e662aa0)
- fix: resolve clippy warnings breaking CI (-D warnings) (a3aa307)
- feat(ui): streaming chat UI with design-dojo tokens (#166) (5dbdaa6)
- feat(tauri-app): wire ModelRouter into Tauri app state (#164) (0247bc3)
- fix: suppress clippy warnings in cerebellum pipeline (aa27156)
- feat(tauri-app): wire end-to-end message flow through cerebellum (#156) (2c5317b)
- feat(core): integrate plures-vault for secret storage (API keys, tokens) (#154) (3867fe5)
- feat(core): add PluresDB FFI bridge to replace TypeScript ProcedureEngine (#153) (0b52b67)
- feat(core): replace InMemoryStore with PluresDB-backed MemoryStore (#149) (28bfba3)
- feat(core): add AgentInvoke step for LLM callbacks from procedures (#147) (0ba745c)
- feat(core): implement built-in cognitive procedures (autorecall, primitive-extract, cerebellum-sweep) (#146) (b26755f)
- feat(core): add cerebellum orchestrator module (#134) (b0cf690)
- docs: three-agent cognitive architecture (cerebellum as orchestrator) (56e04bf)
- docs: PluresDB native procedure integration plan (bf89425)
- fix: use Svelte 5 onclick attribute over deprecated on:click directive (#126) (0434ec3)
- feat: Integrate Praxis coprocessor guidance into memory sidebar (#105) (49d4d7b)
- feat: native max-min optimization engine for fine-tuned policy execution (#102) (a7cbab3)
- ci: add PR lane event relay to centralized merge FSM (8db09b6)
- feat(marketplace): add LoRA adapter distribution — AdapterPackage, MarketplaceListing, Marketplace (#100) (5b031f3)
- ci: stabilize build-installers workflow for Tauri compilation reliability (#99) (b64ed55)
- feat: implement Arca + Faber + Agenda MVP foundations (#95) (0d39de0)
- feat(ci): emit capacity-cleared event to praxis-business when Copilot slot frees (#96) (b2811a3)
- ci: stabilize Build Installers workflow — pin Rust + tauri-cli + fix cache key (#93) (a41d75e)
- feat: marketplace crate for skill/extension discovery (#90) (483ef0a)
- fix(ci): remove `secrets` context from step `if` condition in build-installers.yml (#88) (aff9652)
- feat: add pares-agens-privacy crate for training data PII protection (#86) (f8b3048)
- docs(readme): update pricing + Pares Sociorum naming (#85) (43f1de3)
- feat: first-run wizard — setup flow for new users (#82) (fa8e705)
- Initial plan (#84) (f0bb5a4)
- feat: Settings UI — model config, channels, preferences (#80) (4dcd769)
- feat(trainer): add skill detection and auto-clustering (#78) (48025e1)
- Initial plan (#77) (dc16bd2)
- fix: unblock Pares Agens MVP — Svelte 5 UI scaffold + release CI permissions (#75) (7fbfbeb)
- feat: OpenClaw → Pares Agens migration tool (#54) (f58aa20)
- docs: Getting started guide, procedure authoring, API reference (#56) (475330b)
- fix(ci): add id-token permission to release workflow (#62) (f3d67c1)
- feat(trainer): add trainer crate for model fine-tuning (#63) (35b19a6)
- docs: landing page — comparison, getting started, FAQ, OG tags, download API (#55) (c4ab1f2)
- feat: cross-platform installers (Windows MSI, macOS DMG, Linux AppImage/deb/rpm) (#52) (6aa638e)
- feat: procedure editor UI — view/edit/create PluresDB procedures (#50) (b04817f)
- feat: Tauri app scaffold with system tray + IPC (#47) (0c9f177)
- feat: cross-platform installers (Windows MSI, macOS DMG, Linux AppImage) (#33) (beaa5b8)
- feat: Pro feature gates + license key validation (#34) (d3707ad)
- docs: landing page + documentation site (#36) (a864859)
- feat: OpenClaw migration tool (#35) (0780143)
- feat: first-run wizard, procedure editor, and Agent/Memory abstractions (#32) (5f8df59)
- feat: Tauri app scaffold with design-dojo UI (#31) (ddf9de5)
- docs: shift from Telegram channels to native multi-device vision (1dfa199)
- docs: rewrite README — support all major model providers, not just local (b5e76f8)
- Update crates/core/src/executor.rs (b55c35d)
- feat: add praxis decision ledger and approval gate procedures (cbe474d)
- Initial plan (b1543a0)
- fix: remove conflicting memory.rs (memory/ dir takes precedence) (0b8883a)
- feat: auto-recall and auto-capture procedures (5a71762)
- Initial plan (faf74d1)
- fix: add chrono/thiserror deps + memory compat types (ac6dc85)
- feat: PluresLM Rust integration (native memory ops) (4b04185)
- Initial plan (2133765)
- fix: align channels with PR #15 Event schema (c14aa40)
- fix: make tool-loop exhaustion warning explicitly conditional (535019d)
- feat: Core procedures (on_message, on_timer, on_state_change) (d8bb443)
- Initial plan (76bca2c)
- fix(mcp-client): address review feedback — notification id, pagination, EOF, Drop, uuid removal (809cf1c)
- Update crates/mcp-client/src/protocol.rs (748e50a)
- Update crates/mcp-client/src/transport/mod.rs (bd1673c)
- Update Cargo.toml (8ed2531)
- Update crates/mcp-client/src/transport/http.rs (b04f9d5)
- Update crates/mcp-client/src/transport/stdio.rs (9271183)
- Update crates/mcp-client/src/transport/http.rs (1a1514b)
- feat(mcp-client): add MCP client crate with stdio/HTTP transports, tool discovery, and OpenAI conversion (1df1d60)
- Initial plan (262b879)
- Update crates/models/tests/integration_test.rs (6a7ce20)
- Update crates/models/src/streaming.rs (7378f29)
- feat(models): OpenAI-compatible model router with SSE streaming (e0a3f63)
- Initial plan (cc3120c)
- feat(core): reactive procedure executor with event loop, registry, and handler stubs (3a9324c)
- Initial plan (def7503)

## [0.2.0] — 2026-02-26

- feat: Telegram channel adapter (teloxide) (#23) (cb7b793)

## [0.1.0] — 2026-02-23

- chore: add copilot instructions and org standards (cf7aac0)
- docs: add Pares Nubis managed cloud replica design (#3) (e00981a)
- feat: remote device security management (Find My Device++) (#4) (fe0fad1)
- docs: formalize design docs and architecture (412f177)
- chore: initial scaffold (7130b86)
- Initial commit (134b579)

