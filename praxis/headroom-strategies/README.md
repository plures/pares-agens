# Headroom Strategy Specs (`.px`)

These `.px` files are the **design specification** for headroom's per-content-type
compression strategies. They document the *intended logic* (constraints + procedures) for
each content type the compressor handles.

## Relationship to the runtime

The **runtime** compression path is the Rust `HeadroomHook`
(`crates/core/src/headroom_bridge.rs`), which drives the genuine leaf actors in
`crates/core/src/headroom.rs` (content-type detect, sentence split, AST signature
extraction, tiktoken counting). The active policy/config artifact is
`praxis/headroom.px`.

These strategy files are **spec/reference**, not an execution path — `pluresdb-px` has no
reactive trigger engine, so `trigger:` here is descriptive. They capture the design intent
(e.g. `crusher.px` = "SmartCrusher" JSON structural compression: array key-dedup, nested
flattening, schema headers) so the Rust implementation has a source of truth to track.

## Why they live here (private agens, not open radix)

Per the ownership principle: **everything agent/AI is `pares-agens` (private, protects
IP); `pares-radix` is open**. Headroom compression is agent IP. These specs were
previously sitting *untracked* in the open `pares-radix` working tree (never committed) —
they were relocated here on 2026-06-17 so the agent-compression design lives only in the
private repo. See `memory/HEADROOM-WHERE-IT-LIVES.md` (workspace) for the full
disambiguation.

## Files

| File | Strategy |
|---|---|
| `crusher.px` | JSON structural compression (SmartCrusher: array dedup, flatten, schema headers) |
| `prose.px` | Natural-language prose compression (sentence selection) |
| `code.px` | Source-code compression (AST signature extraction) |
| `log.px` | Log-line compression (dedup, pattern folding) |
| `memory.px` | Memory/history compression |
| `pipeline.px` | Multi-stage compression pipeline orchestration |
| `router.px` | Content-type routing to the right strategy |
| `scorer.px` | Block severity / value scoring |
| `crusher.px` / `fitter.px` | Structural fit + budget fitting |
| `cache.px` | Compression result caching |
| `ccr.px` | Compression-candidate ranking |
| `config.px` | Shared config/thresholds |
| `types.px` | Shared type definitions |
