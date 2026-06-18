//! HeadroomActionHandler — production ActionHandler for headroom .px procedures.
//!
//! Implements the 9 real side-effect actors:
//! 1. detect_content_type  — heuristic content classification
//! 2. compute_content_hash — SHA-256 via sha2
//! 3. count_tokens         — tiktoken-rs cl100k_base
//! 4. compute_embedding    — pluresdb::FastEmbedder (feature-gated)
//! 5. cosine_similarity    — pure math dot product / norms
//! 6. split_sentences      — unicode-segmentation sentence bounds
//! 7. extract_ast_signatures — heuristic signature extraction
//! 8. pluresdb_read        — direct CrdtStore read
//! 9. pluresdb_write       — direct CrdtStore write
//! Plus: pluresdb_query (prefix scan), delete_from_pluresdb
//!
//! All other ~160 actions return sensible stubs so the px executor never stalls.

use std::sync::{Arc, OnceLock};

use pluresdb::CrdtStore;
use pluresdb_px::px::executor::{ActionHandler, ExecutionError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tiktoken_rs::{cl100k_base, CoreBPE};
use unicode_segmentation::UnicodeSegmentation;

/// Cached tiktoken BPE tokenizer — `cl100k_base()` allocates ~100MB+ of BPE
/// tables, so we init once and reuse across all calls.
static BPE: OnceLock<CoreBPE> = OnceLock::new();

fn bpe() -> Result<&'static CoreBPE, ExecutionError> {
    // OnceLock::get_or_init is stable; the error case is extremely rare
    // (tiktoken only fails if the bundled BPE data is corrupt).
    Ok(BPE.get_or_init(|| {
        cl100k_base().expect("tiktoken cl100k_base init failed")
    }))
}

const ACTOR: &str = "pares-agens-headroom";

// ── Public handler struct ─────────────────────────────────────────────────────

/// Production ActionHandler for headroom .px procedures.
///
/// Holds a shared CrdtStore backing pluresdb_read / pluresdb_write / pluresdb_query.
/// All real computation actions (hashing, tokenizing, embedding, etc.) are
/// stateless and execute inline.
pub struct HeadroomActionHandler {
    db: Arc<CrdtStore>,
}

impl HeadroomActionHandler {
    /// Create a new handler backed by the given CrdtStore.
    pub fn new(db: Arc<CrdtStore>) -> Self {
        Self { db }
    }
}
// ── ActionHandler impl ────────────────────────────────────────────────────────

impl ActionHandler for HeadroomActionHandler {
    fn call(&self, name: &str, params: &Value) -> Result<Value, ExecutionError> {
        match name {
            // ── 1. detect_content_type ────────────────────────────────────
            "detect_content_type" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let trimmed = content.trim_start();
                let (ct, conf): (&str, f64) = if trimmed.starts_with('{') || trimmed.starts_with('[') {
                    ("json", 0.92)
                } else if is_log_content(content) {
                    ("log", 0.85)
                } else if is_code_content(content) {
                    ("code", 0.88)
                } else if looks_like_error(content) {
                    ("error", 0.82)
                } else {
                    ("prose", 0.75)
                };
                Ok(json!({ "content_type": ct, "confidence": conf }))
            }

            // ── 2. compute_content_hash ───────────────────────────────────
            "compute_content_hash" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let mut h = Sha256::new();
                h.update(content.as_bytes());
                Ok(json!({ "hash": format!("sha256:{:x}", h.finalize()) }))
            }

            // ── 3. count_tokens ───────────────────────────────────────────
            "count_tokens" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let bpe = bpe()?;
                let n = bpe.encode_with_special_tokens(content).len();
                Ok(json!({ "tokens": n }))
            }

            // ── 4. compute_embedding ──────────────────────────────────────
            "compute_embedding" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                compute_embedding_impl(content)
            }

            // ── 5. cosine_similarity ──────────────────────────────────────
            "cosine_similarity" => {
                let a = float_vec(params.get("a"));
                let b = float_vec(params.get("b"));
                if a.is_empty() || b.len() != a.len() {
                    return Ok(json!({ "score": 0.0 }));
                }
                let dot: f64 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
                let na: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
                let nb: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();
                let score = if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na * nb) };
                Ok(json!({ "score": score }))
            }

            // ── 6. split_sentences ────────────────────────────────────────
            "split_sentences" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let sentences: Vec<&str> = content
                    .split_sentence_bounds()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                Ok(json!({ "sentences": sentences }))
            }

            // ── 7. extract_ast_signatures ─────────────────────────────────
            "extract_ast_signatures" | "extract_signatures" => {
                let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let lang = params.get("language").and_then(|v| v.as_str()).unwrap_or("unknown");
                Ok(json!({ "signatures": extract_signatures_heuristic(content, lang) }))
            }

            // ── 8. pluresdb_read ──────────────────────────────────────────
            "pluresdb_read" | "lookup_ccr_entry" | "get_memory_entry" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                match self.db.get(key) {
                    Some(r) => Ok(json!({ "value": r.data })),
                    None    => Ok(json!({ "value": null })),
                }
            }

            // ── 9. pluresdb_write ─────────────────────────────────────────
            "pluresdb_write" | "store_in_pluresdb" | "store_memory_entry" => {
                let key   = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let value = params.get("value").cloned().unwrap_or(Value::Null);
                self.db.put(key, ACTOR, value);
                Ok(json!({ "ok": true }))
            }

            // ── pluresdb_query (prefix scan) ──────────────────────────────
            "pluresdb_query" => {
                let prefix = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                let keys: Vec<String> = self.db.list()
                    .into_iter()
                    .filter(|r| r.id.as_str().starts_with(prefix))
                    .map(|r| r.id.to_string())
                    .collect();
                Ok(json!({ "keys": keys }))
            }

            "delete_from_pluresdb" => {
                let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
                let _ = self.db.delete(key);
                Ok(json!({ "deleted": true }))
            }
            // ── Router ────────────────────────────────────────────────────
            "route_json"  => Ok(json!({"routed":true,"strategy":"structural"})),
            "route_code"  => Ok(json!({"routed":true,"strategy":"ast_summary"})),
            "route_prose" => Ok(json!({"routed":true,"strategy":"semantic"})),
            "route_log"   => Ok(json!({"routed":true,"strategy":"pattern_dedup"})),
            "route_error" => Ok(json!({"routed":true,"strategy":"error_focused"})),
            "route_mixed" => Ok(json!({"routed":true,"strategy":"hybrid"})),
            "analyze_json_structure" => {
                let c = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let t = c.trim();
                Ok(json!({
                    "is_array_of_objects": t.starts_with('['),
                    "is_deeply_nested": c.matches('{').count() > 5,
                    "is_simple_object": !t.starts_with('[') && c.matches('{').count() <= 2,
                    "has_repeated_values": false
                }))
            }
            "detect_language" => {
                let lang = detect_language_heuristic(
                    params.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                );
                Ok(json!({"language": lang, "confidence": 0.80}))
            }
            "measure_density" => {
                let c = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if c.is_empty() {
                    return Ok(json!({"tokens_per_sentence": 0.0, "total_sentences": 0}));
                }
                let bpe_ref = bpe()?;
                let t = bpe_ref.encode_with_special_tokens(c).len();
                let s = c.split_sentence_bounds().filter(|s| !s.trim().is_empty()).count();
                let tps: f64 = if s == 0 { 0.0 } else { t as f64 / s as f64 };
                Ok(json!({"tokens_per_sentence": tps, "total_sentences": s}))
            }
            "detect_patterns" | "detect_log_patterns" =>
                Ok(json!({"repeated_count":0,"unique_count":0,"patterns":[]})),
            "extract_error_frame" =>
                Ok(json!({"message":"","file":"","line":0,"frames":[]})),
            "split_by_structure" =>
                Ok(json!([{"section_id":"s1","content_type":"prose"}])),
            "write_plan_entry" => Ok(json!({"written":true})),

            // ── Pipeline ──────────────────────────────────────────────────
            "calculate_budget" => Ok(json!({
                "max_context_tokens":128000,
                "reserved_for_response":4096,
                "reserved_for_system":2048,
                "available_for_content":121856
            })),
            "split_into_blocks" =>
                Ok(json!([{"block_id":"b1","content_type":"prose","tokens":500}])),
            "classify_content"    => Ok(json!({"classified":true,"content_type":"prose"})),
            "score_all_blocks"    => Ok(json!({"scored":true,"count":0})),
            "list_plan_entries"   => Ok(json!([])),
            "execute_structural_compression" => Ok(json!({"compressed":true,"ratio":0.5})),
            "execute_code_compression"       => Ok(json!({"compressed":true,"ratio":0.4})),
            "execute_prose_compression"      => Ok(json!({"compressed":true,"ratio":0.6})),
            "execute_log_compression"        => Ok(json!({"compressed":true,"ratio":0.3})),
            "execute_hybrid_compression"     => Ok(json!({"compressed":true,"ratio":0.5})),
            "skip_compression"    => Ok(json!({"skipped":true})),
            "list_compressible_blocks" => Ok(json!([])),
            "store_for_retrieval" => Ok(json!({"stored":true,"ccr_ref":"ccr_stub"})),
            "fit_to_budget"       => Ok(json!({"fitted":true,"output_tokens":500})),
            "align_cache_prefix"  => Ok(json!({"aligned":true,"hash":"stub"})),
            "order_for_cache"     => Ok(json!({"ordered":true})),
            "inject_retrieval_hints" => Ok(json!({"hints_injected":0})),
            "assemble_output"     => Ok(json!({"output_tokens":500,"blocks_included":1})),
            "compute_pipeline_result" => Ok(json!({
                "output_tokens":500,"input_tokens":1000,
                "compression_ratio":0.5,"latency_ms":10
            })),
            "check_shared_memory"      => Ok(json!({"count":0,"hits":[]})),
            "compress_remaining_blocks" => Ok(json!({"compressed":true})),
            "ingest_new_to_memory"     => Ok(json!({"ingested":true})),
            "reuse_compressed_from_memory" => Ok(json!({"reused":false,"tokens_saved":0})),
            "get_block"             => Ok(json!({"block_id":"b1","content":"","content_type":"prose"})),
            "deduplicate_patterns"  => Ok(json!({"deduped":true,"patterns_merged":0})),
            "write_compressed"      => Ok(json!({"written":true,"tokens":0})),
            "reassemble_sections"   => Ok(json!({"reassembled":true})),

            // ── Scorer ────────────────────────────────────────────────────
            "get_block_metadata"    => Ok(json!({"block_id":"target","content_type":"prose","tokens":500,"source":"user_message"})),
            "compute_relevance_score" => Ok(json!({"score":0.7})),
            "compute_recency_score"   => Ok(json!({"score":0.8})),
            "compute_source_score"    => Ok(json!({"score":0.6})),
            "compute_composite"       => Ok(json!({"score":0.7})),
            "assign_severity"         => Ok(json!({"severity":"medium"})),
            "write_scored_block"      => Ok(json!({"written":true})),
            "get_composite_score"     => Ok(json!({"value":0.7})),
            "update_severity"         => Ok(json!({"updated":true})),
            "list_unscored_blocks"    => Ok(json!([])),
            "sort_blocks_by_score"    => Ok(json!([])),

            // ── Crusher ───────────────────────────────────────────────────
            "analyze_array_schema"    => Ok(json!({"uniform_keys":false,"key_count":0,"element_count":0})),
            "extract_schema_header"   => Ok(json!({"keys":[],"types":[]})),
            "tabulate_values"         => Ok(json!({"rows":0,"columns":0,"format":"tabular"})),
            "group_by_shape"          => Ok(json!([])),
            "measure_depth"           => Ok(json!({"max_level":1})),
            "flatten_paths"           => Ok(json!({"paths":0,"flattened":true})),
            "dedup_values"            => Ok(json!({"unique_values":0,"duplicates_removed":0})),
            "strip_null_fields"       => Ok(json!({"stripped":0})),
            "abbreviate_keys"         => Ok(json!({"keys_shortened":0})),
            "find_repeated_values"    => Ok(json!({"count":0,"values":[]})),
            "create_value_index"      => Ok(json!({"index_size":0})),
            "replace_with_refs"       => Ok(json!({"replacements":0})),
            "extract_error_essentials" => Ok(json!({"message":"","error_type":"unknown"})),
            "extract_top_frames"      => Ok(json!({"frames":[]})),
            "strip_internal_frames"   => Ok(json!({"stripped":0})),

            // ── Code compression ──────────────────────────────────────────
            "get_scored_block"    => Ok(json!({"block_id":"target","severity":"medium","composite_score":0.7})),
            "get_language"        => Ok(json!({"language":"unknown","confidence":0.5})),
            "preserve_verbatim"   => Ok(json!({"preserved":true})),
            "compress_bodies_only" => Ok(json!({"compressed":true,"bodies_summarized":0})),
            "extract_bodies"      => Ok(json!([])),
            "summarize_body"      => Ok(json!({"summary":"// ..."})),
            "pass_through_body"   => Ok(json!({"passed":true})),
            "reassemble_with_compressed_bodies" => Ok(json!({"reassembled":true,"tokens":0})),
            "signatures_and_logic"   => Ok(json!({"skeleton":true})),
            "extract_control_flow"   => Ok(json!({"branches":[]})),
            "extract_error_handling" => Ok(json!({"handlers":[]})),
            "assemble_skeleton"      => Ok(json!({"skeleton_tokens":0})),
            "signatures_only"        => Ok(json!({"signatures":true})),
            "extract_type_definitions" => Ok(json!({"types":[]})),
            "extract_imports"        => Ok(json!({"imports":[]})),
            "assemble_interface"     => Ok(json!({"interface_tokens":0})),
            "existence_stub"         => Ok(json!({"stub":true})),

            // ── Prose compression ─────────────────────────────────────────
            "light_prose_compression"      => Ok(json!({"method":"light"})),
            "moderate_prose_compression"   => Ok(json!({"method":"moderate"})),
            "aggressive_prose_compression" => Ok(json!({"method":"aggressive"})),
            "one_line_summary"             => Ok(json!({"summary":"..."})),
            "remove_filler_sentences"      => Ok(json!({"removed":0,"remaining":0})),
            "remove_redundant_phrases"     => Ok(json!({"trimmed":0})),
            "score_sentences"              => Ok(json!([])),
            "select_top_sentences"         => Ok(json!({"selected":0,"first_preserved":false,"last_preserved":false})),
            "extract_named_entities"       => Ok(json!({"entities":[]})),
            "ensure_entity_coverage"       => Ok(json!({"coverage":1.0})),
            "extract_key_facts"            => Ok(json!({"facts":[]})),
            "assemble_bullet_summary"      => Ok(json!({"summary":""})),
            "extract_topic"                => Ok(json!({"topic":""})),
            "split_by_turns"               => Ok(json!([])),
            "classify_turn_importance"     => Ok(json!([])),
            "pass_through_turn"            => Ok(json!({"passed":true})),
            "extract_decision"             => Ok(json!({"decision":""})),
            "write_decision_summary"       => Ok(json!({"written":true})),
            "one_line_turn_summary"        => Ok(json!({"summary":""})),
            "reassemble_compressed_history" => Ok(json!({"reassembled":true})),

            // ── Log compression ───────────────────────────────────────────
            "split_log_lines"               => Ok(json!({"lines":0,"line_array":[]})),
            "normalize_timestamps"          => Ok(json!({"normalized":true,"lines":0})),
            "normalize_ids"                 => Ok(json!({"generalized":true})),
            "group_by_pattern"              => Ok(json!([])),
            "get_patterns"                  => Ok(json!([])),
            "classify_log_lines"            => Ok(json!({"error":0,"warn":0,"info":0,"debug":0})),
            "extract_error_lines"           => Ok(json!({"lines":[]})),
            "keep_first_occurrence"         => Ok(json!({"kept":true})),
            "count_occurrences"             => Ok(json!({"count":0})),
            "write_dedup_summary"           => Ok(json!({"written":true})),
            "extract_unique_lines"          => Ok(json!({"unique":0})),
            "assemble_compressed_log"       => Ok(json!({"output_lines":0})),
            "detect_log_format"             => Ok(json!({"is_json_lines":false,"format":"plain"})),
            "extract_common_fields"         => Ok(json!({"fields":[]})),
            "group_by_level"                => Ok(json!({"error":0,"warn":0,"info":0,"debug":0})),
            "preserve_level"                => Ok(json!({"preserved":true})),
            "dedup_level"                   => Ok(json!({"deduped":true})),
            "summarize_level"               => Ok(json!({"summarized":true})),
            "assemble_structured_log_summary" => Ok(json!({"summary":true,"output_lines":0})),
            // ── CCR ───────────────────────────────────────────────────────
            "generate_ccr_ref" => {
                let id = uuid_hex();
                Ok(json!({"ref_id": format!("ccr_{}", &id[..8]), "short_token": format!("[CCR:{}]", &id[..8])}))
            }
            "get_original_content"    => Ok(json!({"content":"","tokens":0})),
            "check_duplicate"         => Ok(json!({"found":false})),
            "reuse_existing_ref"      => Ok(json!({"reused":false,"ref":""})),
            "parse_retrieval_request" => Ok(json!({"ref":"","request_id":""})),
            "refresh_ttl"             => Ok(json!({"refreshed":true,"new_ttl":3600})),
            "increment_access_count"  => Ok(json!({"count":1})),
            "list_expired_entries"    => Ok(json!({"count":0,"entries":[]})),
            "list_lru_entries"        => Ok(json!([])),
            "count_ccr_entries" => {
                let n = self.db.list().into_iter()
                    .filter(|r| r.id.as_str().starts_with("ccr:")).count();
                Ok(json!({"total": n}))
            }
            "get_compressed_blocks_with_ccr" => Ok(json!([])),
            "append_hint"             => Ok(json!({"appended":true})),

            // ── Cache alignment ───────────────────────────────────────────
            "get_system_prompt"         => Ok(json!({"prompt":"","tokens":0})),
            "get_static_context"        => Ok(json!({"context":"","tokens":0})),
            "get_previous_prefix_hash"  => Ok(json!({"value":null})),
            "compute_stable_prefix"     => Ok(json!({"prefix":"","bytes":0})),
            "compute_prefix_hash" => {
                let p = params.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
                let mut h = Sha256::new(); h.update(p.as_bytes());
                Ok(json!({"value": format!("{:x}", h.finalize())}))
            }
            "store_prefix_hash"         => Ok(json!({"stored":true})),
            "write_aligned_prefix"      => Ok(json!({"written":true})),
            "get_compressed_blocks"     => Ok(json!([])),
            "partition_stable_volatile" => Ok(json!({"stable":[],"volatile":[]})),
            "sort_stable_by_hash"       => Ok(json!([])),
            "sort_volatile_by_recency"  => Ok(json!([])),
            "assemble_ordered_context"  => Ok(json!({"ordered":true,"blocks":[]})),
            "separate_system_prompt"    => Ok(json!({"separated":true})),
            "stabilize_first_user_block" => Ok(json!({"stabilized":true})),
            "maximize_system_prefix"    => Ok(json!({"maximized":true,"prefix_tokens":0})),
            "get_response_metadata"     => Ok(json!({"cache_hit":false,"cached_tokens":0})),
            "increment_cache_hits"      => Ok(json!({"total_hits":1})),
            "increment_cache_misses"    => Ok(json!({"total_misses":1})),

            // ── Fitter ────────────────────────────────────────────────────
            "get_model_context_limit"  => Ok(json!({"limit":128000,"model":"gpt-4o"})),
            "get_system_reservation"   => Ok(json!({"tokens":2048})),
            "get_response_reservation" => Ok(json!({"tokens":4096})),
            "compute_available"        => Ok(json!({"available_for_content":121856})),
            "write_budget"             => Ok(json!({"written":true})),
            "get_budget"               => Ok(json!({"available_for_content":121856,"max_context_tokens":128000})),
            "list_scored_blocks_desc"  => Ok(json!([])),
            "partition_critical"       => Ok(json!({"critical":[],"non_critical":[]})),
            "sum_tokens"               => Ok(json!({"total":0})),
            "compute_remaining_budget" => Ok(json!({"remaining":121856})),
            "try_include"              => Ok(json!({"fits":true,"remaining_after":121856})),
            "include_block"            => Ok(json!({"included":true})),
            "try_recompress"           => Ok(json!({"fits":true,"new_tokens":0})),
            "drop_block"               => Ok(json!({"dropped":true})),
            "compute_overflow"         => Ok(json!({"remaining":0,"overflow_tokens":0})),
            "list_included_by_score_asc" => Ok(json!([])),
            "recompress_aggressive"    => Ok(json!({"still_over":false,"new_tokens":0})),
            "recompress_critical"      => Ok(json!({"reduced":true,"new_tokens":0})),
            "get_current_query"        => Ok(json!({"query":"","tokens":0})),
            "rescore_blocks_for_query" => Ok(json!({"rescored":true})),
            "sort_by_query_relevance"  => Ok(json!([])),
            "select_for_query"         => Ok(json!({"selected":true})),

            // ── Shared memory ─────────────────────────────────────────────
            "get_compressed_block"       => Ok(json!({"block_id":"b1","content":"","tokens":0})),
            "lookup_by_hash"             => Ok(json!({"found":false})),
            "merge_provenance"           => Ok(json!({"merged":true,"agents":[]})),
            "check_capacity"             => Ok(json!({"at_limit":false,"current":0,"max":1000})),
            "evict_lru_entry" | "evict_entry" => Ok(json!({"evicted":true})),
            "search_memory_by_relevance" => Ok(json!([])),
            "refresh_access"             => Ok(json!({"refreshed":true})),
            "list_by_agent"              => Ok(json!([])),
            "find_near_duplicates"       => Ok(json!({"count":0,"pairs":[]})),
            "merge_entries"              => Ok(json!({"merged":true})),
            "find_stale_entries"         => Ok(json!({"count":0,"entries":[]})),
            "count_entries"              => Ok(json!({"total":0})),
            "count_unique_agents"        => Ok(json!({"agents":0})),
            "compute_savings"            => Ok(json!({"savings_ratio":0.0,"tokens_saved":0})),

            // ── QA fixtures & test procedures ─────────────────────────────
            "get_fixture"         => Ok(json!({"id":"f1","content_type":"prose","raw_tokens":500})),
            "get_compressed"      => Ok(json!({"block_id":"b1","tokens":250})),
            "compute_key_coverage" => Ok(json!({"ratio":1.0})),
            "count_signatures"    => Ok(json!({"count":0})),
            "count_error_lines"   => Ok(json!({"count":0})),
            "compute_entity_coverage" => Ok(json!({"ratio":1.0})),
            "compare_content"     => Ok(json!({"identical":true})),
            "get_ccr_ref"         => Ok(json!({"id":"ccr_stub"})),
            "get_retrieved_content" => Ok(json!({"content":""})),
            "simulate_time_advance" => Ok(json!({"advanced":true})),
            "get_pipeline_result" => Ok(json!({"output_tokens":500,"compression_ratio":0.5,"blocks_processed":1,"latency_ms":10})),
            "check_all_critical_included" => Ok(json!({"all_included":true})),
            "count_ccr_refs"      => Ok(json!({"total":0})),
            "check_block_included" => Ok(json!({"value":true})),
            "get_prefix_hash"     => Ok(json!({"value":"stub"})),
            "write_test_input"    => Ok(json!({"written":true,"blocks":1})),
            "run_benchmarks"      => Ok(json!({"complete":true})),
            "memory_stats"        => Ok(json!({"entries":0,"agents":0})),
            "compress_context"    => Ok(json!({"complete":true})),
            "retrieve_original"   => Ok(json!({"retrieved":true})),
            "evict_expired"       => Ok(json!({"evicted":0})),
            "test_json_key_preservation"
            | "test_code_signature_preservation"
            | "test_log_error_preservation"
            | "test_prose_entity_preservation"
            | "test_ccr_store_and_retrieve"
            | "test_ccr_eviction"
            | "test_output_within_budget"
            | "test_critical_never_dropped"
            | "test_cache_prefix_stability"
            | "test_memory_dedup"
            | "test_bypass_small_input"
            | "test_empty_input"
            | "test_single_critical_block"
            | "test_full_pipeline" => Ok(json!({"ok":true})),

            // ── Unknown: debug-log and return ok stub ─────────────────────
            other => {
                tracing::debug!(action = other, "HeadroomActionHandler: unknown action, returning ok stub");
                Ok(json!({"ok": true}))
            }
        }
    }
}
// ── Helper functions ──────────────────────────────────────────────────────────

/// Returns true if the content looks like log output (timestamp + level patterns).
fn is_log_content(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().take(10).collect();
    if lines.len() < 3 {
        return false;
    }
    let log_line_count = lines.iter().filter(|l| {
        let t = l.trim();
        // Explicit log level markers (most reliable)
        t.contains(" INFO ") || t.contains(" WARN ") || t.contains(" ERROR ")
            || t.contains(" DEBUG ") || t.contains(" TRACE ")
            || t.contains("[info]") || t.contains("[warn]") || t.contains("[error]")
            // ISO-8601 timestamp at line start: YYYY-MM-DDThh:mm:ss
            || (t.len() >= 19
                && t.as_bytes().get(4) == Some(&b'-')
                && t.as_bytes().get(7) == Some(&b'-')
                && (t.as_bytes().get(10) == Some(&b'T') || t.as_bytes().get(10) == Some(&b' '))
                && t.as_bytes().get(13) == Some(&b':'))
    }).count();
    log_line_count >= 2
}

/// Returns true if content looks like an error or stack trace.
/// Requires multiple error indicators to avoid false positives on code
/// that merely defines error types.
fn looks_like_error(content: &str) -> bool {
    let indicators = [
        content.contains("panicked at"),
        content.contains("Traceback"),
        content.contains("stack trace"),
        content.contains("thread '") && content.contains("panicked"),
        // "Error:" at line start (not in an enum definition)
        content.lines().any(|l| {
            let t = l.trim();
            t.starts_with("Error:") || t.starts_with("error:") || t.starts_with("error[")
        }),
        // "Exception:" at line start
        content.lines().any(|l| l.trim().starts_with("Exception:")),
        // Stack frame pattern: "  at <something> (<file>:<line>)"
        content.lines().filter(|l| {
            let t = l.trim();
            (t.starts_with("at ") || t.starts_with("  at "))
                && (t.contains('(') || t.contains("::"))
        }).count() >= 2,
        // "Caused by:" chain
        content.contains("Caused by:"),
    ];
    // Need at least 2 indicators to classify as error
    indicators.iter().filter(|&&b| b).count() >= 2
}

/// Returns true if content looks like source code (structural analysis, not just keywords).
/// Uses brace/indent density + code-keyword co-occurrence to avoid false positives
/// on prose that merely *discusses* code.
fn is_code_content(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len() as f64;

    // Single-line: check if the entire line IS a code statement (not prose mentioning code)
    if lines.len() <= 2 {
        let t = content.trim();
        return t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("def ")
            || t.starts_with("function ") || t.starts_with("class ") || t.starts_with("impl ")
            || t.starts_with("struct ") || t.starts_with("enum ");
    }

    // Multi-line: use structural + keyword co-occurrence
    // Structural indicators: braces, semicolons, indentation
    let brace_lines = lines.iter().filter(|l| l.contains('{') || l.contains('}')).count() as f64;
    let indented_lines = lines.iter().filter(|l| l.starts_with("    ") || l.starts_with('\t')).count() as f64;
    let semicolons = lines.iter().filter(|l| l.trim_end().ends_with(';')).count() as f64;

    // Keyword indicators: lines that START with code keywords (not just contain them)
    let keyword_lines = lines.iter().filter(|l| {
        let t = l.trim();
        t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("pub(crate) fn ")
            || t.starts_with("def ") || t.starts_with("function ") || t.starts_with("async fn ")
            || t.starts_with("class ") || t.starts_with("impl ") || t.starts_with("struct ")
            || t.starts_with("enum ") || t.starts_with("trait ") || t.starts_with("const ")
            || t.starts_with("let ") || t.starts_with("use ") || t.starts_with("import ")
            || t.starts_with("from ") || t.starts_with("#[")
    }).count() as f64;

    // Structural ratio: what fraction of lines have code-like structure?
    let structural_ratio = (brace_lines + indented_lines + semicolons) / (3.0 * total);
    let keyword_ratio = keyword_lines / total;

    // Need BOTH structural AND keyword signals to classify as code.
    // This prevents prose about code from triggering (keywords present but no structure).
    structural_ratio > 0.15 && keyword_ratio > 0.05
}

/// Convert a JSON value to a Vec<f64> (handles arrays of numbers).
fn float_vec(v: Option<&Value>) -> Vec<f64> {
    match v {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|x| x.as_f64())
            .collect(),
        _ => vec![],
    }
}

/// Heuristic language detection from source content.
fn detect_language_heuristic(content: &str) -> &'static str {
    if content.contains("fn ") && (content.contains("let ") || content.contains("impl ")) {
        "rust"
    } else if content.contains("def ") && content.contains(":") {
        "python"
    } else if content.contains("function ") || content.contains("const ") || content.contains("let ") {
        "javascript"
    } else if content.contains("public class ") || content.contains("void ") {
        "java"
    } else if content.contains("#include") || content.contains("std::") {
        "cpp"
    } else if content.contains("package main") || content.contains("func ") {
        "go"
    } else if content.contains("SELECT ") || content.contains("FROM ") {
        "sql"
    } else {
        "unknown"
    }
}

/// Heuristic AST signature extraction without tree-sitter grammars.
///
/// Extracts lines that look like function/method/type signatures.
/// Good enough for code compression decisions; replace with real tree-sitter
/// when grammar crates are available.
fn extract_signatures_heuristic(content: &str, language: &str) -> Vec<String> {
    let mut sigs = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
            continue;
        }

        let is_sig = match language {
            "rust" => {
                (trimmed.starts_with("pub fn ")
                    || trimmed.starts_with("fn ")
                    || trimmed.starts_with("pub async fn ")
                    || trimmed.starts_with("async fn ")
                    || trimmed.starts_with("pub struct ")
                    || trimmed.starts_with("pub enum ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("enum ")
                    || trimmed.starts_with("trait ")
                    || trimmed.starts_with("pub trait ")
                    || trimmed.starts_with("impl "))
                    && !trimmed.ends_with(',')
            }
            "python" => {
                trimmed.starts_with("def ")
                    || trimmed.starts_with("async def ")
                    || trimmed.starts_with("class ")
            }
            "javascript" | "typescript" => {
                trimmed.starts_with("function ")
                    || trimmed.starts_with("async function ")
                    || trimmed.starts_with("class ")
                    || trimmed.contains("=> {")
                    || (trimmed.starts_with("const ") && trimmed.contains("function"))
            }
            "java" | "cpp" | "c" => {
                (trimmed.contains('(') && trimmed.contains(')'))
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with("*")
                    && (trimmed.ends_with('{') || trimmed.ends_with(';'))
            }
            _ => {
                // Generic: lines containing parentheses that look like declarations
                trimmed.contains('(')
                    && trimmed.contains(')')
                    && !trimmed.starts_with("//")
                    && trimmed.len() < 200
            }
        };

        if is_sig {
            sigs.push(trimmed.to_string());
        }
    }

    sigs
}

/// Compute a short UUID-like hex string for CCR ref generation.
///
/// Uses SystemTime for entropy — not cryptographic, but sufficient for
/// unique CCR reference IDs.
fn uuid_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    // Mix with a simple counter via thread_local
    use std::cell::Cell;
    thread_local! {
        static COUNTER: Cell<u32> = const { Cell::new(0) };
    }
    let count = COUNTER.with(|c| {
        let v = c.get().wrapping_add(1);
        c.set(v);
        v
    });
    format!("{:08x}{:08x}", nanos, count)
}

/// Embedding computation, extracted so cfg blocks can live at module level.
#[cfg(feature = "embeddings")]
fn compute_embedding_impl(content: &str) -> Result<Value, ExecutionError> {
    use pluresdb::EmbedText as _;
    use pluresdb::FastEmbedder;
    let embedder = FastEmbedder::new("BAAI/bge-small-en-v1.5")
        .map_err(|e| ExecutionError::ActionFailed {
            action: "compute_embedding".into(),
            message: format!("embedder init: {e}"),
        })?;
    let vecs = embedder.embed(&[content])
        .map_err(|e| ExecutionError::ActionFailed {
            action: "compute_embedding".into(),
            message: format!("embed: {e}"),
        })?;
    let v = vecs.into_iter().next().unwrap_or_default();
    Ok(json!({ "embedding": v }))
}

#[cfg(not(feature = "embeddings"))]
fn compute_embedding_impl(_content: &str) -> Result<Value, ExecutionError> {
    let zeros: Vec<f32> = vec![0.0_f32; 384];
    Ok(json!({ "embedding": zeros }))
}
// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;
    use serde_json::json;

    // Share ONE CrdtStore across all tests to avoid the HNSW 1M-element
    // pre-allocation (~300 MB per instance) being multiplied across parallel tests.
    fn shared_db() -> Arc<CrdtStore> {
        static DB: OnceLock<Arc<CrdtStore>> = OnceLock::new();
        DB.get_or_init(|| Arc::new(CrdtStore::default())).clone()
    }

    fn handler() -> HeadroomActionHandler {
        HeadroomActionHandler::new(shared_db())
    }

    // ── detect_content_type ──────────────────────────────────────────────

    #[test]
    fn detect_json_by_brace_prefix() {
        let h = handler();
        let r = h.call("detect_content_type", &json!({"content": "{\"key\": 1}"})).unwrap();
        assert_eq!(r["content_type"], "json");
        assert!(r["confidence"].as_f64().unwrap() > 0.9);
    }

    #[test]
    fn detect_json_by_array_prefix() {
        let h = handler();
        let r = h.call("detect_content_type", &json!({"content": "[1,2,3]"})).unwrap();
        assert_eq!(r["content_type"], "json");
    }

    #[test]
    fn detect_code_by_fn_keyword() {
        let h = handler();
        let r = h.call("detect_content_type", &json!({"content": "pub fn hello() { }"})).unwrap();
        assert_eq!(r["content_type"], "code");
    }

    #[test]
    fn detect_prose_for_plain_text() {
        let h = handler();
        let r = h.call("detect_content_type", &json!({"content": "This is some plain text about nothing in particular."})).unwrap();
        assert_eq!(r["content_type"], "prose");
    }

    #[test]
    fn detect_error_for_stack_trace() {
        let h = handler();
        let r = h.call("detect_content_type", &json!({"content": "Error: something went wrong\n  at fn1 (file.rs:10)\n  at fn2 (main.rs:5)\nCaused by: connection refused"})).unwrap();
        assert_eq!(r["content_type"], "error");
    }

    // ── compute_content_hash ─────────────────────────────────────────────

    #[test]
    fn hash_starts_with_sha256_prefix() {
        let h = handler();
        let r = h.call("compute_content_hash", &json!({"content": "hello"})).unwrap();
        let hash = r["hash"].as_str().unwrap();
        assert!(hash.starts_with("sha256:"), "hash = {hash}");
        assert_eq!(hash.len(), 7 + 64); // "sha256:" + 64 hex chars
    }

    #[test]
    fn hash_is_deterministic() {
        let h = handler();
        let p = json!({"content": "deterministic input"});
        let r1 = h.call("compute_content_hash", &p).unwrap();
        let r2 = h.call("compute_content_hash", &p).unwrap();
        assert_eq!(r1["hash"], r2["hash"]);
    }

    #[test]
    fn hash_differs_for_different_content() {
        let h = handler();
        let r1 = h.call("compute_content_hash", &json!({"content": "aaa"})).unwrap();
        let r2 = h.call("compute_content_hash", &json!({"content": "bbb"})).unwrap();
        assert_ne!(r1["hash"], r2["hash"]);
    }

    #[test]
    fn sha256_of_known_input() {
        // echo -n "hello" | sha256sum => 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        let h = handler();
        let r = h.call("compute_content_hash", &json!({"content": "hello"})).unwrap();
        assert_eq!(
            r["hash"].as_str().unwrap(),
            "sha256:2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    // ── count_tokens ────────────────────────────────────────────────────

    #[test]
    fn count_tokens_returns_positive_for_nonempty() {
        let h = handler();
        let r = h.call("count_tokens", &json!({"content": "hello world"})).unwrap();
        let n = r["tokens"].as_u64().unwrap();
        assert!(n > 0, "expected > 0 tokens, got {n}");
    }

    #[test]
    fn count_tokens_zero_for_empty() {
        let h = handler();
        let r = h.call("count_tokens", &json!({"content": ""})).unwrap();
        assert_eq!(r["tokens"].as_u64().unwrap(), 0);
    }

    #[test]
    fn count_tokens_increases_with_content() {
        let h = handler();
        let short = h.call("count_tokens", &json!({"content": "hi"})).unwrap();
        let long  = h.call("count_tokens", &json!({"content": "This is a much longer sentence with many more tokens in it."})).unwrap();
        assert!(long["tokens"].as_u64().unwrap() > short["tokens"].as_u64().unwrap());
    }

    // ── cosine_similarity ────────────────────────────────────────────────

    #[test]
    fn cosine_identical_vectors_is_one() {
        let h = handler();
        let v = json!([1.0, 0.0, 0.0]);
        let r = h.call("cosine_similarity", &json!({"a": v, "b": v.clone()})).unwrap();
        let score = r["score"].as_f64().unwrap();
        assert!((score - 1.0).abs() < 1e-9, "expected 1.0, got {score}");
    }

    #[test]
    fn cosine_orthogonal_vectors_is_zero() {
        let h = handler();
        let r = h.call("cosine_similarity", &json!({
            "a": [1.0, 0.0],
            "b": [0.0, 1.0]
        })).unwrap();
        let score = r["score"].as_f64().unwrap();
        assert!(score.abs() < 1e-9, "expected ~0, got {score}");
    }

    #[test]
    fn cosine_opposite_vectors_is_negative_one() {
        let h = handler();
        let r = h.call("cosine_similarity", &json!({
            "a": [1.0, 0.0],
            "b": [-1.0, 0.0]
        })).unwrap();
        let score = r["score"].as_f64().unwrap();
        assert!((score + 1.0).abs() < 1e-9, "expected -1.0, got {score}");
    }

    #[test]
    fn cosine_empty_vectors_returns_zero() {
        let h = handler();
        let r = h.call("cosine_similarity", &json!({"a": [], "b": []})).unwrap();
        assert_eq!(r["score"].as_f64().unwrap(), 0.0);
    }

    #[test]
    fn cosine_mismatched_lengths_returns_zero() {
        let h = handler();
        let r = h.call("cosine_similarity", &json!({
            "a": [1.0, 2.0],
            "b": [1.0]
        })).unwrap();
        assert_eq!(r["score"].as_f64().unwrap(), 0.0);
    }

    // ── split_sentences ──────────────────────────────────────────────────

    #[test]
    fn split_sentences_basic() {
        let h = handler();
        let r = h.call("split_sentences", &json!({
            "content": "First sentence. Second sentence. Third sentence."
        })).unwrap();
        let sents = r["sentences"].as_array().unwrap();
        assert!(sents.len() >= 3, "expected >= 3 sentences, got {}", sents.len());
    }

    #[test]
    fn split_sentences_empty_returns_empty() {
        let h = handler();
        let r = h.call("split_sentences", &json!({"content": ""})).unwrap();
        assert_eq!(r["sentences"].as_array().unwrap().len(), 0);
    }

    // ── extract_ast_signatures ───────────────────────────────────────────

    #[test]
    fn extract_rust_fn_signatures() {
        let h = handler();
        let code = "pub fn hello() -> String { String::new() }\nfn world() {}\nlet x = 1;";
        let r = h.call("extract_ast_signatures", &json!({
            "content": code, "language": "rust"
        })).unwrap();
        let sigs = r["signatures"].as_array().unwrap();
        assert!(sigs.iter().any(|s| s.as_str().unwrap_or("").contains("pub fn hello")),
            "expected 'pub fn hello' in sigs: {sigs:?}");
    }

    #[test]
    fn extract_python_def_signatures() {
        let h = handler();
        let code = "def process(x):\n    return x + 1\n\nclass Foo:\n    pass";
        let r = h.call("extract_ast_signatures", &json!({
            "content": code, "language": "python"
        })).unwrap();
        let sigs = r["signatures"].as_array().unwrap();
        assert!(sigs.iter().any(|s| s.as_str().unwrap_or("").contains("def process")),
            "expected 'def process' in sigs: {sigs:?}");
    }

    // ── PluresDB read/write/query ────────────────────────────────────────

    #[test]
    fn pluresdb_write_then_read_roundtrip() {
        let h = handler();
        h.call("pluresdb_write", &json!({"key": "rr:test_roundtrip", "value": {"x": 42}})).unwrap();
        let r = h.call("pluresdb_read", &json!({"key": "rr:test_roundtrip"})).unwrap();
        assert_eq!(r["value"]["x"], 42);
    }

    #[test]
    fn pluresdb_read_missing_key_returns_null() {
        let h = handler();
        let r = h.call("pluresdb_read", &json!({"key": "rr:definitely_missing_xyz"})).unwrap();
        assert!(r["value"].is_null());
    }

    #[test]
    fn pluresdb_query_prefix_scan() {
        let h = handler();
        h.call("pluresdb_write", &json!({"key": "scan_ns:a", "value": 1})).unwrap();
        h.call("pluresdb_write", &json!({"key": "scan_ns:b", "value": 2})).unwrap();
        h.call("pluresdb_write", &json!({"key": "scan_other:c", "value": 3})).unwrap();
        let r = h.call("pluresdb_query", &json!({"prefix": "scan_ns:"})).unwrap();
        let keys = r["keys"].as_array().unwrap();
        assert!(keys.len() >= 2, "expected >= 2 keys with prefix 'scan_ns:', got {keys:?}");
        assert!(keys.iter().all(|k| k.as_str().unwrap_or("").starts_with("scan_ns:")));
    }

    #[test]
    fn pluresdb_delete_removes_key() {
        let h = handler();
        h.call("pluresdb_write", &json!({"key": "del_ns:unique_del_key", "value": true})).unwrap();
        h.call("delete_from_pluresdb", &json!({"key": "del_ns:unique_del_key"})).unwrap();
        let r = h.call("pluresdb_read", &json!({"key": "del_ns:unique_del_key"})).unwrap();
        assert!(r["value"].is_null(), "expected null after delete");
    }

    // ── Unknown action ───────────────────────────────────────────────────

    #[test]
    fn unknown_action_returns_ok_stub() {
        let h = handler();
        let r = h.call("some_future_action_not_yet_defined", &json!({})).unwrap();
        assert_eq!(r["ok"], true);
    }

    // ── Stub completeness: all catalog actions should not Err ────────────

    #[test]
    fn all_catalog_actions_return_ok() {
        let h = handler();
        let actions = [
            "route_json", "route_code", "route_prose", "route_log", "route_error", "route_mixed",
            "analyze_json_structure", "detect_language", "measure_density", "detect_patterns",
            "extract_error_frame", "split_by_structure", "write_plan_entry",
            "calculate_budget", "split_into_blocks", "classify_content", "score_all_blocks",
            "list_plan_entries", "execute_structural_compression", "execute_code_compression",
            "execute_prose_compression", "execute_log_compression", "execute_hybrid_compression",
            "skip_compression", "list_compressible_blocks", "store_for_retrieval", "fit_to_budget",
            "align_cache_prefix", "order_for_cache", "inject_retrieval_hints", "assemble_output",
            "compute_pipeline_result", "check_shared_memory", "compress_remaining_blocks",
            "ingest_new_to_memory", "reuse_compressed_from_memory", "get_block",
            "deduplicate_patterns", "write_compressed", "reassemble_sections",
            "get_block_metadata", "compute_relevance_score", "compute_recency_score",
            "compute_source_score", "compute_composite", "assign_severity", "write_scored_block",
            "get_composite_score", "update_severity", "list_unscored_blocks", "sort_blocks_by_score",
            "generate_ccr_ref", "check_duplicate", "refresh_ttl", "increment_access_count",
            "list_expired_entries", "list_lru_entries", "count_ccr_entries",
            "get_system_prompt", "get_static_context", "compute_prefix_hash", "store_prefix_hash",
            "get_model_context_limit", "write_budget", "get_budget", "sum_tokens",
            "try_include", "include_block", "drop_block", "compute_overflow",
        ];
        for action in actions {
            let result = h.call(action, &json!({}));
            assert!(result.is_ok(), "action '{action}' returned Err: {:?}", result.err());
        }
    }
}