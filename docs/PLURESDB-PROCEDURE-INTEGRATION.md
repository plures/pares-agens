# PluresDB Native Procedure Integration

**Status**: Planned  
**Date**: 2026-03-17  
**Design Doc**: [development-guide/design/PLURESDB-NATIVE-PROCEDURES.md](https://github.com/plures/development-guide/blob/main/design/PLURESDB-NATIVE-PROCEDURES.md)

## Context

pluresLM-mcp currently has a TypeScript procedure engine that duplicates PluresDB's Rust `pluresdb-procedures` crate. This doc tracks how pares-agens consumes PluresDB directly as Tier 1 (in-process, zero serialization).

## Pares Agens Integration Points

### 1. Memory Recall (Hot Path)
**Current**: Not yet implemented (planned via pluresLM-mcp HTTP)  
**Target**: Direct `pluresdb-procedures` crate dependency

```toml
# crates/core/Cargo.toml
[dependencies]
pluresdb-core = { git = "https://github.com/plures/pluresdb" }
pluresdb-procedures = { git = "https://github.com/plures/pluresdb" }
```

Recall becomes an in-process procedure call:
```rust
let engine = ProcedureEngine::new(&store, &actor);
let results = engine.exec_dsl("filter(category in [\"conversation\",\"decision\"]) | sort(score desc) | limit(10) | transform(format: \"toon\")")?;
```

### 2. Subconscious Processing (AgensRuntime)
The existing `praxis/guidance.rs` `GuidanceService` becomes a consumer of `AgensRuntime` procedure output:

```rust
// On startup, register cerebellum procedures
runtime.register_timer("cerebellum_sweep", Duration::from_secs(300), Arc::new(move |_| {
    let primitives = engine.exec(&[
        Step::Filter { predicate: Predicate::eq("category", "primitive") },
        Step::Filter { predicate: Predicate::gt("created_at", five_minutes_ago) },
    ])?;
    
    // Only invoke LLM when conflicts detected or alignment scoring needed
    if needs_reasoning(&primitives) {
        let guidance = model.analyze(primitives)?;
        guidance_service.update(guidance);
    }
    Ok(())
}));
```

### 3. Primitive Extraction (after_store)
Registered as an `AgensRuntime` handler:
```rust
runtime.register_procedure("after_store", Arc::new(|event| {
    // Extract: fact, rule, constraint, objective, hypothesis, risk, decision, evidence, action-item
    // Store back as nodes with category "primitive" and typed edges
}));
```

### 4. Decision Ledger Integration
`praxis/ledger.rs` already exists. Wire it to procedure events:
- Every procedure execution logs to the ledger
- High-stakes actions trigger `ValidationStatus::GateRequired`
- Gates surface through active channel

## What PluresDB Needs First

Before pares-agens can consume directly, `pluresdb-procedures` needs:
- [ ] `VectorSearch` step (semantic search using stored embeddings)
- [ ] `TextSearch` step (keyword/fulltext)
- [ ] `Transform` step with format modes (`structured`, `fused`, `toon`)
- [ ] `Conditional` step
- [ ] `Parallel` step
- [ ] `Assign` / `Emit` steps

Tracked in: plures/pluresdb issues (to be created)

## Migration from OpenClaw

Pares Agens replaces OpenClaw as the primary runtime. pluresLM continues as:
1. **pluresLM-mcp** — MCP server for external consumers (OpenClaw plugin, Claude Desktop, etc.)
2. **pluresLM** (OpenClaw plugin) — thin bridge, maintained for existing customers

Pares Agens does NOT use MCP internally. It embeds PluresDB directly.
