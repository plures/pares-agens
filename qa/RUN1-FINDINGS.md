# pares-agens QA Pilot — Run 1 Findings (local build, v1.59.5 @ 0496daf)

**Build:** `cargo build --release -p pares-agens-cli` succeeded clean, 5m43s, zero errors.
Binary: `target\release\pares-agens.exe` (package `pares-agens-cli` produces binary name `pares-agens`).

## BLOCKING FINDING: no channel-agnostic way to run the agent headless (violates C-TEST-001/002)

`pares-agens.exe --help` exposes exactly 3 subcommands: `migrate`, `serve`, `tui`.

- `serve` is the ONLY headless/daemon mode, and it **hard-requires `--telegram-token`**
  (`main.rs:1940`, no `Option<>`, no default) — there is no stdin/HTTP/MCP-only serve mode
  exposed at the CLI layer, even though the channels crate clearly HAS the code for it:
  `crates/channels/src/stdio_spine.rs`, `http_spine.rs`, `stdin.rs` all exist and are real
  (non-trivial file sizes, referenced from `lib.rs`).
- This means: **today, you cannot QA pares-agens's core agent behavior without a live
  Telegram bot token.** That is exactly the anti-pattern C-TEST-001/002 exists to prevent
  ("if the only way to verify a feature is through a specific channel adapter, development
  is not finished"). The capability to test channel-agnostically appears to already exist
  in the library layer — it's just not wired up as a CLI entry point.
- I am NOT working around this by testing through Telegram (that would itself violate
  C-TEST-002). I am reporting it as a real gap, per "no stubs / no silent workarounds."

## What this means for the T1–T16 task suite (qa/tasks.md)
None of T1–T13 (the `shipped`-feature tasks) can be executed against this build TODAY without
either (a) a live Telegram token, or (b) wiring `stdio_spine`/`http_spine` into the CLI's
`serve` command (or exposing a dedicated `pares-agens.exe mcp` / `--channel stdio` flag).

**This is itself the QA pilot's first real, high-value finding** — precisely what requirement 3
of the original ask wanted: insight that helps developers fix the *root cause* (missing headless
CLI wiring) rather than the QA process working around it.

## Recommended next action (not yet done — a dev-stage fix, correctly routed there)
Per `sdlc-orchestration`, this is a `develop`-stage bug, not a `qa`-stage workaround: add a
`pares-agens.exe serve --stdio` (or `--channel stdio|http`) flag that uses the existing
`stdio_spine`/`http_spine` code without requiring `--telegram-token`. Once that lands, the
QA pilot resumes and runs T1–T13 against it for real.

## Status of this pilot run
- FEATURES.md and qa/tasks.md: committed (`0496daf`), stand as-is — accurate and ready.
- README.md stale-features fix: committed (`0496daf`).
- Task suite EXECUTION: blocked on the headless-CLI gap above. Not faked, not worked around.
- Recommend filing this as a real GitHub issue on pares-agens (dev-stage fix) so it enters the
  normal issue→Copilot pipeline, tagged so it's understood as a QA-enablement blocker.
