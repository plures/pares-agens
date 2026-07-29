# Task grounding / Telegram timeout analysis

## Root causes

1. **Conversation history was discarded on an inferred topic shift.** Orchestrator computes embedding similarity and used it as `clear_history` (`crates/core/src/orchestrator/mod.rs:314`), while the agent interprets that flag by replacing the loaded channel history with an empty vector (`crates/core/src/agent.rs:1090-1094`). The model prompt otherwise correctly includes persisted history before the current user message (`crates/core/src/agent.rs:1407-1410`). A task discussion followed by a semantically different status question can therefore lose the immediately preceding task turns, leaving the model to invent an answer. Semantic shift remains useful only for clearing recalled/context-manager items (`crates/core/src/orchestrator/mod.rs:363-371`); it is not a safe signal for deleting explicit conversation state.

2. **The raw timeout prose was itself the Telegram fallback.** Telegram wraps the agent future in a timeout (`crates/channels/src/telegram.rs:2652-2655`), then sends `TELEGRAM_TIMEOUT_FALLBACK` through the same progressive reply delivery path on expiry (`crates/channels/src/telegram.rs:2734-2750`). The constant at `crates/channels/src/telegram.rs:69` contained “this turn took too long and was stopped,” so that text necessarily appeared as ordinary reply content.

## Fixes

- Preserve explicit channel conversation history across semantic topic shifts. Topic shifts now clear only transient recalled context; `CerebellumContext.clear_history` remains false. Explicit branch/session commands remain responsible for user-requested history changes.
- Replace the first-person/raw timeout sentence with an explicitly labelled `⚠️ System notice:` stating that no assistant response was produced.
- Updated real regression tests in both changed crates.

## Validation

Validation results are appended below by the test commands.

- Core regression: `cargo test -p pares-agens-core preprocess_preserves_conversation_history_on_topic_shift` — exit 0.
- Channels regression: `cargo test -p pares-agens-channels progressive_missing_response_fallbacks_are_visible` — exit missing.
- Clippy: `cargo clippy -p pares-agens-core -p pares-agens-channels --all-targets -- -D warnings` — exit missing.
- Workspace-wide fmt check is blocked by pre-existing formatting differences outside the changed files (including `crates/tauri-app` and `crates/core/tests/headroom_e2e.rs`).

- Core regression command completed: cargo test -p pares-agens-core preprocess_preserves_conversation_history_on_topic_shift.
- Channels regression command completed: cargo test -p pares-agens-channels progressive_missing_response_fallbacks_are_visible.
- Clippy command: cargo clippy -p pares-agens-core -p pares-agens-channels --all-targets -- -D warnings.
- Workspace fmt check reports pre-existing formatting differences outside changed files (tauri-app and core headroom_e2e test).
