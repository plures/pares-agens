//! E2E proof for the headroom compression **seam** (`HeadroomHook`).
//!
//! This is the C-VERIFY-001 gate, ported to the Stage-4 plugin surface.
//!
//! ## Why this drives `HeadroomHook` directly (not `Agent::with_headroom`)
//!
//! In the pre-collapse agens-core fork, headroom was wired into the agent loop
//! via `Agent::with_headroom(hook)`, and this test drove `Agent::handle_event`.
//! After the Stage-4 collapse, headroom is **agens-brought IP** that lives in
//! this plugin (`agens_plugin::headroom`), and radix's `Agent` deliberately has
//! no headroom hook point — radix never depends on agens, and headroom is not a
//! host-presented capability. The plugin applies headroom by calling
//! [`HeadroomHook::compress_messages`] on the transient message list **before**
//! handing it to radix's model path. This test exercises exactly that seam,
//! which is the surface `AgensProvider` uses in production.
//!
//! It asserts the contract that matters:
//!
//! 1. With an enabled hook and an over-threshold payload, the compressed list
//!    has **strictly fewer tokens** than the original (real reduction, not a
//!    `success:true` stub), while message **count**, **roles**, and **tool
//!    metadata** are preserved.
//! 2. Compression is **transient**: the input slice is never mutated — the
//!    caller's canonical `Vec` is untouched (proven by re-counting it after).
//! 3. A **disabled** hook, a **below-threshold** payload all pass through
//!    verbatim (the model would see the originals).

use agens_plugin::headroom::{
    count_message_tokens, in_memory_hook, in_memory_hook_disabled, HeadroomHook,
};
use pares_agens_core::model::ChatMessage;

/// A multi-thousand-token user message built from large prose + a big code
/// block, guaranteed to blow past the 500-token (chars/4) gate.
fn over_threshold_user_content() -> String {
    let mut s = String::new();
    // ~120 distinct, long sentences of prose.
    for i in 0..120 {
        s.push_str(&format!(
            "This is sentence number {i} in a deliberately verbose paragraph that exists \
             purely to inflate the token count well beyond the compression threshold so the \
             headroom hook is forced to engage its extractive prose trimming heuristics. "
        ));
    }
    s.push('\n');
    // A large code block with many real signatures + bodies to elide.
    s.push_str("```rust\n");
    for i in 0..40 {
        s.push_str(&format!(
            "pub fn handler_{i}(input: &str, count: usize) -> Result<String, String> {{\n    \
             let mut acc = String::new();\n    for _ in 0..count {{ acc.push_str(input); }}\n    \
             if acc.is_empty() {{ return Err(\"empty\".into()); }}\n    Ok(acc)\n}}\n\n"
        ));
    }
    s.push_str("```\n");
    s
}

const TEST_SYSTEM_PROMPT: &str = "You are a test agent.";

/// The uncompressed message list a first turn (empty history) would build:
/// `[system, user(content)]`.
fn baseline_messages(system: &str, content: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::system(system), ChatMessage::user(content)]
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. ENABLED HOOK + OVER-THRESHOLD → real reduction at the seam
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn seam_compresses_over_threshold_payload() {
    let hook = in_memory_hook(500);

    let content = over_threshold_user_content();
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, &content);
    let baseline_tokens = count_message_tokens(&baseline);
    assert!(
        baseline_tokens > 500,
        "test fixture must exceed the 500-token gate; got {baseline_tokens}"
    );

    let compressed = hook.compress_messages("req-1", &baseline).await;

    // (b) message COUNT preserved.
    assert_eq!(
        compressed.len(),
        baseline.len(),
        "compression must not add/drop messages"
    );
    // (c) roles preserved, positionally.
    for (got, want) in compressed.iter().zip(baseline.iter()) {
        assert_eq!(got.role, want.role, "role must be preserved");
    }
    assert_eq!(compressed[0].role, "system");
    assert_eq!(compressed[1].role, "user");

    // (a) REAL reduction: strictly fewer tokens than the uncompressed payload.
    let compressed_tokens = count_message_tokens(&compressed);
    assert!(
        compressed_tokens < baseline_tokens,
        "expected strict token reduction at the headroom seam: \
         compressed {compressed_tokens} tok is not < baseline {baseline_tokens} tok"
    );

    eprintln!(
        "SEAM-E2E reduction: baseline {baseline_tokens} tok -> compressed {compressed_tokens} tok \
         (saved {} tok, {:.1}%)",
        baseline_tokens - compressed_tokens,
        100.0 * (baseline_tokens - compressed_tokens) as f64 / baseline_tokens as f64
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Compression is TRANSIENT — the caller's input slice is never mutated.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn seam_does_not_mutate_caller_input() {
    let hook = in_memory_hook(500);

    let content = over_threshold_user_content();
    let canonical = baseline_messages(TEST_SYSTEM_PROMPT, &content);
    let canonical_tokens_before = count_message_tokens(&canonical);

    let compressed = hook.compress_messages("req-hist", &canonical).await;

    // The returned clone was genuinely compressed...
    assert!(
        count_message_tokens(&compressed) < canonical_tokens_before,
        "sanity: the returned payload should have been compressed"
    );

    // ...but the caller's canonical Vec is byte-for-byte untouched.
    assert_eq!(
        count_message_tokens(&canonical),
        canonical_tokens_before,
        "compression must be transient: canonical input token count changed"
    );
    let middle_marker = "This is sentence number 60 in a deliberately verbose paragraph";
    assert!(
        canonical[1].content.contains(middle_marker),
        "canonical user message must retain the FULL uncompressed content"
    );
    assert_eq!(
        canonical[1].content, content,
        "canonical user content must be verbatim, not the compressed clone"
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3a. DISABLED hook → exact pass-through.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn seam_disabled_hook_is_passthrough() {
    let hook = in_memory_hook_disabled();

    let content = over_threshold_user_content();
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, &content);
    let baseline_tokens = count_message_tokens(&baseline);

    let out = hook.compress_messages("req-disabled", &baseline).await;
    assert_eq!(out.len(), baseline.len());
    assert_eq!(
        count_message_tokens(&out),
        baseline_tokens,
        "disabled hook must be an exact pass-through"
    );
    assert_eq!(out[1].content, content, "user content must be verbatim");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3b. BELOW-THRESHOLD payload with an enabled hook → exact pass-through.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn seam_below_threshold_is_passthrough() {
    let hook: HeadroomHook = in_memory_hook(500);

    // Tiny content → [system, user] aggregate is well under 500 tokens.
    let content = "hello there";
    let baseline = baseline_messages(TEST_SYSTEM_PROMPT, content);
    assert!(
        count_message_tokens(&baseline) <= 500,
        "below-threshold fixture must stay under the gate"
    );

    let out = hook.compress_messages("req-small", &baseline).await;
    assert_eq!(
        count_message_tokens(&out),
        count_message_tokens(&baseline),
        "below-threshold payload must pass through untouched"
    );
    assert_eq!(out[1].content, content);
}

// ═══════════════════════════════════════════════════════════════════════════
// 3c. Tool metadata (role + tool_call_id) is preserved through compression.
// ═══════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn seam_preserves_tool_metadata() {
    let hook = in_memory_hook(10);

    let mut tool_msg = ChatMessage {
        role: "tool".into(),
        content: "sentence one. sentence two. ".repeat(100),
        tool_call_id: Some("call_abc".into()),
        tool_calls: None,
    };
    tool_msg.content.push_str("trailing.");
    let messages = vec![tool_msg];

    let out = hook.compress_messages("req-tool", &messages).await;
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].role, "tool");
    assert_eq!(out[0].tool_call_id.as_deref(), Some("call_abc"));
}
