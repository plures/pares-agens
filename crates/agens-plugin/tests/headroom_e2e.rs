//! End-to-end integration tests for headroom-px.
//!
//! Exercises the full pipeline: parse .px procedures → compile → execute with
//! the real HeadroomActionHandler (SHA-256, tiktoken, sentence splitting, etc.)
//! backed by a live CrdtStore.

use agens_plugin::headroom::HeadroomActionHandler;
use pluresdb::CrdtStore;
use pluresdb_px::px::compiler::compile;
use pluresdb_px::px::executor::{self, ActionHandler};
use pluresdb_px::px::parse;
use serde_json::{json, Value};
use std::sync::Arc;

fn make_handler() -> (HeadroomActionHandler, Arc<CrdtStore>) {
    let store = Arc::new(CrdtStore::default());
    let handler = HeadroomActionHandler::new(store.clone());
    (handler, store)
}

fn load_px(name: &str) -> String {
    // Resolve the headroom-strategies `.px` specs. These are agens-side IP
    // (preserved at `praxis/headroom-strategies/`). Resolve relative to this
    // crate via CARGO_MANIFEST_DIR so the test is hermetic and runs anywhere
    // (incl. the sandboxed nix build), not just one dev box.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        manifest.join("../../praxis/headroom-strategies").join(name),
        manifest.join("../../praxis").join(name), // e.g. headroom.px at praxis/ root
    ];
    for p in &candidates {
        if p.exists() {
            return std::fs::read_to_string(p)
                .unwrap_or_else(|e| panic!("failed reading fixture {}: {}", p.display(), e));
        }
    }
    panic!(
        "Cannot find fixture {} (looked in {:?})",
        name,
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
    );
}

fn compile_procedure(source: &str, proc_name: &str) -> Value {
    let doc = parse(source).unwrap_or_else(|e| panic!("Parse failed: {}", e));
    let records = compile(&doc);
    for rec in &records {
        if let Some(name) = rec.data.get("name").and_then(|v| v.as_str()) {
            if name == proc_name {
                return rec.data.clone();
            }
        }
    }
    panic!(
        "Procedure '{}' not found. Available: {:?}",
        proc_name,
        records
            .iter()
            .filter_map(|r| r.data.get("name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// REAL SIDE-EFFECT ACTOR TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn e2e_detect_json() {
    let (h, _) = make_handler();
    let r = h.call("detect_content_type", &json!({"content": r#"[{"id":1}]"#})).unwrap();
    assert_eq!(r["content_type"], "json");
    assert!(r["confidence"].as_f64().unwrap() > 0.8);
}

#[test]
fn e2e_detect_code() {
    let (h, _) = make_handler();
    let r = h.call("detect_content_type", &json!({"content": "fn main() {\n    println!(\"hi\");\n}"})).unwrap();
    assert_eq!(r["content_type"], "code");
}

#[test]
fn e2e_detect_log() {
    let (h, _) = make_handler();
    let r = h.call("detect_content_type", &json!({"content": "2026-01-15T10:30:00Z ERROR db fail\n2026-01-15T10:30:01Z WARN retry\n2026-01-15T10:30:02Z ERROR conn refused\n2026-01-15T10:30:03Z INFO recovered"})).unwrap();
    assert_eq!(r["content_type"], "log");
}

#[test]
fn e2e_detect_prose() {
    let (h, _) = make_handler();
    let r = h.call("detect_content_type", &json!({"content": "The quick brown fox jumps over the lazy dog."})).unwrap();
    assert_eq!(r["content_type"], "prose");
}

#[test]
fn e2e_hash_deterministic() {
    let (h, _) = make_handler();
    let h1 = h.call("compute_content_hash", &json!({"content": "hello"})).unwrap();
    let h2 = h.call("compute_content_hash", &json!({"content": "hello"})).unwrap();
    assert_eq!(h1["hash"], h2["hash"]);
    let h3 = h.call("compute_content_hash", &json!({"content": "world"})).unwrap();
    assert_ne!(h1["hash"], h3["hash"]);
    assert!(h1["hash"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn e2e_token_counting() {
    let (h, _) = make_handler();
    let r = h.call("count_tokens", &json!({"content": ""})).unwrap();
    assert_eq!(r["tokens"], 0);
    let r = h.call("count_tokens", &json!({"content": "Hello world test tokens here."})).unwrap();
    let n = r["tokens"].as_u64().unwrap();
    assert!(n > 3 && n < 20, "Expected 3-20 tokens, got {}", n);
}

#[test]
fn e2e_sentence_splitting() {
    let (h, _) = make_handler();
    let r = h.call("split_sentences", &json!({"content": "First. Second. Third."})).unwrap();
    let s = r["sentences"].as_array().unwrap();
    assert!(s.len() >= 3, "Expected >=3 sentences, got {}", s.len());
}

#[test]
fn e2e_cosine_similarity() {
    let (h, _) = make_handler();
    let r = h.call("cosine_similarity", &json!({"a": [1,0,0], "b": [1,0,0]})).unwrap();
    assert!((r["score"].as_f64().unwrap() - 1.0).abs() < 1e-6);
    let r = h.call("cosine_similarity", &json!({"a": [1,0], "b": [0,1]})).unwrap();
    assert!(r["score"].as_f64().unwrap().abs() < 1e-6);
}

#[test]
fn e2e_extract_signatures() {
    let (h, _) = make_handler();
    let code = "pub fn process(input: &str) -> Result<()> {}\nfn helper() -> bool {}";
    let r = h.call("extract_ast_signatures", &json!({"content": code, "language": "rust"})).unwrap();
    let sigs = r["signatures"].as_array().unwrap();
    assert!(sigs.len() >= 2, "Expected >=2 sigs, got {:?}", sigs);
}

#[test]
fn e2e_pluresdb_roundtrip() {
    let (h, _) = make_handler();
    h.call("pluresdb_write", &json!({"key": "headroom:e2e:test", "value": {"x": 42}})).unwrap();
    let r = h.call("pluresdb_read", &json!({"key": "headroom:e2e:test"})).unwrap();
    assert_eq!(r["value"]["x"], 42);
    let q = h.call("pluresdb_query", &json!({"prefix": "headroom:e2e:"})).unwrap();
    assert!(q["keys"].as_array().unwrap().iter().any(|k| k == "headroom:e2e:test"));
}

// ═══════════════════════════════════════════════════════════════════════
// .PX PROCEDURE EXECUTION WITH REAL HANDLER
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn e2e_router_classify_content() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("router.px"), "classify_content");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "classify_content failed: {:?}", result.error);
    assert!(result.variables.contains_key("detection"));
}

#[test]
fn e2e_router_route_json() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("router.px"), "route_json");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "route_json failed: {:?}", result.error);
}

#[test]
fn e2e_router_route_code() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("router.px"), "route_code");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "route_code failed: {:?}", result.error);
}

#[test]
fn e2e_router_route_prose() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("router.px"), "route_prose");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "route_prose failed: {:?}", result.error);
}

#[test]
fn e2e_router_route_log() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("router.px"), "route_log");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "route_log failed: {:?}", result.error);
}

#[test]
fn e2e_pipeline_compress_context() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("pipeline.px"), "compress_context");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_context failed: {:?}", result.error);
    assert!(result.variables.contains_key("output"));
    assert!(result.variables.contains_key("result"));
}

#[test]
fn e2e_pipeline_shared_memory() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("pipeline.px"), "compress_with_shared_memory");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_with_shared_memory failed: {:?}", result.error);
}

#[test]
fn e2e_scorer_score_block() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("scorer.px"), "score_block");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "score_block failed: {:?}", result.error);
}

#[test]
fn e2e_scorer_assign_severity() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("scorer.px"), "assign_severity");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "assign_severity failed: {:?}", result.error);
}

#[test]
fn e2e_code_compress() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("code.px"), "compress_code");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_code failed: {:?}", result.error);
}

#[test]
fn e2e_code_bodies_only() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("code.px"), "compress_bodies_only");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_bodies_only failed: {:?}", result.error);
}

#[test]
fn e2e_prose_compress() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("prose.px"), "compress_prose");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_prose failed: {:?}", result.error);
}

#[test]
fn e2e_prose_conversation() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("prose.px"), "compress_conversation");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_conversation failed: {:?}", result.error);
}

#[test]
fn e2e_log_compress() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("log.px"), "compress_log");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_log failed: {:?}", result.error);
}

#[test]
fn e2e_fitter_calculate_budget() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("fitter.px"), "calculate_budget");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "calculate_budget failed: {:?}", result.error);
}

#[test]
fn e2e_fitter_fit_to_budget() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("fitter.px"), "fit_to_budget");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "fit_to_budget failed: {:?}", result.error);
}

#[test]
fn e2e_cache_align() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("cache.px"), "align_cache_prefix");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "align_cache_prefix failed: {:?}", result.error);
}

#[test]
fn e2e_ccr_store() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("ccr.px"), "store_for_retrieval");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "store_for_retrieval failed: {:?}", result.error);
}

#[test]
fn e2e_memory_ingest() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("memory.px"), "ingest_memory");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "ingest_memory failed: {:?}", result.error);
}

#[test]
fn e2e_crusher_json_array() {
    let (h, _) = make_handler();
    let proc = compile_procedure(&load_px("crusher.px"), "compress_json_array");
    let result = executor::execute(&proc, &h).unwrap();
    assert!(result.success, "compress_json_array failed: {:?}", result.error);
}

// ═══════════════════════════════════════════════════════════════════════
// FULL SWEEP — every procedure with real handler
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn e2e_all_procedures_with_real_handler() {
    let fixtures = &[
        "router.px", "pipeline.px", "scorer.px", "code.px", "prose.px",
        "fitter.px", "crusher.px", "cache.px", "ccr.px", "memory.px", "log.px",
    ];
    let mut total = 0;
    let mut failures: Vec<String> = Vec::new();

    for fixture in fixtures {
        let (handler, _) = make_handler();
        let source = load_px(fixture);
        let doc = match parse(&source) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}: parse error: {}", fixture, e));
                continue;
            }
        };
        let records = compile(&doc);
        for rec in &records {
            let name = rec.data.get("name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
            if rec.data.get("steps").is_none() {
                continue;
            }
            total += 1;
            match executor::execute(&rec.data, &handler) {
                Ok(result) => {
                    if !result.success {
                        failures.push(format!(
                            "{}::{} failed: {:?}", fixture, name, result.error
                        ));
                    }
                }
                Err(e) => {
                    failures.push(format!("{}::{} error: {}", fixture, name, e));
                }
            }
        }
    }

    if !failures.is_empty() {
        panic!(
            "Failed {}/{} procedures with real handler:\n{}",
            failures.len(), total, failures.join("\n")
        );
    }
    assert!(total >= 20, "Expected >=20 procedures, found {}", total);
    eprintln!("E2E: {} procedures executed with real HeadroomActionHandler", total);
}

// ═══════════════════════════════════════════════════════════════════════
// PERFORMANCE — pipeline latency target (<100ms)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn e2e_pipeline_latency_under_100ms() {
    let (handler, _) = make_handler();
    let proc = compile_procedure(&load_px("pipeline.px"), "compress_context");

    let start = std::time::Instant::now();
    let result = executor::execute(&proc, &handler).unwrap();
    let elapsed = start.elapsed();

    assert!(result.success, "compress_context failed: {:?}", result.error);
    assert!(
        elapsed.as_millis() < 100,
        "Pipeline took {}ms, exceeds 100ms target",
        elapsed.as_millis()
    );
    eprintln!("Pipeline latency: {}ms", elapsed.as_millis());
}
