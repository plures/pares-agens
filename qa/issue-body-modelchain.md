**Problem:** FEATURES.md lists `offline-local-model` and `bitnet-local-model` as `shipped`. QA pilot (qa/tasks.md T6/T13) found `ModelChain` in `crates/core/src/model_chain.rs` (with passing unit test `offline_mode_uses_bitnet`) and `LOCAL_BITNET_PROVIDER` in `crates/models/src/config.rs`, plus real bitnet client crates (`crates/bitnet`, `crates/bitnet-sys`) — but `ModelChain::new` is never called anywhere outside `model_chain.rs`/its own tests. No CLI flag/env/config wires it in; `pares-agens serve --help` has no offline/bitnet flag.

**Impact:** Offline operation and BitNet fallback are dead code from the running binary's perspective — the feature is unusable today despite `shipped` status and passing unit tests.

**Proposed fix:** Wire `ModelChain` into `Commands::Serve` behind an explicit flag (e.g. `--offline`), constructing the bitnet `ModelClient` when enabled — same pattern as the `--stdio` fix in #672.

**Evidence:** pre-release build `f1b4890`; qa/tasks.md T6 + T13.

**Priority:** P2 — not release-blocking, but FEATURES.md's `shipped` claim needs correcting (wire it for real, or downgrade status).
